use anyhow::{bail, Result};

use crate::protocol::{Request, Response};

pub async fn run() -> Result<()> {
    let response = super::send_request(Request::ListDevices).await?;

    match response {
        Response::Devices { devices } => {
            if devices.is_empty() {
                println!("No devices registered");
            } else {
                println!("{:<6} {:<38} {:<20}", "ID", "Device ID", "Name");
                println!("{}", "-".repeat(64));
                for d in devices {
                    println!("{:<6} {:<38} {:<20}", d.id, d.device_id, d.device_name);
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
