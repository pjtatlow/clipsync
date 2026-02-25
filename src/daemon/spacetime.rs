use anyhow::Result;
use spacetimedb_sdk::{DbContext, Identity, Table, TableWithPrimaryKey};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};

use crate::config::Config;
use crate::module_bindings::*;

// Import reducer extension traits
use crate::module_bindings::register_device_reducer::register_device;
use crate::module_bindings::register_key_reducer::register_key;
use crate::module_bindings::send_clip_reducer::send_clip;
use crate::module_bindings::sync_clip_reducer::sync_clip;

// Events sent from SpacetimeDB thread to main loop
#[derive(Debug)]
pub enum SpacetimeEvent {
    Connected {
        identity: Identity,
        token: String,
    },
    Disconnected,
    SubscriptionApplied,
    ClipUpdated(CurrentClip),
}

// Commands sent from main loop to SpacetimeDB thread
pub enum SpacetimeCommand {
    SyncClip {
        device_id: String,
        content_type: ClipContentType,
        encrypted_data: Vec<u8>,
        size_bytes: u64,
    },
    SendClip {
        recipient: Identity,
        content_type: ClipContentType,
        encrypted_data: Vec<u8>,
        size_bytes: u64,
    },
    RegisterKey {
        public_key: Vec<u8>,
    },
    RegisterDevice {
        device_id: String,
        device_name: String,
    },
    ListDevices {
        reply: oneshot::Sender<Vec<Device>>,
    },
    LookupKey {
        identity: Identity,
        reply: oneshot::Sender<Option<Vec<u8>>>,
    },
    GetCurrentClip {
        reply: oneshot::Sender<Option<CurrentClip>>,
    },
}

pub fn spawn_spacetime_thread(
    config: &Config,
    token: Option<String>,
    event_tx: mpsc::Sender<SpacetimeEvent>,
    command_rx: crossbeam_channel::Receiver<SpacetimeCommand>,
) -> Result<()> {
    let server_url = config.server_url.clone();
    let database_name = config.database_name.clone();

    std::thread::Builder::new()
        .name("spacetimedb".to_string())
        .spawn(move || {
            let event_tx_connect = event_tx.clone();
            let event_tx_disconnect = event_tx.clone();
            let event_tx_sub = event_tx.clone();
            let event_tx_clip = event_tx.clone();

            let conn: DbConnection = DbConnection::builder()
                .with_uri(&server_url)
                .with_database_name(&database_name)
                .with_token(token)
                .on_connect(move |conn: &DbConnection, identity: Identity, token: &str| {
                    info!("Connected to SpacetimeDB as {:?}", identity);

                    let _ = event_tx_connect.blocking_send(SpacetimeEvent::Connected {
                        identity,
                        token: token.to_string(),
                    });

                    // Subscribe to all tables
                    let event_tx_for_sub = event_tx_sub.clone();
                    let event_tx_for_clip = event_tx_clip.clone();

                    conn.subscription_builder()
                        .on_applied(move |ctx: &SubscriptionEventContext| {
                            info!("Subscription applied");
                            let _ = event_tx_for_sub
                                .blocking_send(SpacetimeEvent::SubscriptionApplied);

                            // Register callbacks for clip updates
                            let tx = event_tx_for_clip.clone();
                            ctx.db.current_clip().on_update(move |_ctx: &EventContext, _old: &CurrentClip, new: &CurrentClip| {
                                let _ = tx.blocking_send(SpacetimeEvent::ClipUpdated(new.clone()));
                            });

                            let tx2 = event_tx_for_clip.clone();
                            ctx.db.current_clip().on_insert(move |_ctx: &EventContext, row: &CurrentClip| {
                                let _ =
                                    tx2.blocking_send(SpacetimeEvent::ClipUpdated(row.clone()));
                            });
                        })
                        .subscribe_to_all_tables();
                })
                .on_disconnect(move |_ctx: &ErrorContext, err: Option<spacetimedb_sdk::Error>| {
                    if let Some(e) = err {
                        warn!("Disconnected from SpacetimeDB: {:?}", e);
                    } else {
                        info!("Disconnected from SpacetimeDB");
                    }
                    let _ = event_tx_disconnect.blocking_send(SpacetimeEvent::Disconnected);
                })
                .build()
                .expect("Failed to connect to SpacetimeDB");

            let conn = Arc::new(conn);

            // Run the connection on a background thread
            let conn_for_run = conn.clone();
            let _handle = conn_for_run.run_threaded();

            // Process commands from the main loop
            loop {
                match command_rx.recv() {
                    Ok(cmd) => match cmd {
                        SpacetimeCommand::SyncClip {
                            device_id,
                            content_type,
                            encrypted_data,
                            size_bytes,
                        } => {
                            if let Err(e) = conn.reducers.sync_clip(
                                device_id,
                                content_type,
                                encrypted_data,
                                size_bytes,
                            ) {
                                error!("Failed to call sync_clip: {}", e);
                            }
                        }
                        SpacetimeCommand::SendClip {
                            recipient,
                            content_type,
                            encrypted_data,
                            size_bytes,
                        } => {
                            if let Err(e) = conn.reducers.send_clip(
                                recipient,
                                content_type,
                                encrypted_data,
                                size_bytes,
                            ) {
                                error!("Failed to call send_clip: {}", e);
                            }
                        }
                        SpacetimeCommand::RegisterKey { public_key } => {
                            if let Err(e) = conn.reducers.register_key(public_key) {
                                error!("Failed to call register_key: {}", e);
                            }
                        }
                        SpacetimeCommand::RegisterDevice {
                            device_id,
                            device_name,
                        } => {
                            if let Err(e) =
                                conn.reducers.register_device(device_id, device_name)
                            {
                                error!("Failed to call register_device: {}", e);
                            }
                        }
                        SpacetimeCommand::ListDevices { reply } => {
                            let devices: Vec<Device> = conn.db.device().iter().collect();
                            let _ = reply.send(devices);
                        }
                        SpacetimeCommand::LookupKey { identity, reply } => {
                            let key = conn
                                .db
                                .user_key()
                                .identity()
                                .find(&identity)
                                .map(|uk| uk.public_key.clone());
                            let _ = reply.send(key);
                        }
                        SpacetimeCommand::GetCurrentClip { reply } => {
                            let clip = conn.try_identity().and_then(|id| {
                                conn.db.current_clip().owner().find(&id)
                            });
                            let _ = reply.send(clip);
                        }
                    },
                    Err(_) => {
                        info!("Command channel closed, shutting down SpacetimeDB thread");
                        break;
                    }
                }
            }
        })?;

    Ok(())
}
