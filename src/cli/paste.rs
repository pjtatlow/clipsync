use anyhow::{bail, Result};
use std::io::{IsTerminal, Write};

use crate::protocol::{Request, Response};

pub async fn run(type_only: bool) -> Result<()> {
    let response = super::send_request(Request::Paste).await?;

    match response {
        Response::ClipData { content_type, data } => {
            if type_only {
                println!("{}", content_type);
                return Ok(());
            }
            match content_type.as_str() {
                "text" => {
                    std::io::stdout().write_all(&data)?;
                }
                "image" => {
                    if std::io::stdout().is_terminal() {
                        eprintln!(
                            "Image data ({} bytes). Pipe to a file: clipsync paste > image.png",
                            data.len()
                        );
                    } else {
                        std::io::stdout().write_all(&data)?;
                    }
                }
                _ => {
                    bail!("Unknown content type: {}", content_type);
                }
            }
        }
        Response::Error { message } => {
            bail!("{}", message);
        }
        _ => {
            bail!("Unexpected response");
        }
    }

    Ok(())
}
