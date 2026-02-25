use anyhow::Result;

use crate::config;

pub async fn run() -> Result<()> {
    // SpacetimeDB auto-generates an anonymous token on first connect.
    // For now, we just check if a token exists.
    match config::load_token()? {
        Some(_) => {
            println!("Already logged in (token saved).");
            println!("The daemon will use this token to authenticate.");
        }
        None => {
            println!("No token found. The daemon will generate one on first connect.");
            println!("Run `clipsync daemon` or `clipsync install` to connect and auto-generate a token.");
        }
    }

    Ok(())
}
