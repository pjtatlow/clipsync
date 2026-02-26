mod cli;
mod config;
mod crypto;
mod daemon;
mod module_bindings;
mod payload;
mod protocol;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "clipsync", about = "Clipboard sync across machines")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Start the daemon (foreground)
    Daemon,
    /// Set up this device (creates account or logs in)
    Setup {
        /// Username
        username: String,
    },
    /// Sync clipboard content to SpacetimeDB
    Copy,
    /// Get latest clip from SpacetimeDB
    Paste,
    /// Show daemon status
    Status,
    /// List registered devices
    Devices,
    /// Get or set config values
    Config {
        /// Config key (watch_clipboard, poll_interval_ms, server_url, database_name)
        key: Option<String>,
        /// Value to set (omit to read current value)
        value: Option<String>,
    },
    /// Install as a system service
    Install,
    /// Remove the system service
    Uninstall,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Daemon => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
                )
                .init();

            let config = config::Config::load().unwrap_or_default();
            daemon::run_daemon(config).await?;
        }
        Command::Setup { username } => cli::setup::run(username).await?,
        Command::Copy => cli::copy::run().await?,
        Command::Paste => cli::paste::run().await?,
        Command::Status => cli::status::run().await?,
        Command::Devices => cli::devices::run().await?,
        Command::Config { key, value } => cli::config::run(key, value)?,
        Command::Install => cli::install::install().await?,
        Command::Uninstall => cli::install::uninstall().await?,
    }

    Ok(())
}
