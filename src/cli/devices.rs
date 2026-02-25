use anyhow::Result;

use crate::protocol::{Request, Response};

pub async fn run() -> Result<()> {
    let response = super::send_request(Request::ListDevices).await?;

    match response {
        Response::Devices { devices } => {
            if devices.is_empty() {
                println!("No devices registered");
            } else {
                println!("{:<6} {:<38} {:<20} {}", "ID", "Device ID", "Name", "Owner");
                println!("{}", "-".repeat(80));
                for d in devices {
                    println!(
                        "{:<6} {:<38} {:<20} {}",
                        d.id,
                        d.device_id,
                        d.device_name,
                        &d.owner[..16]
                    );
                }
            }
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
