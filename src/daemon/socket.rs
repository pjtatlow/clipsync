use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use bytes::BytesMut;
use futures::SinkExt;
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot, Semaphore};
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::{debug, error, info, warn};

use crate::config::socket_path;
use crate::protocol::{Request, Response, MAX_IPC_FRAME_SIZE};

use futures::StreamExt;

const MAX_CONCURRENT_CONNECTIONS: usize = 16;
const CONNECTION_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

pub struct SocketRequest {
    pub request: Request,
    pub reply: oneshot::Sender<Response>,
}

pub async fn run_socket_server(request_tx: mpsc::Sender<SocketRequest>) -> Result<()> {
    let path = socket_path();

    // Ensure parent directory exists
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Atomic stale socket detection: try to bind first, handle AddrInUse
    let listener = match UnixListener::bind(&path) {
        Ok(l) => {
            // Set permissions immediately after bind
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            }
            l
        }
        Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => {
            // Check if another daemon is actually running
            match tokio::net::UnixStream::connect(&path).await {
                Ok(_) => {
                    anyhow::bail!(
                        "Another daemon is already running (socket {} is active)",
                        path.display()
                    );
                }
                Err(_) => {
                    // Verify the file is actually a socket before removing it
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::FileTypeExt;
                        let metadata = std::fs::symlink_metadata(&path)?;
                        if !metadata.file_type().is_socket() {
                            anyhow::bail!(
                                "Refusing to remove {}: not a socket (may be a symlink or regular file)",
                                path.display()
                            );
                        }
                    }
                    info!("Removing stale socket at {}", path.display());
                    std::fs::remove_file(&path)?;
                    let l = UnixListener::bind(&path)?;
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
                    }
                    l
                }
            }
        }
        Err(e) => return Err(e.into()),
    };

    info!("Socket server listening at {}", path.display());

    let semaphore = Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));

    loop {
        let (stream, _) = listener.accept().await?;
        let request_tx = request_tx.clone();
        let semaphore = semaphore.clone();

        tokio::spawn(async move {
            let _permit = match semaphore.acquire().await {
                Ok(permit) => permit,
                Err(_) => {
                    warn!("Connection semaphore closed");
                    return;
                }
            };

            // Verify peer UID matches our UID
            #[cfg(unix)]
            {
                match stream.peer_cred() {
                    Ok(cred) => {
                        let our_uid = nix::unistd::getuid().as_raw();
                        if cred.uid() != our_uid {
                            warn!(
                                "Rejected connection from different UID (peer={}, ours={})",
                                cred.uid(),
                                our_uid
                            );
                            return;
                        }
                    }
                    Err(e) => {
                        warn!("Failed to get peer credentials: {}", e);
                        return;
                    }
                }
            }

            let codec = LengthDelimitedCodec::builder()
                .max_frame_length(MAX_IPC_FRAME_SIZE)
                .new_codec();
            let mut framed = Framed::new(stream, codec);

            loop {
                let result = match tokio::time::timeout(CONNECTION_IDLE_TIMEOUT, framed.next()).await
                {
                    Ok(Some(result)) => result,
                    Ok(None) => break,
                    Err(_) => {
                        debug!("Connection idle timeout reached");
                        break;
                    }
                };

                match result {
                    Ok(data) => {
                        let request: Request = match serde_json::from_slice(&data) {
                            Ok(req) => req,
                            Err(e) => {
                                warn!("Invalid request: {}", e);
                                let resp = Response::Error {
                                    message: format!("Invalid request: {}", e),
                                };
                                let resp_bytes = match serde_json::to_vec(&resp) {
                                    Ok(b) => b,
                                    Err(e) => {
                                        error!("Failed to serialize response: {}", e);
                                        break;
                                    }
                                };
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
                                let resp_bytes = match serde_json::to_vec(&response) {
                                    Ok(b) => b,
                                    Err(e) => {
                                        error!("Failed to serialize response: {}", e);
                                        break;
                                    }
                                };
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
