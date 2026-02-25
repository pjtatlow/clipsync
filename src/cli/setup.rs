use anyhow::Result;

use crate::config::{self, Config};
use crate::crypto;

pub async fn run(device_name: String) -> Result<()> {
    // Generate device ID if not exists
    let device_id = match config::load_device_id()? {
        Some(id) => {
            println!("Using existing device ID: {}", id);
            id
        }
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            config::save_device_id(&id)?;
            println!("Generated device ID: {}", id);
            id
        }
    };

    // Generate age keypair if not exists
    let (_identity, recipient) = match crypto::load_private_key() {
        Ok(existing) => {
            let recipient = existing.to_public();
            println!("Using existing encryption key: {}", recipient);
            (existing, recipient)
        }
        Err(_) => {
            let (identity, recipient) = crypto::generate_keypair();
            crypto::store_private_key(&identity)?;
            println!("Generated encryption key: {}", recipient);
            (identity, recipient)
        }
    };

    // Ensure config exists
    let config = Config::load().unwrap_or_default();
    config.save()?;

    println!();
    println!("Setup complete!");
    println!("  Device ID:  {}", device_id);
    println!("  Device Name: {}", device_name);
    println!("  Public Key: {}", recipient);
    println!();
    println!("Start the daemon with: clipsync daemon");
    println!("Or install as a service: clipsync install");

    Ok(())
}
