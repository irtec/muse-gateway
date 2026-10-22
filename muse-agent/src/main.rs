use muse_agent::{create_agent_router, AgentConfig, AppState, ProcessSupervisor, UdsClient};
use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::net::TcpListener;
use tracing::{info, warn};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let log_dir =
        env::var("MUSE_LOG_DIR").unwrap_or_else(|_| "/home/hatch/workspace/agent/logs".to_string());
    let _ = std::fs::create_dir_all(&log_dir);

    let file_appender = tracing_appender::rolling::daily(&log_dir, "muse-agent.log");
    let (non_blocking, _guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = tracing_subscriber::EnvFilter::from_default_env()
        .add_directive(tracing::Level::INFO.into());

    tracing_subscriber::registry()
        .with(env_filter)
        .with(tracing_subscriber::fmt::layer().with_ansi(true))
        .with(
            tracing_subscriber::fmt::layer()
                .with_ansi(false)
                .with_writer(non_blocking),
        )
        .init();

    info!("Starting muse-agent (Unified HTTP Bridge & Service Supervisor)");

    let mut config_path = env::var("MUSE_AGENT_CONFIG")
        .unwrap_or_else(|_| "/home/hatch/workspace/agent/config/muse-agent.toml".to_string());

    let args: Vec<String> = env::args().collect();
    let mut i = 1;
    while i < args.len() {
        if (args[i] == "--config" || args[i] == "-c") && i + 1 < args.len() {
            config_path = args[i + 1].clone();
            i += 2;
        } else if !args[i].starts_with('-') {
            config_path = args[i].clone();
            i += 1;
        } else {
            i += 1;
        }
    }

    let config = if PathBuf::from(&config_path).exists() {
        info!("Loading config from {}", config_path);
        AgentConfig::load_from_file(&config_path)?
    } else {
        warn!(
            "Config file {} not found, using default configuration",
            config_path
        );
        AgentConfig::default_example()
    };

    // 1. Run Autonomous Supervisor in background Tokio task
    let supervisor = Arc::new(ProcessSupervisor::new(
        config.supervisor.clone(),
        config.vm.id.clone(),
    ));
    let supervisor_clone = supervisor.clone();
    tokio::spawn(async move {
        supervisor_clone.run_loop().await;
    });

    // 2. Initialize UDS Clients for space-inference & space-media
    let uds_client = Arc::new(UdsClient::new(
        &config.inference.socket_path,
        config.inference.max_frame_size_mb,
    ));

    let media_client = Arc::new(UdsClient::new(
        &config.inference.media_socket_path,
        config.inference.max_frame_size_mb,
    ));

    let state = AppState {
        config: config.clone(),
        uds_client,
        media_client,
        supervisor: supervisor.clone(),
    };

    let app = create_agent_router(state);
    let addr: SocketAddr = format!("{}:{}", config.server.host, config.server.port).parse()?;

    // 3. Bind the primary HTTP listener FIRST: a failure here means another
    // process already owns this port, so we abort before touching anything else.
    info!("Binding HTTP server to {}", addr);
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            tracing::error!(
                "Address already in use on {}: another muse-agent is likely running. Aborting to avoid a double-start race.",
                addr
            );
            return Err(e.into());
        }
        Err(e) => return Err(e.into()),
    };
    info!("muse-agent HTTP listening on http://{}", addr);

    // 4. Start File Manager if enabled in configuration
    if config.file_manager.enabled {
        let mut fm_config = config.file_manager.clone();
        if fm_config.auth_token.is_none() {
            fm_config.auth_token = Some(config.vm.token.clone());
        }
        tokio::spawn(async move {
            let fm_addr: SocketAddr = match format!("{}:{}", fm_config.host, fm_config.port).parse()
            {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!("Invalid file_manager address: {}", e);
                    return;
                }
            };
            info!("Starting native file-manager server on http://{}", fm_addr);
            let fm_app = muse_agent::create_file_manager_router(fm_config);
            match TcpListener::bind(fm_addr).await {
                Ok(listener) => {
                    info!("muse-agent file-manager listening on http://{}", fm_addr);
                    if let Err(e) = axum::serve(listener, fm_app).await {
                        tracing::error!("File manager server error: {}", e);
                    }
                }
                Err(e) => {
                    tracing::error!(
                        "Failed to bind file-manager to {}: {}. Continuing without it (primary HTTP bridge unaffected).",
                        fm_addr, e
                    );
                }
            }
        });
    }

    // 5. Start One-Way Native Sync if role == Replica
    if config.sync.enabled && config.sync.role == muse_agent::SyncRole::Replica {
        let auth_token = config
            .sync
            .auth_token
            .clone()
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| config.vm.token.clone());

        let sync_client = Arc::new(muse_agent::SyncClient::new(
            config.sync.clone(),
            auth_token,
            config.egress.clone(),
        ));
        tokio::spawn(async move {
            sync_client.run_loop().await;
        });
    }

    axum::serve(listener, app).await?;

    Ok(())
}
