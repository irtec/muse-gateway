mod config;
mod lb;
mod pool;
mod telegram;

use config::HubConfig;
use lb::{create_file_server_router, create_router, AppState};
use pool::WorkerPool;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use telegram::TelegramNotifier;
use tokio::time::interval;
use tracing::{info, warn};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    info!("Starting muse-hub (Central HTTP Gateway & Load Balancer)");

    let config_path =
        env::var("MUSE_HUB_CONFIG").unwrap_or_else(|_| "/etc/muse-hub/config.toml".to_string());

    let config = if PathBuf::from(&config_path).exists() {
        info!("Loading configuration from {}", config_path);
        HubConfig::load_from_file(&config_path)?
    } else {
        warn!(
            "Configuration file {} not found, using default configuration",
            config_path
        );
        HubConfig::default_example()
    };

    let config = Arc::new(config);

    // Initialize Telegram Notifier
    let telegram = if let Some(tg_cfg) = &config.telegram {
        TelegramNotifier::new(
            tg_cfg.bot_token.clone(),
            tg_cfg.chat_id.clone(),
            tg_cfg.enabled,
        )
    } else {
        TelegramNotifier::disabled()
    };

    let pool = Arc::new(WorkerPool::new(config.pool.resolved_workers(), telegram));

    // Periodic Health Check Loop for all target workers
    let pool_checker = pool.clone();
    let check_interval = Duration::from_secs(config.pool.health_check_interval_secs);
    tokio::spawn(async move {
        let mut ticker = interval(check_interval);
        loop {
            ticker.tick().await;
            pool_checker.run_health_checks().await;
        }
    });

    // HTTP Client with configured request timeout for forwarding
    let http_client = reqwest::Client::builder()
        .timeout(Duration::from_secs(config.pool.request_timeout_secs))
        .pool_idle_timeout(Duration::from_secs(90))
        .build()?;

    // App state shared between API router and File Server router
    let app_state = AppState {
        config: config.clone(),
        pool: pool.clone(),
        http_client,
    };

    let api_app = create_router(app_state.clone());
    let fs_app = create_file_server_router(app_state);

    let http_addr = format!("{}:{}", config.server.host, config.server.http_port);
    let api_listener = tokio::net::TcpListener::bind(&http_addr).await?;
    info!("muse-hub HTTP API Server listening on {}", http_addr);

    let fs_addr = format!("{}:{}", config.server.host, config.server.file_server_port);
    let fs_listener = tokio::net::TcpListener::bind(&fs_addr).await?;
    info!(
        "muse-hub File Server Failover Router listening on {}",
        fs_addr
    );

    tokio::try_join!(
        async {
            axum::serve(api_listener, api_app)
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        },
        async {
            axum::serve(fs_listener, fs_app)
                .await
                .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)
        }
    )?;

    Ok(())
}
