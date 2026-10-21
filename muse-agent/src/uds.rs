use muse_core::{read_be_frame, write_be_frame, ProtocolError, DEFAULT_MAX_FRAME_SIZE};
use std::path::PathBuf;
use tokio::net::UnixStream;
use tracing::{debug, error};

pub struct UdsClient {
    socket_path: PathBuf,
    max_frame_size: usize,
}

impl UdsClient {
    pub fn new(socket_path: impl Into<PathBuf>, max_frame_size_mb: Option<usize>) -> Self {
        let max_size = max_frame_size_mb
            .map(|mb| mb * 1024 * 1024)
            .unwrap_or(DEFAULT_MAX_FRAME_SIZE);

        Self {
            socket_path: socket_path.into(),
            max_frame_size: max_size,
        }
    }

    /// Sends a length-prefixed request to the local Unix domain socket
    /// and waits for the length-prefixed response.
    pub async fn send_request(&self, request_payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
        debug!(
            "Connecting to local inference UDS at {}",
            self.socket_path.display()
        );

        let mut stream = UnixStream::connect(&self.socket_path).await.map_err(|e| {
            error!(
                "Failed to connect to UDS {}: {}",
                self.socket_path.display(),
                e
            );
            ProtocolError::Io(e)
        })?;

        debug!(
            "Writing {} bytes to space-inference socket",
            request_payload.len()
        );
        write_be_frame(&mut stream, request_payload).await?;

        debug!("Waiting for response from space-inference socket");
        let response_payload = read_be_frame(&mut stream, self.max_frame_size).await?;
        debug!(
            "Received {} bytes response from space-inference socket",
            response_payload.len()
        );

        Ok(response_payload)
    }
}
