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
    let key_str = identity.to_string().expose_secret().to_string();

    let path = identity_file_path();
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(&path, &key_str).with_context(|| "Failed to write identity file")?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    Ok(())
}

pub fn load_private_key() -> Result<x25519::Identity> {
    let path = identity_file_path();
    let key_str =
        std::fs::read_to_string(&path).with_context(|| "Failed to read identity file")?;
    let identity: x25519::Identity = key_str
        .trim()
        .parse()
        .map_err(|e| anyhow::anyhow!("Failed to parse identity from file: {}", e))?;
    Ok(identity)
}

pub fn encrypt_with_passphrase(data: &[u8], passphrase: &str) -> Result<Vec<u8>> {
    let encryptor = age::Encryptor::with_user_passphrase(
        age::secrecy::SecretString::from(passphrase.to_string()),
    );

    let mut encrypted = vec![];
    let mut writer = encryptor
        .wrap_output(&mut encrypted)
        .with_context(|| "Failed to create age passphrase writer")?;
    writer
        .write_all(data)
        .with_context(|| "Failed to write passphrase-encrypted data")?;
    writer
        .finish()
        .with_context(|| "Failed to finish passphrase encryption")?;

    Ok(encrypted)
}

pub fn decrypt_with_passphrase(encrypted: &[u8], passphrase: &str) -> Result<Vec<u8>> {
    let decryptor = age::Decryptor::new(encrypted)
        .map_err(|e| anyhow::anyhow!("Failed to create passphrase decryptor: {}", e))?;

    let identity = age::scrypt::Identity::new(
        age::secrecy::SecretString::from(passphrase.to_string()),
    );

    let mut decrypted = vec![];
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| anyhow::anyhow!("Failed to decrypt with passphrase: {}", e))?;
    reader
        .read_to_end(&mut decrypted)
        .with_context(|| "Failed to read passphrase-decrypted data")?;

    Ok(decrypted)
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
    fn passphrase_encrypt_decrypt_round_trip() {
        let plaintext = b"secret age private key data";
        let passphrase = "mypassword123";

        let encrypted = encrypt_with_passphrase(plaintext, passphrase).unwrap();
        assert_ne!(encrypted, plaintext);

        let decrypted = decrypt_with_passphrase(&encrypted, passphrase).unwrap();
        assert_eq!(decrypted, plaintext);
    }
}
