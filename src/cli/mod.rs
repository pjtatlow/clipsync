pub mod config;
pub mod copy;
pub mod devices;
pub mod install;
pub mod invite;
pub mod paste;
pub mod setup;
pub mod status;
pub mod xclip;

use anyhow::{Context, Result};
use bytes::BytesMut;
use futures::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

use crate::config::socket_path;
use crate::protocol::{Request, Response, MAX_IPC_FRAME_SIZE};

pub async fn send_request(request: Request) -> Result<Response> {
    let path = socket_path();

    if !path.exists() {
        anyhow::bail!(
            "Daemon not running. Start with `clipsync daemon` or `clipsync install`."
        );
    }

    let stream = UnixStream::connect(&path)
        .await
        .with_context(|| format!("Failed to connect to daemon at {}", path.display()))?;

    let codec = LengthDelimitedCodec::builder()
        .max_frame_length(MAX_IPC_FRAME_SIZE)
        .new_codec();
    let mut framed = Framed::new(stream, codec);

    let request_bytes = serde_json::to_vec(&request)?;
    framed
        .send(BytesMut::from(&request_bytes[..]).freeze())
        .await?;

    let response_bytes = framed
        .next()
        .await
        .ok_or_else(|| anyhow::anyhow!("Connection closed before response"))??;

    let response: Response = serde_json::from_slice(&response_bytes)?;
    Ok(response)
}
