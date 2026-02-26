use anyhow::{bail, Result};

use crate::config::Config;

pub fn run(key: Option<String>, value: Option<String>) -> Result<()> {
    let mut config = Config::load().unwrap_or_default();

    match (key, value) {
        // No args: show all config
        (None, None) => {
            println!("watch_clipboard = {}", config.watch_clipboard);
            println!("poll_interval_ms = {}", config.poll_interval_ms);
            println!("server_url = {}", config.server_url);
            println!("database_name = {}", config.database_name);
        }
        // Key only: show that value
        (Some(k), None) => match k.as_str() {
            "watch_clipboard" => println!("{}", config.watch_clipboard),
            "poll_interval_ms" => println!("{}", config.poll_interval_ms),
            "server_url" => println!("{}", config.server_url),
            "database_name" => println!("{}", config.database_name),
            _ => bail!("Unknown config key: {}\nValid keys: watch_clipboard, poll_interval_ms, server_url, database_name", k),
        },
        // Key + value: set it
        (Some(k), Some(v)) => {
            match k.as_str() {
                "watch_clipboard" => {
                    config.watch_clipboard = v.parse()
                        .map_err(|_| anyhow::anyhow!("Expected true or false"))?;
                }
                "poll_interval_ms" => {
                    config.poll_interval_ms = v.parse()
                        .map_err(|_| anyhow::anyhow!("Expected a number"))?;
                }
                "server_url" => config.server_url = v,
                "database_name" => config.database_name = v,
                _ => bail!("Unknown config key: {}\nValid keys: watch_clipboard, poll_interval_ms, server_url, database_name", k),
            }
            config.save()?;
            println!("Set {} = {}", k, match k.as_str() {
                "watch_clipboard" => config.watch_clipboard.to_string(),
                "poll_interval_ms" => config.poll_interval_ms.to_string(),
                "server_url" => config.server_url,
                "database_name" => config.database_name,
                _ => unreachable!(),
            });
            println!("Restart the daemon for changes to take effect.");
        }
        // Value without key doesn't make sense
        (None, Some(_)) => bail!("Must specify a key to set a value"),
    }

    Ok(())
}
