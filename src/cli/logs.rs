use anyhow::{Context, Result};
use std::process::Command;

const SERVICE_LABEL: &str = "com.clipsync.daemon";

pub fn run(follow: bool, lines: Option<u32>) -> Result<()> {
    let lines = lines.unwrap_or(100);

    if cfg!(target_os = "macos") {
        run_macos(follow, lines)
    } else {
        run_linux(follow, lines)
    }
}

fn run_linux(follow: bool, lines: u32) -> Result<()> {
    let mut args = vec![
        "--user".to_string(),
        "-u".to_string(),
        SERVICE_LABEL.to_string(),
        "-n".to_string(),
        lines.to_string(),
        "--no-pager".to_string(),
    ];

    if follow {
        args.push("-f".to_string());
    }

    let status = Command::new("journalctl")
        .args(&args)
        .status()
        .context("Failed to run journalctl. Is systemd available?")?;

    if !status.success() {
        anyhow::bail!("journalctl exited with status {}", status);
    }

    Ok(())
}

fn run_macos(follow: bool, lines: u32) -> Result<()> {
    // On macOS, launchd logs go to the system log.
    // Use `log` command to query for the subsystem.
    let args = vec![
        "show".to_string(),
        "--predicate".to_string(),
        format!("subsystem == \"{}\" OR process == \"clipsync\"", SERVICE_LABEL),
        "--style".to_string(),
        "compact".to_string(),
        "--last".to_string(),
        format!("{}m", (lines as u64 * 2).max(5)), // rough heuristic: ~2 min per line
    ];

    if follow {
        // `log stream` for live tailing
        let status = Command::new("log")
            .args([
                "stream",
                "--predicate",
                &format!(
                    "subsystem == \"{}\" OR process == \"clipsync\"",
                    SERVICE_LABEL
                ),
                "--style",
                "compact",
            ])
            .status()
            .context("Failed to run `log stream`")?;

        if !status.success() {
            anyhow::bail!("`log stream` exited with status {}", status);
        }
    } else {
        let status = Command::new("log")
            .args(&args)
            .status()
            .context("Failed to run `log show`")?;

        if !status.success() {
            anyhow::bail!("`log show` exited with status {}", status);
        }
    }

    Ok(())
}
