use anyhow::{bail, Result};

use crate::protocol::{Request, Response};

pub async fn run() -> Result<()> {
    let code = uuid::Uuid::new_v4().to_string();

    let response = super::send_request(Request::CreateInvite { code }).await?;

    match response {
        Response::InviteCreated { code } => {
            println!("Invite code: {}", code);
            println!();
            println!("Share this with the person you want to invite:");
            println!("  clipsync setup <username> --invite-code {}", code);
        }
        Response::Error { message } => {
            bail!("{}", message);
        }
        _ => {
            bail!("Unexpected response from daemon");
        }
    }

    Ok(())
}
