pub mod clipboard;
pub mod socket;
pub mod spacetime;

use anyhow::Result;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};

use crate::config::{self, Config};
use crate::crypto;
use crate::module_bindings::ClipContentType;
use crate::payload::ClipboardPayload;
use crate::protocol::{DeviceInfo, Request, Response};

use self::clipboard::{ClipboardCommand, ClipboardEvent};
use self::socket::SocketRequest;
use self::spacetime::{SpacetimeCommand, SpacetimeEvent};

pub async fn run_daemon(config: Config) -> Result<()> {
    let device_id = config::load_device_id()?
        .ok_or_else(|| anyhow::anyhow!("Device not set up. Run `clipsync setup` first."))?;
    let token = config::load_token()?;
    let user_id = config::load_user_id()?
        .ok_or_else(|| anyhow::anyhow!("Not logged in. Run `clipsync setup` first."))?;

    info!("Starting daemon with device_id={}, user_id={}", device_id, user_id);

    // Channels for SpacetimeDB
    let (stdb_event_tx, mut stdb_event_rx) = mpsc::channel::<SpacetimeEvent>(32);
    let (stdb_cmd_tx, stdb_cmd_rx) = crossbeam_channel::unbounded::<SpacetimeCommand>();

    // Channels for clipboard
    let (clip_event_tx, mut clip_event_rx) = mpsc::channel::<ClipboardEvent>(32);
    let (clip_cmd_tx, clip_cmd_rx) = std::sync::mpsc::channel::<ClipboardCommand>();

    // Channel for socket requests
    let (socket_req_tx, mut socket_req_rx) = mpsc::channel::<SocketRequest>(32);

    // Spawn SpacetimeDB connection thread
    spacetime::spawn_spacetime_thread(&config, token, user_id, stdb_event_tx, stdb_cmd_rx)?;

    // Spawn clipboard watcher thread
    clipboard::spawn_clipboard_watcher(config.poll_interval_ms, clip_event_tx, clip_cmd_rx)?;

    // Spawn socket server
    let mut socket_handle = tokio::spawn(socket::run_socket_server(socket_req_tx));

    // State
    let mut connected = false;
    let watching = config.watch_clipboard;

    // Load encryption identity
    let age_identity = match crypto::load_private_key() {
        Ok(id) => Some(id),
        Err(e) => {
            warn!("Failed to load private key: {}", e);
            None
        }
    };

    info!("Daemon main loop started (watching={})", watching);

    loop {
        tokio::select! {
            // SpacetimeDB events
            Some(event) = stdb_event_rx.recv() => {
                match event {
                    SpacetimeEvent::Connected { identity: id, token: tok } => {
                        info!("Connected as {}", id.to_hex());
                        connected = true;

                        // Save the token
                        if let Err(e) = config::save_token(&tok) {
                            warn!("Failed to save token: {}", e);
                        }

                        // Register our device
                        let _ = stdb_cmd_tx.send(SpacetimeCommand::RegisterDevice {
                            device_id: device_id.clone(),
                            device_name: hostname(),
                        });
                    }
                    SpacetimeEvent::Disconnected => {
                        warn!("Disconnected from SpacetimeDB");
                        connected = false;
                    }
                    SpacetimeEvent::SubscriptionApplied => {
                        info!("Subscription applied, ready to sync");
                    }
                    SpacetimeEvent::ClipUpdated(clip) => {
                        // Ignore our own syncs from this device
                        if clip.sender_device_id == device_id {
                            continue;
                        }

                        info!("Received clip update from device {}", clip.sender_device_id);

                        if let Some(age_id) = &age_identity {
                            match crypto::decrypt(&clip.encrypted_data, age_id) {
                                Ok(plaintext) => {
                                    match ClipboardPayload::deserialize(&plaintext) {
                                        Ok(payload) => {
                                            let _ = clip_cmd_tx.send(
                                                ClipboardCommand::SetClipboard { payload },
                                            );
                                        }
                                        Err(e) => error!("Failed to deserialize clip: {}", e),
                                    }
                                }
                                Err(e) => error!("Failed to decrypt clip: {}", e),
                            }
                        }
                    }
                }
            }

            // Clipboard events (only process if watching is enabled)
            Some(event) = clip_event_rx.recv(), if watching => {
                match event {
                    ClipboardEvent::Changed { payload } => {
                        if !connected {
                            continue;
                        }

                        if let Some(age_id) = &age_identity {
                            let recipient = age_id.to_public();
                            match payload.serialize() {
                                Ok(data) => {
                                    let size_bytes = data.len() as u64;
                                    match crypto::encrypt(&data, vec![recipient]) {
                                        Ok(encrypted) => {
                                            let content_type = match &payload {
                                                ClipboardPayload::Text(_) => ClipContentType::Text,
                                                ClipboardPayload::Image { .. } => ClipContentType::Image,
                                                ClipboardPayload::Files(_) => ClipContentType::Files,
                                            };
                                            let _ = stdb_cmd_tx.send(SpacetimeCommand::SyncClip {
                                                device_id: device_id.clone(),
                                                content_type,
                                                encrypted_data: encrypted,
                                                size_bytes,
                                            });
                                        }
                                        Err(e) => error!("Failed to encrypt clip: {}", e),
                                    }
                                }
                                Err(e) => error!("Failed to serialize clip: {}", e),
                            }
                        }
                    }
                }
            }

            // Socket requests from CLI
            Some(req) = socket_req_rx.recv() => {
                let response = handle_request(
                    req.request,
                    connected,
                    user_id,
                    &device_id,
                    watching,
                    &age_identity,
                    &stdb_cmd_tx,
                    &clip_cmd_tx,
                ).await;
                let _ = req.reply.send(response);
            }

            // Socket server failure
            result = &mut socket_handle => {
                match result {
                    Ok(Ok(())) => info!("Socket server shut down"),
                    Ok(Err(e)) => error!("Socket server error: {}", e),
                    Err(e) => error!("Socket server task panicked: {}", e),
                }
                break;
            }
        }
    }

    // Cleanup socket
    let path = config::socket_path();
    let _ = std::fs::remove_file(&path);

    Ok(())
}

async fn handle_request(
    request: Request,
    connected: bool,
    user_id: u64,
    device_id: &str,
    watching: bool,
    age_identity: &Option<age::x25519::Identity>,
    stdb_cmd_tx: &crossbeam_channel::Sender<SpacetimeCommand>,
    clip_cmd_tx: &std::sync::mpsc::Sender<ClipboardCommand>,
) -> Response {
    match request {
        Request::Status => {
            // Look up username from SpacetimeDB
            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = stdb_cmd_tx.send(SpacetimeCommand::GetUsername {
                user_id,
                reply: reply_tx,
            });
            let username = reply_rx.await.ok().flatten();

            Response::Status {
                connected,
                username,
                user_id: Some(user_id),
                device_id: device_id.to_string(),
                watching,
            }
        }

        Request::Copy { data } => {
            let payload = if let Some(data) = data {
                // Data provided (from stdin)
                ClipboardPayload::Text(String::from_utf8_lossy(&data).to_string())
            } else {
                // Read from system clipboard
                let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
                if clip_cmd_tx
                    .send(ClipboardCommand::ReadClipboard { reply: reply_tx })
                    .is_err()
                {
                    return Response::Error {
                        message: "Clipboard thread not available".to_string(),
                    };
                }
                match reply_rx.await {
                    Ok(Some(p)) => p,
                    Ok(None) => {
                        return Response::Error {
                            message: "Clipboard is empty".to_string(),
                        }
                    }
                    Err(_) => {
                        return Response::Error {
                            message: "Clipboard read failed".to_string(),
                        }
                    }
                }
            };

            if !connected {
                return Response::Error {
                    message: "Not connected to SpacetimeDB".to_string(),
                };
            }

            if let Some(age_id) = age_identity {
                let recipient = age_id.to_public();
                match payload.serialize() {
                    Ok(data) => {
                        let size_bytes = data.len() as u64;
                        match crypto::encrypt(&data, vec![recipient]) {
                            Ok(encrypted) => {
                                let content_type = match &payload {
                                    ClipboardPayload::Text(_) => ClipContentType::Text,
                                    ClipboardPayload::Image { .. } => ClipContentType::Image,
                                    ClipboardPayload::Files(_) => ClipContentType::Files,
                                };
                                let _ = stdb_cmd_tx.send(SpacetimeCommand::SyncClip {
                                    device_id: device_id.to_string(),
                                    content_type,
                                    encrypted_data: encrypted,
                                    size_bytes,
                                });
                                Response::Ok
                            }
                            Err(e) => Response::Error {
                                message: format!("Encryption failed: {}", e),
                            },
                        }
                    }
                    Err(e) => Response::Error {
                        message: format!("Serialization failed: {}", e),
                    },
                }
            } else {
                Response::Error {
                    message: "No encryption key configured. Run `clipsync setup`.".to_string(),
                }
            }
        }

        Request::Paste => {
            if !connected {
                return Response::Error {
                    message: "Not connected to SpacetimeDB".to_string(),
                };
            }

            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = stdb_cmd_tx.send(SpacetimeCommand::GetCurrentClip {
                user_id,
                reply: reply_tx,
            });

            match reply_rx.await {
                Ok(Some(clip)) => {
                    if let Some(age_id) = age_identity {
                        match crypto::decrypt(&clip.encrypted_data, age_id) {
                            Ok(plaintext) => match ClipboardPayload::deserialize(&plaintext) {
                                Ok(payload) => {
                                    let data = match &payload {
                                        ClipboardPayload::Text(text) => text.as_bytes().to_vec(),
                                        ClipboardPayload::Image { png_data, .. } => {
                                            png_data.clone()
                                        }
                                        ClipboardPayload::Files(_) => {
                                            match payload.serialize() {
                                                Ok(d) => d,
                                                Err(e) => {
                                                    return Response::Error {
                                                        message: format!(
                                                            "Failed to serialize files: {}",
                                                            e
                                                        ),
                                                    }
                                                }
                                            }
                                        }
                                    };
                                    Response::ClipData {
                                        content_type: payload.content_type_str().to_string(),
                                        data,
                                    }
                                }
                                Err(e) => Response::Error {
                                    message: format!("Failed to deserialize clip: {}", e),
                                },
                            },
                            Err(e) => Response::Error {
                                message: format!("Failed to decrypt clip: {}", e),
                            },
                        }
                    } else {
                        Response::Error {
                            message: "No encryption key configured".to_string(),
                        }
                    }
                }
                Ok(None) => Response::Error {
                    message: "No clip available".to_string(),
                },
                Err(_) => Response::Error {
                    message: "Failed to get clip from SpacetimeDB".to_string(),
                },
            }
        }

        Request::ListDevices => {
            let (reply_tx, reply_rx) = oneshot::channel();
            let _ = stdb_cmd_tx.send(SpacetimeCommand::ListDevices {
                user_id,
                reply: reply_tx,
            });

            match reply_rx.await {
                Ok(devices) => Response::Devices {
                    devices: devices
                        .into_iter()
                        .map(|d| DeviceInfo {
                            id: d.id,
                            device_id: d.device_id,
                            device_name: d.device_name,
                        })
                        .collect(),
                },
                Err(_) => Response::Error {
                    message: "Failed to list devices".to_string(),
                },
            }
        }

        Request::Shutdown => {
            info!("Shutdown requested via socket");
            std::process::exit(0);
        }
    }
}

fn hostname() -> String {
    gethostname::gethostname()
        .to_string_lossy()
        .to_string()
}
