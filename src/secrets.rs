//! Encryption at rest for third-party credentials (git remote tokens).
//!
//! Secrets are sealed with ChaCha20-Poly1305 under a server key, with a caller-chosen context
//! (e.g. the project id) as associated data, so a sealed blob copied to another row does not
//! open. The key comes from `LIGHTLEAF_SECRET_KEY` (64 hex characters) or, when that is unset,
//! from `$LIGHTLEAF_DATA/secret.key`, created on first start with mode 0600. Keeping the key in
//! the environment (or on another volume) means a leaked database or backup alone reveals nothing.

use anyhow::{Context, bail};
use chacha20poly1305::{
    ChaCha20Poly1305, Key, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use rand::RngCore;
use std::path::Path;

const NONCE_LEN: usize = 12;

pub struct SecretBox {
    cipher: ChaCha20Poly1305,
}

impl SecretBox {
    pub fn from_key(key: [u8; 32]) -> Self {
        SecretBox {
            cipher: ChaCha20Poly1305::new(Key::from_slice(&key)),
        }
    }

    /// The key from `env_key` (hex), else from `<data_dir>/secret.key`, which is created if missing.
    pub fn load(data_dir: &Path, env_key: Option<&str>) -> anyhow::Result<Self> {
        if let Some(hexkey) = env_key {
            return Ok(Self::from_key(
                parse_key(hexkey).context("LIGHTLEAF_SECRET_KEY")?,
            ));
        }
        let path = data_dir.join("secret.key");
        match std::fs::read_to_string(&path) {
            Ok(s) => {
                warn_if_readable_by_others(&path);
                Ok(Self::from_key(
                    parse_key(&s).with_context(|| format!("{}", path.display()))?,
                ))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let mut key = [0u8; 32];
                rand::rngs::OsRng.fill_bytes(&mut key);
                write_private(&path, &hex::encode(key))
                    .with_context(|| format!("creating {}", path.display()))?;
                Ok(Self::from_key(key))
            }
            Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
        }
    }

    /// `nonce || ciphertext+tag`.
    pub fn seal(&self, context: &str, plaintext: &str) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ct = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext.as_bytes(),
                    aad: context.as_bytes(),
                },
            )
            .expect("ChaCha20-Poly1305 encryption cannot fail for in-memory input");
        let mut out = nonce.to_vec();
        out.extend_from_slice(&ct);
        out
    }

    pub fn open(&self, context: &str, blob: &[u8]) -> anyhow::Result<String> {
        if blob.len() < NONCE_LEN {
            bail!("sealed secret is too short");
        }
        let (nonce, ct) = blob.split_at(NONCE_LEN);
        let pt = self
            .cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ct,
                    aad: context.as_bytes(),
                },
            )
            .map_err(|_| {
                anyhow::anyhow!("cannot decrypt a stored secret (was the server key changed?)")
            })?;
        Ok(String::from_utf8(pt)?)
    }
}

fn parse_key(s: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = hex::decode(s.trim()).context("the key must be 64 hex characters")?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("the key must be 64 hex characters (32 bytes)"))
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(contents.as_bytes())
}

#[cfg(unix)]
fn warn_if_readable_by_others(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(m) = std::fs::metadata(path)
        && m.permissions().mode() & 0o077 != 0
    {
        tracing::warn!(
            "{} is readable by other users; run chmod 600 on it",
            path.display()
        );
    }
}

#[cfg(not(unix))]
fn warn_if_readable_by_others(_: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_context_binding() {
        let b = SecretBox::from_key([7; 32]);
        let blob = b.seal("project-a", "olp_token");
        assert_eq!(b.open("project-a", &blob).unwrap(), "olp_token");
        assert!(b.open("project-b", &blob).is_err(), "context must bind");
        let other = SecretBox::from_key([8; 32]);
        assert!(other.open("project-a", &blob).is_err(), "key must bind");
        let mut tampered = blob.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(b.open("project-a", &tampered).is_err());
        assert_ne!(
            b.seal("project-a", "x"),
            b.seal("project-a", "x"),
            "nonces differ"
        );
    }

    #[test]
    fn key_file_is_created_private_and_reused() {
        let dir = std::env::temp_dir().join(format!("ll-secret-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = SecretBox::load(&dir, None).unwrap();
        let blob = a.seal("p", "s");
        let b = SecretBox::load(&dir, None).unwrap();
        assert_eq!(b.open("p", &blob).unwrap(), "s");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("secret.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(SecretBox::load(&dir, Some("abcd")).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
