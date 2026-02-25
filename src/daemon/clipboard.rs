use anyhow::Result;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

use crate::payload::{self, ClipboardPayload};

#[derive(Debug)]
pub enum ClipboardEvent {
    Changed { payload: ClipboardPayload },
}

pub enum ClipboardCommand {
    SetClipboard { payload: ClipboardPayload },
    ReadClipboard { reply: tokio::sync::oneshot::Sender<Option<ClipboardPayload>> },
}

fn hash_bytes(data: &[u8]) -> u64 {
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
}

/// Spawn clipboard polling thread that detects changes.
pub fn spawn_clipboard_watcher(
    poll_interval_ms: u64,
    event_tx: mpsc::Sender<ClipboardEvent>,
    command_rx: std::sync::mpsc::Receiver<ClipboardCommand>,
) -> Result<()> {
    let last_written_hash: Arc<Mutex<Option<u64>>> = Arc::new(Mutex::new(None));
    let last_written_hash_for_cmd = last_written_hash.clone();

    // Spawn the command handler + clipboard poller in one thread
    std::thread::Builder::new()
        .name("clipboard".to_string())
        .spawn(move || {
            let mut clipboard = match arboard::Clipboard::new() {
                Ok(cb) => cb,
                Err(e) => {
                    error!("Failed to initialize clipboard: {}", e);
                    return;
                }
            };

            let mut last_hash: Option<u64> = None;
            let poll_dur = std::time::Duration::from_millis(poll_interval_ms);

            loop {
                // Process any pending commands (non-blocking)
                while let Ok(cmd) = command_rx.try_recv() {
                    match cmd {
                        ClipboardCommand::SetClipboard { payload } => {
                            match &payload {
                                ClipboardPayload::Text(text) => {
                                    let h = hash_bytes(text.as_bytes());
                                    *last_written_hash_for_cmd.lock().unwrap() = Some(h);
                                    last_hash = Some(h);
                                    if let Err(e) = clipboard.set_text(text) {
                                        error!("Failed to set clipboard text: {}", e);
                                    }
                                }
                                ClipboardPayload::Image {
                                    png_data,
                                    ..
                                } => {
                                    match payload::png_to_rgba(png_data) {
                                        Ok((w, h, rgba)) => {
                                            let hash = hash_bytes(&rgba);
                                            *last_written_hash_for_cmd.lock().unwrap() =
                                                Some(hash);
                                            last_hash = Some(hash);
                                            let img_data = arboard::ImageData {
                                                width: w as usize,
                                                height: h as usize,
                                                bytes: rgba.into(),
                                            };
                                            if let Err(e) = clipboard.set_image(img_data) {
                                                error!("Failed to set clipboard image: {}", e);
                                            }
                                        }
                                        Err(e) => {
                                            error!("Failed to decode PNG for clipboard: {}", e);
                                        }
                                    }
                                }
                                ClipboardPayload::Files(_) => {
                                    // Files can't be set to clipboard directly via arboard
                                    warn!("File clipboard sync not supported for setting clipboard");
                                }
                            }
                        }
                        ClipboardCommand::ReadClipboard { reply } => {
                            let payload = read_clipboard(&mut clipboard);
                            let _ = reply.send(payload);
                        }
                    }
                }

                // Poll clipboard for changes
                if let Some(current_payload) = read_clipboard(&mut clipboard) {
                    let current_hash = match &current_payload {
                        ClipboardPayload::Text(text) => hash_bytes(text.as_bytes()),
                        ClipboardPayload::Image { png_data, .. } => {
                            // Hash raw clipboard data, not the PNG encoding
                            // But since we only have PNG here, we use it
                            hash_bytes(png_data)
                        }
                        ClipboardPayload::Files(_) => 0, // Won't happen from arboard
                    };

                    let should_notify = match last_hash {
                        Some(prev) => prev != current_hash,
                        None => true,
                    };

                    if should_notify {
                        // Check if this is content we just wrote
                        let was_written = {
                            let guard = last_written_hash.lock().unwrap();
                            guard.as_ref() == Some(&current_hash)
                        };

                        if !was_written {
                            debug!("Clipboard changed, notifying");
                            if event_tx
                                .blocking_send(ClipboardEvent::Changed {
                                    payload: current_payload,
                                })
                                .is_err()
                            {
                                break;
                            }
                        } else {
                            // Clear the written hash now that we've seen it
                            *last_written_hash.lock().unwrap() = None;
                        }

                        last_hash = Some(current_hash);
                    }
                }

                std::thread::sleep(poll_dur);
            }
        })?;

    Ok(())
}

fn read_clipboard(clipboard: &mut arboard::Clipboard) -> Option<ClipboardPayload> {
    // Try text first
    if let Ok(text) = clipboard.get_text() {
        if !text.is_empty() {
            return Some(ClipboardPayload::Text(text));
        }
    }

    // Try image
    if let Ok(img) = clipboard.get_image() {
        let rgba = img.bytes.to_vec();
        let width = img.width as u32;
        let height = img.height as u32;
        match payload::rgba_to_png(&rgba, width, height) {
            Ok(png_data) => {
                return Some(ClipboardPayload::Image {
                    width,
                    height,
                    png_data,
                });
            }
            Err(e) => {
                warn!("Failed to convert clipboard image to PNG: {}", e);
            }
        }
    }

    None
}
