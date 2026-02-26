use anyhow::{Context, Result};
use service_manager::*;
use std::ffi::OsString;

const SERVICE_LABEL: &str = "com.clipsync.daemon";

fn service_label() -> Result<ServiceLabel> {
    SERVICE_LABEL
        .parse()
        .map_err(|e: std::io::Error| anyhow::anyhow!("Invalid service label: {}", e))
}

pub async fn install() -> Result<()> {
    let mut manager = <dyn ServiceManager>::native()
        .context("Failed to get native service manager")?;
    manager
        .set_level(ServiceLevel::User)
        .context("Failed to set service level to user")?;

    let exe = std::env::current_exe()?;

    let install_ctx = ServiceInstallCtx {
        label: service_label()?,
        program: exe,
        args: vec![OsString::from("daemon")],
        contents: None,
        username: None,
        working_directory: None,
        environment: None,
        autostart: true,
    };

    manager
        .install(install_ctx)
        .context("Failed to install service")?;

    manager
        .start(ServiceStartCtx {
            label: service_label()?,
        })
        .context("Failed to start service")?;

    println!("Service installed and started.");
    println!("The daemon will start automatically on login.");

    Ok(())
}

pub async fn uninstall() -> Result<()> {
    let mut manager = <dyn ServiceManager>::native()
        .context("Failed to get native service manager")?;
    manager
        .set_level(ServiceLevel::User)
        .context("Failed to set service level to user")?;

    // Try to stop first, ignore errors if not running
    let _ = manager.stop(ServiceStopCtx {
        label: service_label()?,
    });

    manager
        .uninstall(ServiceUninstallCtx {
            label: service_label()?,
        })
        .context("Failed to uninstall service")?;

    println!("Service uninstalled.");

    Ok(())
}
