use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};
use std::io;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const DEFAULT_MAX_FRAME_SIZE: usize = 32 * 1024 * 1024; // 32MB max frame to support ~465K tokens

#[derive(Error, Debug)]
pub enum ProtocolError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("Frame size exceeds limit: {0} > {1}")]
    FrameTooLarge(usize, usize),
    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Unexpected EOF while reading frame")]
    UnexpectedEof,
}

/// Reads a 4-byte big-endian length-prefixed frame.
pub async fn read_be_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_size: usize,
) -> Result<Vec<u8>, ProtocolError> {
    let mut len_bytes = [0u8; 4];
    match reader.read_exact(&mut len_bytes).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
            return Err(ProtocolError::UnexpectedEof);
        }
        Err(e) => return Err(ProtocolError::Io(e)),
    }

    let length = u32::from_be_bytes(len_bytes) as usize;
    if length > max_size {
        return Err(ProtocolError::FrameTooLarge(length, max_size));
    }

    let mut buf = vec![0u8; length];
    reader.read_exact(&mut buf).await?;
    Ok(buf)
}

/// Writes a 4-byte big-endian length-prefixed frame.
pub async fn write_be_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    payload: &[u8],
) -> Result<(), ProtocolError> {
    let len = payload.len() as u32;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Tunnel message types between muse-agent (Spoke) and muse-hub (Edge Gateway).
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(tag = "type", content = "payload")]
pub enum TunnelMessage {
    /// Agent registers itself to Hub
    Register {
        vm_id: String,
        token: String,
        is_fresh_boot: bool,
    },
    /// Agent registration response
    RegisterAck {
        success: bool,
        message: String,
    },
    /// Periodic heartbeat ping/pong
    HeartbeatPing {
        vm_id: String,
        timestamp_ms: u64,
    },
    HeartbeatPong {
        timestamp_ms: u64,
    },
    /// AI Inference request from Hub to Agent
    InferenceRequest {
        request_id: String,
        path: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    },
    /// AI Inference response from Agent to Hub (single delta / batch)
    InferenceResponse {
        request_id: String,
        status_code: u16,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        is_last: bool,
    },
    /// Inference error (e.g. UDS socket down / timeout)
    InferenceError {
        request_id: String,
        error: String,
    },
}

impl TunnelMessage {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let json_bytes = serde_json::to_vec(self)?;
        let mut buf = BytesMut::with_capacity(4 + json_bytes.len());
        buf.put_u32(json_bytes.len() as u32);
        buf.put_slice(&json_bytes);
        Ok(buf.to_vec())
    }

    pub fn decode(mut bytes: Bytes) -> Result<Self, ProtocolError> {
        if bytes.remaining() < 4 {
            return Err(ProtocolError::UnexpectedEof);
        }
        let length = bytes.get_u32() as usize;
        if bytes.remaining() < length {
            return Err(ProtocolError::UnexpectedEof);
        }
        let slice = &bytes.chunk()[..length];
        let msg = serde_json::from_slice(slice)?;
        Ok(msg)
    }
}
