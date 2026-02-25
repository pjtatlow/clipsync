use age::x25519;
use anyhow::{Context, Result};
use std::io::{Read, Write};

use crate::config::config_dir;

pub fn generate_keypair() -> (x25519::Identity, x25519::Recipient) {
    let identity = x25519::Identity::generate();
    let recipient = identity.to_public();
    (identity, recipient)
}

pub fn identity_file_path() -> std::path::PathBuf {
    config_dir().join("identity.age")
}

pub fn store_private_key(identity: &x25519::Identity) -> Result<()> {
    // Try OS keyring first
    if let Ok(entry) = keyring::Entry::new("clipsync", "identity") {
        let key_str = identity.to_string().expose_secret().to_string();
        if entry.set_password(&key_str).is_ok() {
            return Ok(());
        }
    }

    // Fallback to file
    let path = identity_file_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    let key_str = identity.to_string().expose_secret().to_string();
    std::fs::write(&path, &key_str).with_context(|| "Failed to write identity file")?;

    // Set restrictive permissions on Unix
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

pub fn load_private_key() -> Result<x25519::Identity> {
    // Try OS keyring first
    if let Ok(entry) = keyring::Entry::new("clipsync", "identity") {
        if let Ok(key_str) = entry.get_password() {
            let identity: x25519::Identity = key_str
                .parse()
                .map_err(|e| anyhow::anyhow!("Failed to parse identity from keyring: {}", e))?;
            return Ok(identity);
        }
    }

    // Fallback to file
    let path = identity_file_path();
    let key_str =
        std::fs::read_to_string(&path).with_context(|| "Failed to read identity file")?;
    let identity: x25519::Identity = key_str
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse identity from file: {}", e))?;
    Ok(identity)
}

pub fn encrypt(data: &[u8], recipients: Vec<x25519::Recipient>) -> Result<Vec<u8>> {
    // Compress with zstd first
    let compressed = zstd::encode_all(data, 3).with_context(|| "zstd compression failed")?;

    // Encrypt with age
    let recipient_refs: Vec<&dyn age::Recipient> = recipients
        .iter()
        .map(|r| r as &dyn age::Recipient)
        .collect();

    let encryptor = age::Encryptor::with_recipients(recipient_refs.into_iter())
        .map_err(|e| anyhow::anyhow!("Failed to create encryptor: {}", e))?;

    let mut encrypted = vec![];
    let mut writer = encryptor
        .wrap_output(&mut encrypted)
        .with_context(|| "Failed to create age writer")?;
    writer
        .write_all(&compressed)
        .with_context(|| "Failed to write encrypted data")?;
    writer
        .finish()
        .with_context(|| "Failed to finish encryption")?;

    Ok(encrypted)
}

pub fn decrypt(encrypted: &[u8], identity: &x25519::Identity) -> Result<Vec<u8>> {
    let decryptor = age::Decryptor::new(encrypted)
        .map_err(|e| anyhow::anyhow!("Failed to create decryptor: {}", e))?;

    let mut decrypted = vec![];
    let mut reader = decryptor
        .decrypt(std::iter::once(identity as &dyn age::Identity))
        .map_err(|e| anyhow::anyhow!("Failed to decrypt: {}", e))?;
    reader
        .read_to_end(&mut decrypted)
        .with_context(|| "Failed to read decrypted data")?;

    // Decompress with zstd
    let decompressed =
        zstd::decode_all(decrypted.as_slice()).with_context(|| "zstd decompression failed")?;

    Ok(decompressed)
}

// Re-export for convenience
use age::secrecy::ExposeSecret;

pub fn public_key_bytes(recipient: &x25519::Recipient) -> Vec<u8> {
    // age X25519 recipient string is "age1..." bech32. We store the raw string bytes for now.
    // The plan says 32 bytes but age's Recipient doesn't expose raw bytes directly.
    // We'll store the bech32 string representation.
    recipient.to_string().into_bytes()
}

pub fn recipient_from_bytes(bytes: &[u8]) -> Result<x25519::Recipient> {
    let s = std::str::from_utf8(bytes).with_context(|| "Invalid UTF-8 in public key")?;
    s.parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse recipient: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_round_trip() {
        let (identity, recipient) = generate_keypair();
        let plaintext = b"hello world, this is a test of E2E encryption";

        let encrypted = encrypt(plaintext, vec![recipient]).unwrap();
        assert_ne!(encrypted, plaintext);

        let decrypted = decrypt(&encrypted, &identity).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn encrypt_decrypt_large_data() {
        let (identity, recipient) = generate_keypair();
        let plaintext: Vec<u8> = (0..100_000).map(|i| (i % 256) as u8).collect();

        let encrypted = encrypt(&plaintext, vec![recipient]).unwrap();
        let decrypted = decrypt(&encrypted, &identity).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn public_key_bytes_round_trip() {
        let (_identity, recipient) = generate_keypair();
        let bytes = public_key_bytes(&recipient);
        let recovered = recipient_from_bytes(&bytes).unwrap();
        assert_eq!(recovered.to_string(), recipient.to_string());
    }
}
