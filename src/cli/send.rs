use anyhow::Result;

use crate::protocol::{Request, Response};

pub async fn run(recipient: String) -> Result<()> {
    let response = super::send_request(Request::Send { recipient }).await?;

    match response {
        Response::Ok => {
            eprintln!("Clip sent");
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
