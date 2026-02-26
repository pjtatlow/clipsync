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
    if password.is_empty() {
        bail!("Password cannot be empty");
    }

    let password_hash = hash_password(&username, &password);

    // Generate a local keypair (used if this is a new account)
    let (local_identity, local_recipient) = crypto::generate_keypair();
    let public_key = crypto::public_key_bytes(&local_recipient);

    // Encrypt local private key with password (stored on server for new accounts)
    use age::secrecy::ExposeSecret;
    let private_key_str = local_identity.to_string().expose_secret().to_string();
    let encrypted_private_key =
        crypto::encrypt_with_passphrase(private_key_str.as_bytes(), &password)?;

    // Generate device ID if needed
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

    // result: Ok((user_id, encrypted_private_key_from_server))
    let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<(u64, Vec<u8>), String>>();
    let (token_tx, token_rx) = std::sync::mpsc::channel::<String>();

    let server_url = config.server_url.clone();
    let database_name = config.database_name.clone();
    let existing_token = config::load_token()?;

    let un = username.clone();
    let ph = password_hash.clone();
    let epk = encrypted_private_key.clone();
    let pk = public_key.clone();
    let did = device_id.clone();
    let dn = device_name.clone();

    std::thread::Builder::new()
        .name("setup-stdb".to_string())
        .spawn(move || {
            let result_tx_sub = result_tx.clone();
            let token_tx_connect = token_tx.clone();

            let un2 = un.clone();
            let ph2 = ph.clone();
            let epk2 = epk.clone();
            let pk2 = pk.clone();
            let did2 = did.clone();
            let dn2 = dn.clone();

            let conn = DbConnection::builder()
                .with_uri(&server_url)
                .with_database_name(&database_name)
                .with_token(existing_token)
                .on_connect(move |conn: &DbConnection, _identity: Identity, token: &str| {
                    let _ = token_tx_connect.send(token.to_string());

                    let rtx = result_tx_sub.clone();
                    let un3 = un2.clone();
                    let ph3 = ph2.clone();
                    let epk3 = epk2.clone();
                    let pk3 = pk2.clone();
                    let did3 = did2.clone();
                    let dn3 = dn2.clone();

                    conn.subscription_builder()
                        .on_applied(move |ctx: &SubscriptionEventContext| {
                            // Call authenticate reducer
                            if let Err(e) = ctx.reducers.authenticate(
                                un3.clone(),
                                ph3.clone(),
                                epk3.clone(),
                                pk3.clone(),
                                did3.clone(),
                                dn3.clone(),
                            ) {
                                let _ = rtx.send(Err(format!("Failed to call authenticate: {}", e)));
                                return;
                            }

                            // Watch for user_identity insert to get our user_id
                            let rtx2 = rtx.clone();
                            ctx.db.user_identity().on_insert(
                                move |ctx2: &EventContext, row: &UserIdentity| {
                                    // Look up the user to get their encrypted_private_key
                                    if let Some(user) = ctx2.db.user().id().find(&row.user_id) {
                                        let _ = rtx2.send(Ok((
                                            row.user_id,
                                            user.encrypted_private_key.clone(),
                                        )));
                                    } else {
                                        let _ = rtx2
                                            .send(Err("User not found after auth".to_string()));
                                    }
                                },
                            );

                            // Also check if identity was already linked (login case where
                            // user_identity row already exists and won't trigger on_insert)
                            let rtx3 = rtx.clone();
                            if let Some(ui) = ctx
                                .db
                                .user_identity()
                                .identity()
                                .find(&ctx.identity())
                            {
                                if let Some(user) = ctx.db.user().id().find(&ui.user_id) {
                                    let _ = rtx3.send(Ok((
                                        ui.user_id,
                                        user.encrypted_private_key.clone(),
                                    )));
                                }
                            }
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

            std::thread::sleep(Duration::from_secs(60));
        })?;

    // Wait for token
    let token = token_rx
        .recv_timeout(Duration::from_secs(30))
        .with_context(|| "Timed out waiting for SpacetimeDB connection")?;

    // Wait for auth result
    let result = result_rx
        .recv_timeout(Duration::from_secs(30))
        .with_context(|| "Timed out waiting for authentication result")?;

    match result {
        Ok((user_id, server_encrypted_pk)) => {
            // Decrypt the private key from the server with our password.
            // For new accounts, this is the key we just uploaded.
            // For existing accounts, this is the original key.
            let private_key_bytes =
                crypto::decrypt_with_passphrase(&server_encrypted_pk, &password)
                    .with_context(|| "Failed to decrypt private key (wrong password?)")?;

            let private_key_str =
                std::str::from_utf8(&private_key_bytes).with_context(|| "Invalid private key")?;

            let age_identity: age::x25519::Identity = private_key_str
                .trim()
                .parse()
                .map_err(|e| anyhow::anyhow!("Failed to parse private key: {}", e))?;

            // Save everything locally
            config::save_user_id(user_id)?;
            config::save_token(&token)?;
            crypto::store_private_key(&age_identity)?;

            let recipient = age_identity.to_public();

            println!();
            println!("Setup complete!");
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
            bail!("Authentication failed: {}", e);
        }
    }

    Ok(())
}

fn hash_password(username: &str, password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(format!("{}:{}", username, password));
    format!("{:x}", hasher.finalize())
}
