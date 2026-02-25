use anyhow::Result;
use std::io::{IsTerminal, Read};

use crate::protocol::{Request, Response};

pub async fn run() -> Result<()> {
    let data = if std::io::stdin().is_terminal() {
        // Not piped, tell daemon to read system clipboard
        None
    } else {
        // Piped, read from stdin
        let mut buf = Vec::new();
        std::io::stdin().read_to_end(&mut buf)?;
        Some(buf)
    };

    let response = super::send_request(Request::Copy { data }).await?;

    match response {
        Response::Ok => {
            eprintln!("Clipboard synced");
        }
        Response::Error { message } => {
            eprintln!("Error: {}", message);
            std::process::exit(1);
        }
        _ => {
            eprintln!("Unexpected response");
            std::process::exit(1);
        }
    }

    Ok(())
}
