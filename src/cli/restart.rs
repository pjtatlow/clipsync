use anyhow::{Context, Result};
use service_manager::*;

const SERVICE_LABEL: &str = "com.clipsync.daemon";

fn service_label() -> Result<ServiceLabel> {
    SERVICE_LABEL
        .parse()
        .map_err(|e: std::io::Error| anyhow::anyhow!("Invalid service label: {}", e))
}

pub fn run() -> Result<()> {
    let mut manager =
        <dyn ServiceManager>::native().context("Failed to get native service manager")?;
    manager
        .set_level(ServiceLevel::User)
        .context("Failed to set service level to user")?;

    let label = service_label()?;

    // Stop (ignore errors if not running)
    let _ = manager.stop(ServiceStopCtx {
        label: label.clone(),
    });

    manager
        .start(ServiceStartCtx { label })
        .context("Failed to start service")?;

    println!("Service restarted.");
    Ok(())
}
