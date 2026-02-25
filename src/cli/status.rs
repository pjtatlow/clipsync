use anyhow::Result;

use crate::protocol::{Request, Response};

pub async fn run() -> Result<()> {
    let response = super::send_request(Request::Status).await?;

    match response {
        Response::Status {
            connected,
            identity,
            device_id,
            watching,
        } => {
            println!("Connected: {}", connected);
            if let Some(id) = identity {
                println!("Identity:  {}", id);
            }
            println!("Device ID: {}", device_id);
            println!("Watching:  {}", watching);
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
