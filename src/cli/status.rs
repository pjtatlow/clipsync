use anyhow::Result;

use crate::protocol::{Request, Response};

pub async fn run() -> Result<()> {
    let response = super::send_request(Request::Status).await?;

    match response {
        Response::Status {
            connected,
            username,
            user_id,
            device_id,
            watching,
        } => {
            println!("Connected: {}", connected);
            if let Some(name) = username {
                println!("Username:  {}", name);
            }
            if let Some(uid) = user_id {
                println!("User ID:   {}", uid);
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
