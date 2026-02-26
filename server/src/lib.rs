use spacetimedb::{
    reducer, table, view, Identity, ReducerContext, SpacetimeType, Table, Timestamp, ViewContext,
};

// --- Custom Types ---

#[derive(SpacetimeType, Clone, Debug, PartialEq)]
pub enum ClipContentType {
    Text,
    Image,
}

// --- Tables ---

/// Private table — contains sensitive fields (password_hash, encrypted_private_key).
/// Access via the `my_profile` view instead.
#[table(accessor = user)]
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
    is_admin: bool,
    created_at: Timestamp,
}

/// Return type for the `my_profile` view.
#[derive(SpacetimeType, Clone, Debug)]
pub struct UserProfile {
    pub user_id: u64,
    pub username: String,
    pub public_key: Vec<u8>,
    pub encrypted_private_key: Vec<u8>,
    pub is_admin: bool,
}

#[table(accessor = user_identity)]
pub struct UserIdentity {
    #[primary_key]
    identity: Identity,
    user_id: u64,
}

#[table(accessor = device)]
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

/// Return type for the `my_devices` view.
#[derive(SpacetimeType, Clone, Debug)]
pub struct DeviceView {
    pub id: u64,
    pub device_id: String,
    pub device_name: String,
    pub registered_at: Timestamp,
}

#[table(accessor = current_clip)]
pub struct CurrentClip {
    #[primary_key]
    user_id: u64,
    sender_device_id: String,
    content_type: ClipContentType,
    encrypted_data: Vec<u8>,
    size_bytes: u64,
    updated_at: Timestamp,
}

#[table(accessor = invite_code, private)]
pub struct InviteCode {
    #[primary_key]
    code: String,
    created_by: u64,
    created_at: Timestamp,
    expires_at: Timestamp,
}

#[table(accessor = failed_login)]
pub struct FailedLogin {
    #[primary_key]
    username: String,
    attempt_count: u32,
    first_attempt_at: Timestamp,
    locked_until: Timestamp,
}

// --- Constants ---

const MAX_ENCRYPTED_SIZE: usize = 55 * 1024 * 1024;
const MAX_FAILED_ATTEMPTS: u32 = 5;
const LOCKOUT_DURATION_MICROS: i64 = 15 * 60 * 1_000_000; // 15 minutes
const ATTEMPT_WINDOW_MICROS: i64 = 15 * 60 * 1_000_000; // 15 minutes
const INVITE_CODE_TTL_MICROS: i64 = 24 * 60 * 60 * 1_000_000; // 24 hours

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
        .ok_or_else(|| "Not logged in. Run `clipsync setup` first.".to_string())
}

fn upsert_device(ctx: &ReducerContext, user_id: u64, device_id: &str, device_name: &str) {
    for existing in ctx.db.device().iter() {
        if existing.user_id == user_id && existing.device_id == device_id {
            ctx.db.device().id().update(Device {
                device_name: device_name.to_string(),
                registered_at: ctx.timestamp,
                ..existing
            });
            return;
        }
    }

    ctx.db.device().insert(Device {
        id: 0,
        user_id,
        device_id: device_id.to_string(),
        device_name: device_name.to_string(),
        registered_at: ctx.timestamp,
    });
}

/// Hash a password with Argon2id using the provided RNG for salt generation.
fn hash_password_argon2(ctx: &ReducerContext, password: &str) -> Result<String, String> {
    use argon2::{Argon2, PasswordHasher};
    use password_hash::SaltString;
    use spacetimedb::rand::RngCore;

    let rng = ctx.rng();
    let mut salt_bytes = [0u8; 16];
    (&*rng).fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|e| format!("Salt generation failed: {}", e))?;
    let argon2 = Argon2::default();
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| format!("Password hashing failed: {}", e))?
        .to_string();
    Ok(hash)
}

/// Verify a password against an Argon2id hash.
fn verify_password_argon2(password: &str, hash_str: &str) -> Result<(), String> {
    use argon2::{Argon2, PasswordVerifier};
    use password_hash::PasswordHash;

    let parsed_hash =
        PasswordHash::new(hash_str).map_err(|_| "Authentication failed".to_string())?;
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .map_err(|_| "Authentication failed".to_string())
}

/// Record a failed login attempt and return an error.
/// Implements brute force protection with account lockout.
fn record_failed_login(ctx: &ReducerContext, username: &str) -> String {
    let now = ctx.timestamp;
    let lockout_until = Timestamp::from_micros_since_unix_epoch(
        now.to_micros_since_unix_epoch() + LOCKOUT_DURATION_MICROS,
    );

    if let Some(existing) = ctx.db.failed_login().username().find(&username.to_string()) {
        let new_count = existing.attempt_count + 1;
        let locked_until = if new_count >= MAX_FAILED_ATTEMPTS {
            lockout_until
        } else {
            existing.locked_until
        };
        ctx.db.failed_login().username().update(FailedLogin {
            attempt_count: new_count,
            locked_until,
            ..existing
        });
    } else {
        ctx.db.failed_login().insert(FailedLogin {
            username: username.to_string(),
            attempt_count: 1,
            first_attempt_at: now,
            locked_until: Timestamp::UNIX_EPOCH,
        });
    }

    "Authentication failed".to_string()
}

/// Clear failed login attempts on successful authentication.
fn clear_failed_logins(ctx: &ReducerContext, username: &str) {
    ctx.db
        .failed_login()
        .username()
        .delete(&username.to_string());
}

/// Check if the account is locked due to too many failed attempts.
/// Also resets the counter if the attempt window has expired.
fn check_brute_force_lockout(ctx: &ReducerContext, username: &str) -> Result<(), String> {
    let now = ctx.timestamp;
    if let Some(record) = ctx.db.failed_login().username().find(&username.to_string()) {
        // Check if currently locked out
        if record.locked_until > now {
            return Err("Authentication failed".to_string());
        }

        // Check if the attempt window has expired; if so, reset the counter
        let window_end = Timestamp::from_micros_since_unix_epoch(
            record.first_attempt_at.to_micros_since_unix_epoch() + ATTEMPT_WINDOW_MICROS,
        );
        if now > window_end {
            // Window expired, reset the record
            ctx.db
                .failed_login()
                .username()
                .delete(&username.to_string());
        }
    }
    Ok(())
}

// --- Reducers ---

/// Authenticate a user. Creates a new account if the username doesn't exist,
/// or logs in if it does. Either way, links this connection's identity to the
/// user and registers the device.
///
/// The first user created becomes admin and does not need an invite code.
/// All subsequent registrations require a valid, unused invite code.
#[reducer]
pub fn authenticate(
    ctx: &ReducerContext,
    username: String,
    password: String,
    encrypted_private_key: Vec<u8>,
    public_key: Vec<u8>,
    device_id: String,
    device_name: String,
    invite_code: String,
) -> Result<(), String> {
    if username.is_empty() {
        return Err("Username cannot be empty".to_string());
    }
    if password.len() < 8 {
        return Err("Authentication failed".to_string());
    }
    if device_id.is_empty() {
        return Err("Device ID cannot be empty".to_string());
    }

    // Check if username already exists
    let user = ctx.db.user().iter().find(|u| u.username == username);

    let user_id = if let Some(existing_user) = user {
        // Login: check brute force lockout before attempting password verification
        check_brute_force_lockout(ctx, &username)?;

        // Verify password with Argon2id
        if verify_password_argon2(&password, &existing_user.password_hash).is_err() {
            return Err(record_failed_login(ctx, &username));
        }

        // Successful login: clear any failed login records
        clear_failed_logins(ctx, &username);

        existing_user.id
    } else {
        // Signup: check brute force lockout (prevents invite code guessing)
        check_brute_force_lockout(ctx, &username)?;

        let is_first_user = ctx.db.user().iter().count() == 0;

        if !is_first_user {
            // Require and validate invite code
            if invite_code.is_empty() {
                return Err(record_failed_login(ctx, &username));
            }
            let invite = match ctx.db.invite_code().code().find(&invite_code) {
                Some(inv) => inv,
                None => return Err(record_failed_login(ctx, &username)),
            };

            // Check expiration
            if invite.expires_at < ctx.timestamp {
                ctx.db.invite_code().code().delete(&invite.code);
                return Err(record_failed_login(ctx, &username));
            }

            // Consume the invite code
            ctx.db.invite_code().code().delete(&invite.code);
        }

        // Hash the password with Argon2id
        let password_hash = hash_password_argon2(ctx, &password)?;

        let new_user = ctx.db.user().insert(User {
            id: 0,
            username: username.clone(),
            password_hash,
            encrypted_private_key,
            public_key,
            is_admin: is_first_user,
            created_at: ctx.timestamp,
        });

        // Successful registration: clear any failed login records
        clear_failed_logins(ctx, &username);

        log::info!(
            "New user '{}' created (id={}, admin={})",
            username,
            new_user.id,
            is_first_user
        );
        new_user.id
    };

    // Link this identity to the user (upsert)
    if let Some(existing) = ctx.db.user_identity().identity().find(ctx.sender()) {
        if existing.user_id != user_id {
            ctx.db.user_identity().identity().update(UserIdentity {
                user_id,
                ..existing
            });
        }
    } else {
        ctx.db.user_identity().insert(UserIdentity {
            identity: ctx.sender(),
            user_id,
        });
    }

    // Register or update device
    upsert_device(ctx, user_id, &device_id, &device_name);

    log::info!("User '{}' authenticated, device '{}'", username, device_id);
    Ok(())
}

/// Create a single-use invite code. Only admins can create invite codes.
#[reducer]
pub fn create_invite_code(ctx: &ReducerContext, code: String) -> Result<(), String> {
    if code.is_empty() {
        return Err("Invite code cannot be empty".to_string());
    }

    if code.len() < 32 {
        return Err("Invite code must be at least 32 characters".to_string());
    }

    let unique_chars = code.chars().collect::<std::collections::HashSet<_>>().len();
    if unique_chars < 16 {
        return Err("Invite code has insufficient entropy".to_string());
    }

    let user_id = get_user_id(ctx)?;
    let user = ctx
        .db
        .user()
        .id()
        .find(&user_id)
        .ok_or_else(|| "User not found".to_string())?;

    if !user.is_admin {
        return Err("Only admins can create invite codes".to_string());
    }

    if ctx.db.invite_code().code().find(&code).is_some() {
        return Err("Invite code already exists".to_string());
    }

    ctx.db.invite_code().insert(InviteCode {
        code: code.clone(),
        created_by: user_id,
        created_at: ctx.timestamp,
        expires_at: Timestamp::from_micros_since_unix_epoch(
            ctx.timestamp.to_micros_since_unix_epoch() + INVITE_CODE_TTL_MICROS,
        ),
    });

    log::info!("Invite code created by admin user_id={}", user_id);
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
    upsert_device(ctx, user_id, &device_id, &device_name);
    Ok(())
}

#[reducer]
pub fn unregister_device(ctx: &ReducerContext, device_id: String) -> Result<(), String> {
    let user_id = get_user_id(ctx)?;

    for existing in ctx.db.device().iter() {
        if existing.user_id == user_id && existing.device_id == device_id {
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

// --- Views ---

/// Returns the current user's profile. Clients use this to get their own
/// user info (including encrypted_private_key) without the user table being public.
#[view(accessor = my_profile, public)]
fn my_profile(ctx: &ViewContext) -> Option<UserProfile> {
    let ui = ctx.db.user_identity().identity().find(ctx.sender())?;
    let user = ctx.db.user().id().find(&ui.user_id)?;
    Some(UserProfile {
        user_id: user.id,
        username: user.username.clone(),
        public_key: user.public_key.clone(),
        encrypted_private_key: user.encrypted_private_key.clone(),
        is_admin: user.is_admin,
    })
}

/// Returns the current user's devices.
#[view(accessor = my_devices, public)]
fn my_devices(ctx: &ViewContext) -> Vec<DeviceView> {
    let Some(ui) = ctx.db.user_identity().identity().find(ctx.sender()) else {
        return vec![];
    };
    ctx.db
        .device()
        .user_id()
        .filter(&ui.user_id)
        .map(|d| DeviceView {
            id: d.id,
            device_id: d.device_id.clone(),
            device_name: d.device_name.clone(),
            registered_at: d.registered_at,
        })
        .collect()
}

/// Returns the current user's current clipboard content.
#[view(accessor = my_current_clip, public)]
fn my_current_clip(ctx: &ViewContext) -> Option<CurrentClip> {
    let ui = ctx.db.user_identity().identity().find(ctx.sender())?;
    ctx.db.current_clip().user_id().find(&ui.user_id)
}
