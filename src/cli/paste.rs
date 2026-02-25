use anyhow::Result;
use std::io::{IsTerminal, Write};

use crate::payload::ClipboardPayload;
use crate::protocol::{Request, Response};

pub async fn run() -> Result<()> {
    let response = super::send_request(Request::Paste).await?;

    match response {
        Response::ClipData { content_type, data } => match content_type.as_str() {
            "text" => {
                std::io::stdout().write_all(&data)?;
            }
            "image" => {
                if std::io::stdout().is_terminal() {
                    eprintln!("Image data ({} bytes). Pipe to a file: clipsync paste > image.png", data.len());
                } else {
                    std::io::stdout().write_all(&data)?;
                }
            }
            "files" => {
                let payload: ClipboardPayload = ClipboardPayload::deserialize(&data)?;
                if let ClipboardPayload::Files(files) = payload {
                    for file in files {
                        let path = std::path::Path::new(&file.name);
                        std::fs::write(path, &file.data)?;
                        eprintln!("Wrote {}", file.name);
                    }
                }
            }
            _ => {
                eprintln!("Unknown content type: {}", content_type);
                std::process::exit(1);
            }
        },
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
