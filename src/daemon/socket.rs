use anyhow::Result;
use bytes::BytesMut;
use futures::SinkExt;
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::{debug, info, warn};

use crate::config::socket_path;
use crate::protocol::{Request, Response};

use futures::StreamExt;

pub struct SocketRequest {
    pub request: Request,
    pub reply: oneshot::Sender<Response>,
}

pub async fn run_socket_server(request_tx: mpsc::Sender<SocketRequest>) -> Result<()> {
    let path = socket_path();

    // Clean up stale socket
    if path.exists() {
        match tokio::net::UnixStream::connect(&path).await {
            Ok(_) => {
                anyhow::bail!(
                    "Another daemon is already running (socket {} is active)",
                    path.display()
                );
            }
            Err(_) => {
                // Stale socket, remove it
                info!("Removing stale socket at {}", path.display());
                std::fs::remove_file(&path)?;
            }
        }
    }

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let listener = UnixListener::bind(&path)?;
    info!("Socket server listening at {}", path.display());

    loop {
        let (stream, _) = listener.accept().await?;
        let request_tx = request_tx.clone();

        tokio::spawn(async move {
            let mut framed = Framed::new(stream, LengthDelimitedCodec::new());

            while let Some(result) = framed.next().await {
                match result {
                    Ok(data) => {
                        let request: Request = match serde_json::from_slice(&data) {
                            Ok(req) => req,
                            Err(e) => {
                                warn!("Invalid request: {}", e);
                                let resp = Response::Error {
                                    message: format!("Invalid request: {}", e),
                                };
                                let resp_bytes = serde_json::to_vec(&resp).unwrap();
                                let _ = framed.send(BytesMut::from(&resp_bytes[..]).freeze()).await;
                                continue;
                            }
                        };

                        debug!("Received request: {:?}", request);

                        let (reply_tx, reply_rx) = oneshot::channel();
                        if request_tx
                            .send(SocketRequest {
                                request,
                                reply: reply_tx,
                            })
                            .await
                            .is_err()
                        {
                            break;
                        }

                        match reply_rx.await {
                            Ok(response) => {
                                let resp_bytes = serde_json::to_vec(&response).unwrap();
                                if framed
                                    .send(BytesMut::from(&resp_bytes[..]).freeze())
                                    .await
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    Err(e) => {
                        debug!("Socket read error: {}", e);
                        break;
                    }
                }
            }
        });
    }
}
