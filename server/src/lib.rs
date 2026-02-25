use spacetimedb::{reducer, table, Identity, ReducerContext, SpacetimeType, Table, Timestamp};

// --- Custom Types ---

#[derive(SpacetimeType, Clone, Debug, PartialEq)]
pub enum ClipContentType {
    Text,
    Image,
    Files,
}

// --- Tables ---

#[table(accessor = user_key, public)]
pub struct UserKey {
    #[primary_key]
    identity: Identity,
    public_key: Vec<u8>,
    updated_at: Timestamp,
}

#[table(accessor = device, public)]
pub struct Device {
    #[primary_key]
    #[auto_inc]
    id: u64,
    #[index(btree)]
    owner: Identity,
    device_id: String,
    device_name: String,
    registered_at: Timestamp,
}

#[table(accessor = current_clip, public)]
pub struct CurrentClip {
    #[primary_key]
    owner: Identity,
    sender: Identity,
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

// --- Reducers ---

const MAX_ENCRYPTED_SIZE: usize = 55 * 1024 * 1024; // 55MB overhead margin on 50MB limit

#[reducer]
pub fn register_key(ctx: &ReducerContext, public_key: Vec<u8>) -> Result<(), String> {
    if let Some(existing) = ctx.db.user_key().identity().find(ctx.sender()) {
        ctx.db.user_key().identity().update(UserKey {
            public_key,
            updated_at: ctx.timestamp,
            ..existing
        });
    } else {
        ctx.db.user_key().insert(UserKey {
            identity: ctx.sender(),
            public_key,
            updated_at: ctx.timestamp,
        });
    }

    log::info!("Key registered for {:?}", ctx.sender());
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

    // Dedup by (owner, device_id) using the btree index on owner
    for existing in ctx.db.device().owner().filter(&ctx.sender()) {
        if existing.device_id == device_id {
            // Update existing device
            ctx.db.device().id().update(Device {
                device_name,
                registered_at: ctx.timestamp,
                ..existing
            });
            log::info!("Device updated: {} for {:?}", device_id, ctx.sender());
            return Ok(());
        }
    }

    // Insert new device
    ctx.db.device().insert(Device {
        id: 0, // auto-inc
        owner: ctx.sender(),
        device_id: device_id.clone(),
        device_name,
        registered_at: ctx.timestamp,
    });

    log::info!("Device registered: {} for {:?}", device_id, ctx.sender());
    Ok(())
}

#[reducer]
pub fn unregister_device(ctx: &ReducerContext, device_id: String) -> Result<(), String> {
    for existing in ctx.db.device().owner().filter(&ctx.sender()) {
        if existing.device_id == device_id {
            ctx.db.device().id().delete(&existing.id);
            log::info!("Device unregistered: {} for {:?}", device_id, ctx.sender());
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

    if let Some(existing) = ctx.db.current_clip().owner().find(ctx.sender()) {
        ctx.db.current_clip().owner().update(CurrentClip {
            sender: ctx.sender(),
            sender_device_id: device_id,
            content_type,
            encrypted_data,
            size_bytes,
            updated_at: ctx.timestamp,
            ..existing
        });
    } else {
        ctx.db.current_clip().insert(CurrentClip {
            owner: ctx.sender(),
            sender: ctx.sender(),
            sender_device_id: device_id,
            content_type,
            encrypted_data,
            size_bytes,
            updated_at: ctx.timestamp,
        });
    }

    log::info!("Clip synced for {:?}", ctx.sender());
    Ok(())
}

#[reducer]
pub fn send_clip(
    ctx: &ReducerContext,
    recipient: Identity,
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

    // Verify recipient has a registered key
    if ctx.db.user_key().identity().find(recipient).is_none() {
        return Err("Recipient has no registered public key".to_string());
    }

    if let Some(existing) = ctx.db.current_clip().owner().find(recipient) {
        ctx.db.current_clip().owner().update(CurrentClip {
            sender: ctx.sender(),
            sender_device_id: String::new(),
            content_type,
            encrypted_data,
            size_bytes,
            updated_at: ctx.timestamp,
            ..existing
        });
    } else {
        ctx.db.current_clip().insert(CurrentClip {
            owner: recipient,
            sender: ctx.sender(),
            sender_device_id: String::new(),
            content_type,
            encrypted_data,
            size_bytes,
            updated_at: ctx.timestamp,
        });
    }

    log::info!("Clip sent from {:?} to {:?}", ctx.sender(), recipient);
    Ok(())
}
