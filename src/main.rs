mod cli;
mod config;
mod crypto;
mod daemon;
mod files;
mod module_bindings;
mod payload;
mod protocol;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "clipsync",
    version,
    about = "Encrypted clipboard and configuration sync across machines"
)]
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
        /// Invite code (required for new accounts, not needed for first user or login)
        #[arg(long)]
        invite_code: Option<String>,
    },
    /// Sync clipboard content to SpacetimeDB
    Copy,
    /// Get latest clip from SpacetimeDB
    Paste {
        /// Only print the content type (text, image, files) instead of the data
        #[arg(long)]
        r#type: bool,
    },
    /// Manage live, versioned configuration collections
    Files {
        #[command(subcommand)]
        command: files::FileCommand,
    },
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
    /// Generate an invite code (admin only, daemon must be running)
    Invite,
    /// Behave like xclip, backed by clipsync (for use as: alias xclip='clipsync xclip')
    #[command(trailing_var_arg = true, allow_hyphen_values = true)]
    Xclip {
        /// Arguments passed to xclip
        args: Vec<String>,
    },
    /// Show daemon logs
    Logs {
        /// Follow log output (like tail -f)
        #[arg(short, long)]
        follow: bool,
        /// Number of lines to show (default: 100)
        #[arg(short = 'n', long)]
        lines: Option<u32>,
    },
    /// Restart the daemon service
    Restart,
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
        Command::Setup {
            username,
            invite_code,
        } => cli::setup::run(username, invite_code).await?,
        Command::Copy => cli::copy::run().await?,
        Command::Paste { r#type } => cli::paste::run(r#type).await?,
        Command::Files { command } => {
            // Resolve relative paths in the caller's working directory, not the daemon's.
            let command = match command {
                files::FileCommand::Add {
                    name,
                    path,
                    directory,
                } => files::FileCommand::Add {
                    name,
                    path: if path.is_absolute() {
                        path
                    } else {
                        std::env::current_dir()?.join(path)
                    },
                    directory,
                },
                files::FileCommand::Bootstrap {
                    dry_run,
                    replace,
                    only,
                    mappings,
                } => {
                    let mappings = files::normalize_mappings(mappings)?;
                    files::FileCommand::Bootstrap {
                        dry_run,
                        replace,
                        only,
                        mappings,
                    }
                }
                other => other,
            };
            match cli::send_request(protocol::Request::Files { command }).await? {
                protocol::Response::FileResult { output } => println!("{output}"),
                protocol::Response::Error { message } => anyhow::bail!(message),
                _ => anyhow::bail!("Unexpected daemon response"),
            }
        }
        Command::Status => cli::status::run().await?,
        Command::Devices => cli::devices::run().await?,
        Command::Config { key, value } => cli::config::run(key, value)?,
        Command::Invite => cli::invite::run().await?,
        Command::Xclip { args } => cli::xclip::run(args).await?,
        Command::Logs { follow, lines } => cli::logs::run(follow, lines)?,
        Command::Restart => cli::restart::run()?,
        Command::Install => cli::install::install().await?,
        Command::Uninstall => cli::install::uninstall().await?,
    }

    Ok(())
}
