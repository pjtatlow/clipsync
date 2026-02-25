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
    let manager = <dyn ServiceManager>::native()
        .with_context(|| "Failed to get native service manager")?;

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
        .with_context(|| "Failed to install service")?;

    manager
        .start(ServiceStartCtx {
            label: service_label()?,
        })
        .with_context(|| "Failed to start service")?;

    println!("Service installed and started.");
    println!("The daemon will start automatically on login.");

    Ok(())
}

pub async fn uninstall() -> Result<()> {
    let manager = <dyn ServiceManager>::native()
        .with_context(|| "Failed to get native service manager")?;

    // Try to stop first, ignore errors if not running
    let _ = manager.stop(ServiceStopCtx {
        label: service_label()?,
    });

    manager
        .uninstall(ServiceUninstallCtx {
            label: service_label()?,
        })
        .with_context(|| "Failed to uninstall service")?;

    println!("Service uninstalled.");

    Ok(())
}
