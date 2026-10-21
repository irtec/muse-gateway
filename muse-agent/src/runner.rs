use bytes::Bytes;
use muse_core::{read_be_frame, write_be_frame, TunnelMessage, DEFAULT_MAX_FRAME_SIZE};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::split;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio::time::interval;
use tracing::{error, info, warn};

use crate::config::AgentConfig;
use crate::uds::UdsClient;

pub async fn run_agent_loop(
    stream: TcpStream,
    config: &AgentConfig,
    uds_client: Arc<UdsClient>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (mut reader, mut writer) = split(stream);
    let (tx_outbox, mut rx_outbox) = mpsc::channel::<TunnelMessage>(64);

    // Initial Registration
    let register_msg = TunnelMessage::Register {
        vm_id: config.vm.id.clone(),
        token: config.vm.token.clone(),
        is_fresh_boot: true,
    };
    let initial_frame = register_msg.encode()?;
    write_be_frame(&mut writer, &initial_frame[4..]).await?;
    info!("Sent registration to hub for VM ID: {}", config.vm.id);

    // Background task: write outgoing frames to TCP stream
    let write_task = tokio::spawn(async move {
        while let Some(msg) = rx_outbox.recv().await {
            match msg.encode() {
                Ok(encoded) => {
                    if let Err(e) = write_be_frame(&mut writer, &encoded[4..]).await {
                        error!("Failed to write frame to Hub: {}", e);
                        break;
                    }
                }
                Err(e) => {
                    error!("Failed to encode tunnel message: {}", e);
                }
            }
        }
    });

    // Heartbeat ticker task
    let tx_heartbeat = tx_outbox.clone();
    let vm_id = config.vm.id.clone();
    let heartbeat_task = tokio::spawn(async move {
        let mut interval_timer = interval(Duration::from_secs(10));
        loop {
            interval_timer.tick().await;
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis() as u64;

            let ping = TunnelMessage::HeartbeatPing {
                vm_id: vm_id.clone(),
                timestamp_ms: now_ms,
            };

            if tx_heartbeat.send(ping).await.is_err() {
                break;
            }
        }
    });

    // Inbound frame reader loop
    loop {
        let frame = match read_be_frame(&mut reader, DEFAULT_MAX_FRAME_SIZE).await {
            Ok(f) => f,
            Err(e) => {
                warn!("Hub stream closed or error: {}", e);
                break;
            }
        };

        // Construct 4-byte BE length prefix to decode with TunnelMessage::decode
        let mut full_buf = bytes::BytesMut::with_capacity(4 + frame.len());
        full_buf.extend_from_slice(&(frame.len() as u32).to_be_bytes());
        full_buf.extend_from_slice(&frame);

        match TunnelMessage::decode(Bytes::from(full_buf)) {
            Ok(msg) => match msg {
                TunnelMessage::RegisterAck { success, message } => {
                    if success {
                        info!("Hub registration confirmed: {}", message);
                    } else {
                        error!("Hub rejected registration: {}", message);
                        break;
                    }
                }
                TunnelMessage::HeartbeatPong { .. } => {
                    // Pong received, connection is healthy
                }
                TunnelMessage::InferenceRequest {
                    request_id,
                    path: _,
                    headers: _,
                    body,
                } => {
                    let uds = uds_client.clone();
                    let tx_reply = tx_outbox.clone();
                    tokio::spawn(async move {
                        info!(
                            "Processing inference request {} ({} bytes)",
                            request_id,
                            body.len()
                        );
                        match uds.send_request(&body).await {
                            Ok(response_body) => {
                                let resp = TunnelMessage::InferenceResponse {
                                    request_id,
                                    status_code: 200,
                                    headers: vec![(
                                        "content-type".to_string(),
                                        "application/json".to_string(),
                                    )],
                                    body: response_body,
                                    is_last: true,
                                };
                                let _ = tx_reply.send(resp).await;
                            }
                            Err(e) => {
                                error!("Inference request {} failed: {}", request_id, e);
                                let err_msg = TunnelMessage::InferenceError {
                                    request_id,
                                    error: format!("UDS Inference error: {}", e),
                                };
                                let _ = tx_reply.send(err_msg).await;
                            }
                        }
                    });
                }
                _ => {}
            },
            Err(e) => {
                error!("Failed to decode incoming tunnel frame: {}", e);
            }
        }
    }

    write_task.abort();
    heartbeat_task.abort();
    Ok(())
}
