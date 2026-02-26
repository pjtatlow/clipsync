use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use spacetimedb_sdk::{DbContext, Identity, Table};
use std::sync::Arc;
use std::time::Duration;

use crate::config::{self, Config};
use crate::crypto;
use crate::module_bindings::*;

pub async fn run(username: String) -> Result<()> {
    let password = rpassword::prompt_password("Password: ")?;
    let confirm = rpassword::prompt_password("Confirm password: ")?;
    if password != confirm {
        bail!("Passwords don't match");
    }

    let password_hash = hash_password(&username, &password);

    // Generate age keypair
    let (age_identity, recipient) = crypto::generate_keypair();
    let public_key = crypto::public_key_bytes(&recipient);

    // Encrypt private key with password for server storage
    use age::secrecy::ExposeSecret;
    let private_key_str = age_identity.to_string().expose_secret().to_string();
    let encrypted_private_key = crypto::encrypt_with_passphrase(private_key_str.as_bytes(), &password)?;

    // Generate device ID
    let device_id = match config::load_device_id()? {
        Some(id) => id,
        None => {
            let id = uuid::Uuid::new_v4().to_string();
            config::save_device_id(&id)?;
            id
        }
    };
    let device_name = gethostname::gethostname().to_string_lossy().to_string();

    // Ensure config exists
    let config = Config::load().unwrap_or_default();
    config.save()?;

    println!("Connecting to SpacetimeDB...");

    // Connect to SpacetimeDB and call signup reducer
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<(u64, String), String>>();
    let (token_tx, token_rx) = std::sync::mpsc::channel::<(Identity, String)>();

    let server_url = config.server_url.clone();
    let database_name = config.database_name.clone();
    let existing_token = config::load_token()?;

    let username_clone = username.clone();
    let password_hash_clone = password_hash.clone();
    let encrypted_pk_clone = encrypted_private_key.clone();
    let public_key_clone = public_key.clone();
    let device_id_clone = device_id.clone();
    let device_name_clone = device_name.clone();

    std::thread::Builder::new()
        .name("signup-stdb".to_string())
        .spawn(move || {
            let result_tx_sub = result_tx.clone();
            let token_tx_connect = token_tx.clone();

            let username_for_sub = username_clone.clone();
            let password_hash_for_sub = password_hash_clone.clone();
            let encrypted_pk_for_sub = encrypted_pk_clone.clone();
            let public_key_for_sub = public_key_clone.clone();
            let device_id_for_sub = device_id_clone.clone();
            let device_name_for_sub = device_name_clone.clone();

            let conn = DbConnection::builder()
                .with_uri(&server_url)
                .with_database_name(&database_name)
                .with_token(existing_token)
                .on_connect(move |conn: &DbConnection, identity: Identity, token: &str| {
                    let _ = token_tx_connect.send((identity, token.to_string()));

                    let rtx = result_tx_sub.clone();

                    conn.subscription_builder()
                        .on_applied(move |ctx: &SubscriptionEventContext| {
                            // Check if we're already linked to a user
                            if let Some(ui) = ctx.db.user_identity().identity().find(&ctx.identity()) {
                                let _ = rtx.send(Err(format!(
                                    "This connection is already linked to user ID {}. Use `clipsync login` instead.",
                                    ui.user_id
                                )));
                                return;
                            }

                            // Call signup reducer
                            if let Err(e) = ctx.reducers.signup(
                                username_for_sub.clone(),
                                password_hash_for_sub.clone(),
                                encrypted_pk_for_sub.clone(),
                                public_key_for_sub.clone(),
                                device_id_for_sub.clone(),
                                device_name_for_sub.clone(),
                            ) {
                                let _ = rtx.send(Err(format!("Failed to call signup: {}", e)));
                                return;
                            }

                            // Watch for user_identity insert to get our user_id
                            let rtx2 = rtx.clone();
                            ctx.db.user_identity().on_insert(move |_ctx: &EventContext, row: &UserIdentity| {
                                let _ = rtx2.send(Ok((row.user_id, String::new())));
                            });
                        })
                        .subscribe_to_all_tables();
                })
                .on_disconnect(move |_ctx: &ErrorContext, err: Option<spacetimedb_sdk::Error>| {
                    if let Some(e) = err {
                        let _ = result_tx.send(Err(format!("Disconnected: {:?}", e)));
                    }
                })
                .build()
                .expect("Failed to connect to SpacetimeDB");

            let conn = Arc::new(conn);
            let _handle = conn.run_threaded();

            // Keep thread alive until result is sent
            std::thread::sleep(Duration::from_secs(60));
        })?;

    // Wait for identity and token
    let (_, token) = token_rx
        .recv_timeout(Duration::from_secs(30))
        .with_context(|| "Timed out waiting for SpacetimeDB connection")?;

    // Wait for signup result
    let result = result_rx
        .recv_timeout(Duration::from_secs(30))
        .with_context(|| "Timed out waiting for signup result")?;

    match result {
        Ok((user_id, _)) => {
            // Save everything locally
            config::save_user_id(user_id)?;
            config::save_token(&token)?;
            crypto::store_private_key(&age_identity)?;

            println!();
            println!("Account created!");
            println!("  Username:    {}", username);
            println!("  User ID:     {}", user_id);
            println!("  Device ID:   {}", device_id);
            println!("  Device Name: {}", device_name);
            println!("  Public Key:  {}", recipient);
            println!();
            println!("Start the daemon with: clipsync daemon");
            println!("Or install as a service: clipsync install");
        }
        Err(e) => {
            bail!("Signup failed: {}", e);
        }
    }

    Ok(())
}

pub fn hash_password(username: &str, password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{}:{}", username, password));
    format!("{:x}", hasher.finalize())
}
