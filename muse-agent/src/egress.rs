use crate::config::EgressConfig;
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tracing::{debug, info};

pub struct EgressConnector {
    config: EgressConfig,
}

impl EgressConnector {
    pub fn new(config: EgressConfig) -> Self {
        Self { config }
    }

    /// Establishes a TCP stream to the destination (target_host:target_port).
    /// If egress proxy is enabled, it connects via HTTP CONNECT tunnel natively.
    pub async fn connect(&self, target_host: &str, target_port: u16) -> io::Result<TcpStream> {
        if !self.config.enabled {
            info!("Direct TCP connection to {}:{}", target_host, target_port);
            return TcpStream::connect((target_host, target_port)).await;
        }

        let proxy_addr = format!("{}:{}", self.config.host, self.config.port);
        info!(
            "Connecting to {}:{} via egress proxy at {}",
            target_host, target_port, proxy_addr
        );

        let mut stream = TcpStream::connect(&proxy_addr).await.map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("Failed to connect to egress proxy {}: {}", proxy_addr, e),
            )
        })?;

        // Construct HTTP CONNECT request
        let mut connect_req = format!(
            "CONNECT {target_host}:{target_port} HTTP/1.1\r\n\
             Host: {target_host}:{target_port}\r\n\
             User-Agent: muse-agent/0.1\r\n"
        );

        if let Some(auth) = &self.config.auth {
            use base64::Engine;
            let encoded = base64::engine::general_purpose::STANDARD.encode(auth.as_bytes());
            connect_req.push_str(&format!("Proxy-Authorization: Basic {encoded}\r\n"));
        }

        connect_req.push_str("\r\n");

        stream.write_all(connect_req.as_bytes()).await?;
        stream.flush().await?;

        // Read HTTP response headers until \r\n\r\n
        let mut header_buf = Vec::new();
        let mut byte_buf = [0u8; 1];

        while header_buf.len() < 4096 {
            let n = stream.read(&mut byte_buf).await?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Egress proxy closed connection prematurely",
                ));
            }
            header_buf.push(byte_buf[0]);

            if header_buf.ends_with(b"\r\n\r\n") {
                break;
            }
        }

        let response_str = String::from_utf8_lossy(&header_buf);
        debug!("Egress proxy handshake response:\n{}", response_str);

        // Check if proxy returned 200 Connection Established
        let first_line = response_str.lines().next().unwrap_or("");
        if !first_line.contains("200") {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionRefused,
                format!("Egress proxy handshake rejected: {}", first_line),
            ));
        }

        info!(
            "Egress tunnel established successfully to {}:{}",
            target_host, target_port
        );
        Ok(stream)
    }
}
