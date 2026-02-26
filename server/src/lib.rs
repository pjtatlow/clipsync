use spacetimedb::{reducer, table, Identity, ReducerContext, SpacetimeType, Table, Timestamp};

// --- Custom Types ---

#[derive(SpacetimeType, Clone, Debug, PartialEq)]
pub enum ClipContentType {
    Text,
    Image,
    Files,
}

// --- Tables ---

#[table(accessor = user, public)]
pub struct User {
    #[primary_key]
    #[auto_inc]
    id: u64,
    #[unique]
    username: String,
    password_hash: String,
    /// age private key encrypted with the user's password (passphrase encryption)
    encrypted_private_key: Vec<u8>,
    /// age public key (bech32 string bytes)
    public_key: Vec<u8>,
    created_at: Timestamp,
}

#[table(accessor = user_identity, public)]
pub struct UserIdentity {
    #[primary_key]
    identity: Identity,
    #[index(btree)]
    user_id: u64,
}

#[table(accessor = device, public)]
pub struct Device {
    #[primary_key]
    #[auto_inc]
    id: u64,
    #[index(btree)]
    user_id: u64,
    device_id: String,
    device_name: String,
    registered_at: Timestamp,
}

#[table(accessor = current_clip, public)]
pub struct CurrentClip {
    #[primary_key]
    user_id: u64,
    sender_device_id: String,
    content_type: ClipContentType,
    encrypted_data: Vec<u8>,
    size_bytes: u64,
    updated_at: Timestamp,
}

// --- Lifecycle Reducers ---

#[reducer(init)]
pub fn init(_ctx: &ReducerContext) {
    log::info!("clipsync module initialized");
}

#[reducer(client_connected)]
pub fn client_connected(ctx: &ReducerContext) {
    log::info!("Client connected: {:?}", ctx.sender());
}

#[reducer(client_disconnected)]
pub fn client_disconnected(ctx: &ReducerContext) {
    log::info!("Client disconnected: {:?}", ctx.sender());
}

// --- Helper ---

fn get_user_id(ctx: &ReducerContext) -> Result<u64, String> {
    ctx.db
        .user_identity()
        .identity()
        .find(ctx.sender())
        .map(|ui| ui.user_id)
        .ok_or_else(|| "Not logged in. Run `clipsync signup` or `clipsync login` first.".to_string())
}

// --- Reducers ---

const MAX_ENCRYPTED_SIZE: usize = 55 * 1024 * 1024; // 55MB overhead margin on 50MB limit

#[reducer]
pub fn signup(
    ctx: &ReducerContext,
    username: String,
    password_hash: String,
    encrypted_private_key: Vec<u8>,
    public_key: Vec<u8>,
    device_id: String,
    device_name: String,
) -> Result<(), String> {
    if username.is_empty() {
        return Err("Username cannot be empty".to_string());
    }
    if device_id.is_empty() {
        return Err("Device ID cannot be empty".to_string());
    }

    // Check username not taken
    for existing in ctx.db.user().iter() {
        if existing.username == username {
            return Err(format!("Username '{}' is already taken", username));
        }
    }

    // Check this identity isn't already linked to a user
    if ctx.db.user_identity().identity().find(ctx.sender()).is_some() {
        return Err("This connection is already linked to a user".to_string());
    }

    // Create user
    let user = ctx.db.user().insert(User {
        id: 0, // auto_inc
        username: username.clone(),
        password_hash,
        encrypted_private_key,
        public_key,
        created_at: ctx.timestamp,
    });

    // Link this identity to the user
    ctx.db.user_identity().insert(UserIdentity {
        identity: ctx.sender(),
        user_id: user.id,
    });

    // Register device
    ctx.db.device().insert(Device {
        id: 0, // auto_inc
        user_id: user.id,
        device_id: device_id.clone(),
        device_name,
        registered_at: ctx.timestamp,
    });

    log::info!("User '{}' signed up (id={}), device '{}'", username, user.id, device_id);
    Ok(())
}

#[reducer]
pub fn login(
    ctx: &ReducerContext,
    username: String,
    password_hash: String,
    device_id: String,
    device_name: String,
) -> Result<(), String> {
    if username.is_empty() {
        return Err("Username cannot be empty".to_string());
    }

    // Find user by username
    let user = ctx
        .db
        .user()
        .iter()
        .find(|u| u.username == username)
        .ok_or_else(|| format!("User '{}' not found", username))?;

    // Verify password
    if user.password_hash != password_hash {
        return Err("Invalid password".to_string());
    }

    // Link this identity to the user (or update if already linked)
    if let Some(existing) = ctx.db.user_identity().identity().find(ctx.sender()) {
        if existing.user_id != user.id {
            ctx.db.user_identity().identity().update(UserIdentity {
                user_id: user.id,
                ..existing
            });
        }
    } else {
        ctx.db.user_identity().insert(UserIdentity {
            identity: ctx.sender(),
            user_id: user.id,
        });
    }

    // Register or update device
    for existing in ctx.db.device().user_id().filter(&user.id) {
        if existing.device_id == device_id {
            ctx.db.device().id().update(Device {
                device_name,
                registered_at: ctx.timestamp,
                ..existing
            });
            log::info!("User '{}' logged in, device '{}' updated", username, device_id);
            return Ok(());
        }
    }

    // New device
    ctx.db.device().insert(Device {
        id: 0, // auto_inc
        user_id: user.id,
        device_id: device_id.clone(),
        device_name,
        registered_at: ctx.timestamp,
    });

    log::info!("User '{}' logged in, new device '{}'", username, device_id);
    Ok(())
}

#[reducer]
pub fn register_device(
    ctx: &ReducerContext,
    device_id: String,
    device_name: String,
) -> Result<(), String> {
    if device_id.is_empty() {
        return Err("device_id cannot be empty".to_string());
    }

    let user_id = get_user_id(ctx)?;

    // Dedup by (user_id, device_id)
    for existing in ctx.db.device().user_id().filter(&user_id) {
        if existing.device_id == device_id {
            ctx.db.device().id().update(Device {
                device_name,
                registered_at: ctx.timestamp,
                ..existing
            });
            log::info!("Device updated: {} for user {}", device_id, user_id);
            return Ok(());
        }
    }

    ctx.db.device().insert(Device {
        id: 0, // auto_inc
        user_id,
        device_id: device_id.clone(),
        device_name,
        registered_at: ctx.timestamp,
    });

    log::info!("Device registered: {} for user {}", device_id, user_id);
    Ok(())
}

#[reducer]
pub fn unregister_device(ctx: &ReducerContext, device_id: String) -> Result<(), String> {
    let user_id = get_user_id(ctx)?;

    for existing in ctx.db.device().user_id().filter(&user_id) {
        if existing.device_id == device_id {
            ctx.db.device().id().delete(&existing.id);
            log::info!("Device unregistered: {} for user {}", device_id, user_id);
            return Ok(());
        }
    }
    Err(format!("Device not found: {}", device_id))
}

#[reducer]
pub fn sync_clip(
    ctx: &ReducerContext,
    device_id: String,
    content_type: ClipContentType,
    encrypted_data: Vec<u8>,
    size_bytes: u64,
) -> Result<(), String> {
    if encrypted_data.len() > MAX_ENCRYPTED_SIZE {
        return Err(format!(
            "Encrypted data too large: {} bytes (max {})",
            encrypted_data.len(),
            MAX_ENCRYPTED_SIZE
        ));
    }

    let user_id = get_user_id(ctx)?;

    if let Some(existing) = ctx.db.current_clip().user_id().find(&user_id) {
        ctx.db.current_clip().user_id().update(CurrentClip {
            sender_device_id: device_id,
            content_type,
            encrypted_data,
            size_bytes,
            updated_at: ctx.timestamp,
            ..existing
        });
    } else {
        ctx.db.current_clip().insert(CurrentClip {
            user_id,
            sender_device_id: device_id,
            content_type,
            encrypted_data,
            size_bytes,
            updated_at: ctx.timestamp,
        });
    }

    log::info!("Clip synced for user {}", user_id);
    Ok(())
}
