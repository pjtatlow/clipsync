use anyhow::{bail, Result};
use std::io::Write;

use crate::protocol::{Request, Response};

pub async fn run(args: Vec<String>) -> Result<()> {
    let mut selection = None;
    let mut target = None;
    let mut output = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-selection" => {
                selection = args.get(i + 1).cloned();
                i += 2;
            }
            "-t" => {
                target = args.get(i + 1).cloned();
                i += 2;
            }
            "-o" => {
                output = true;
                i += 1;
            }
            _ => {
                i += 1;
            }
        }
    }

    // Only handle clipboard reads (-selection clipboard -o)
    if selection.as_deref() != Some("clipboard") || !output {
        return Ok(());
    }

    let response = super::send_request(Request::Paste).await?;

    let clip_type = match &response {
        Response::ClipData { content_type, .. } => content_type.clone(),
        _ => bail!("Unexpected response from daemon"),
    };

    // TARGETS query
    if target.as_deref() == Some("TARGETS") {
        match clip_type.as_str() {
            "image" => println!("image/png"),
            "text" => println!("text/plain"),
            _ => bail!("Unknown clip type: {}", clip_type),
        }
        return Ok(());
    }

    // Image read
    if let Some(t) = &target {
        if t.starts_with("image/") {
            if clip_type == "image" {
                if let Response::ClipData { data, .. } = response {
                    std::io::stdout().write_all(&data)?;
                    return Ok(());
                }
            }
            bail!("No image data available");
        }
    }

    // Text read (explicit text/plain or no target)
    if target.as_deref() == Some("text/plain") || target.is_none() {
        if clip_type == "text" {
            if let Response::ClipData { data, .. } = response {
                std::io::stdout().write_all(&data)?;
                return Ok(());
            }
        }
        bail!("No text data available");
    }

    bail!("Unsupported target: {}", target.unwrap_or_default());
}
