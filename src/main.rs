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
    /// Authenticate with SpacetimeDB
    Login,
    /// Generate keys and register device
    Setup {
        /// Name for this device
        device_name: String,
    },
    /// Sync clipboard content to SpacetimeDB
    Copy,
    /// Get latest clip from SpacetimeDB
    Paste,
    /// Send current clip to another user
    Send {
        /// Recipient identity (hex string)
        recipient: String,
    },
    /// Show daemon status
    Status,
    /// List registered devices
    Devices,
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
        Command::Login => cli::login::run().await?,
        Command::Setup { device_name } => cli::setup::run(device_name).await?,
        Command::Copy => cli::copy::run().await?,
        Command::Paste => cli::paste::run().await?,
        Command::Send { recipient } => cli::send::run(recipient).await?,
        Command::Status => cli::status::run().await?,
        Command::Devices => cli::devices::run().await?,
        Command::Install => cli::install::install().await?,
        Command::Uninstall => cli::install::uninstall().await?,
    }

    Ok(())
}
