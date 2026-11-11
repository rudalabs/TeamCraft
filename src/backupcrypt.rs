//! Encryption of backup files (`teamcraft backup --encrypt`).
//!
//! The key is derived from a passphrase with Argon2id (salt and cost in the
//! manifest). Every file – the database dump and each vault object – is
//! encrypted on its own with XChaCha20-Poly1305 in the STREAM construction
//! (64 KiB chunks, each authenticated, the last one marked), so a file is
//! never held in memory, truncation and reordering are detected, and a
//! wrong passphrase or tampering fails before anything is restored:
//!
//! ```text
//! "TCENC001" | 19-byte random nonce | chunk₁ … chunkₙ (≤ 64 KiB + 16-byte tag each)
//! ```
//!
//! The SHA-256 of the plain content is computed on the way in and out, so
//! the manifest and the object names keep naming the plain content.

use std::{
    fs,
    io::{Read, Write},
    path::Path,
};

use anyhow::{bail, Context, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    aead::{
        generic_array::GenericArray,
        stream::{DecryptorBE32, EncryptorBE32},
    },
    KeyInit, XChaCha20Poly1305,
};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 8] = b"TCENC001";
const NONCE_LEN: usize = 19;
const CHUNK: usize = 64 * 1024;
const TAG: usize = 16;
/// Argon2id cost of new backups: 64 MiB, 3 passes, 1 lane.
const M_COST: u32 = 64 * 1024;
const T_COST: u32 = 3;
const P_COST: u32 = 1;

/// A derived backup key with the parameters it came from.
pub struct BackupKey {
    key: [u8; 32],
    salt: [u8; 16],
    m_cost: u32,
    t_cost: u32,
    p_cost: u32,
}

/// Never shows the key.
impl std::fmt::Debug for BackupKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackupKey")
            .field("key", &"<redacted>")
            .field("m_cost", &self.m_cost)
            .finish()
    }
}

impl BackupKey {
    /// A fresh key for a new backup (random salt).
    pub fn new(passphrase: &str) -> Result<BackupKey> {
        let mut salt = [0u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        Self::derive(passphrase, salt, M_COST, T_COST, P_COST)
    }

    /// A key with cheap derivation parameters, for tests that need a key but
    /// do not test the cost (the real cost takes seconds in a debug build).
    #[cfg(test)]
    pub(crate) fn for_tests(passphrase: &str) -> BackupKey {
        Self::derive(passphrase, [7; 16], 64, 1, 1).expect("test key")
    }

    fn derive(
        passphrase: &str,
        salt: [u8; 16],
        m_cost: u32,
        t_cost: u32,
        p_cost: u32,
    ) -> Result<BackupKey> {
        if passphrase.chars().count() < 12 {
            bail!("the backup passphrase must have at least 12 characters");
        }
        let params = Params::new(m_cost, t_cost, p_cost, Some(32))
            .map_err(|e| anyhow::anyhow!("invalid key derivation parameters: {e}"))?;
        let mut key = [0u8; 32];
        Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
            .hash_password_into(passphrase.as_bytes(), &salt, &mut key)
            .map_err(|e| anyhow::anyhow!("key derivation failed: {e}"))?;
        Ok(BackupKey {
            key,
            salt,
            m_cost,
            t_cost,
            p_cost,
        })
    }

    /// A value that tells a wrong passphrase apart from a damaged file
    /// (the key itself is 256 random-looking bits, so this reveals nothing).
    fn check(&self) -> String {
        let mut h = Sha256::new();
        h.update(b"teamcraft-backup-key-check");
        h.update(self.key);
        hex::encode(h.finalize())
    }

    /// The `encryption` object of the manifest.
    pub fn manifest_entry(&self) -> Value {
        json!({
            "cipher": "xchacha20poly1305-stream-be32",
            "kdf": "argon2id",
            "salt": hex::encode(self.salt),
            "m_cost": self.m_cost,
            "t_cost": self.t_cost,
            "p_cost": self.p_cost,
            "key_check": self.check(),
        })
    }

    /// Derives the key of an existing backup from its manifest entry and
    /// checks the passphrase.
    pub fn from_manifest(passphrase: &str, entry: &Value) -> Result<BackupKey> {
        let field = |name: &str| entry.get(name).context(format!("encryption: no '{name}'"));
        if field("cipher")?.as_str() != Some("xchacha20poly1305-stream-be32")
            || field("kdf")?.as_str() != Some("argon2id")
        {
            bail!("the backup is encrypted with an unknown method");
        }
        let salt: [u8; 16] = hex::decode(field("salt")?.as_str().unwrap_or(""))
            .ok()
            .and_then(|s| s.try_into().ok())
            .context("encryption: invalid 'salt'")?;
        let cost = |name: &str| -> Result<u32> {
            field(name)?
                .as_u64()
                .and_then(|v| u32::try_from(v).ok())
                .filter(|v| *v > 0 && *v <= 4 * 1024 * 1024)
                .with_context(|| format!("encryption: invalid '{name}'"))
        };
        let key = Self::derive(
            passphrase,
            salt,
            cost("m_cost")?,
            cost("t_cost")?,
            cost("p_cost")?,
        )?;
        if Some(key.check().as_str()) != field("key_check")?.as_str() {
            bail!("wrong backup passphrase");
        }
        Ok(key)
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(GenericArray::from_slice(&self.key))
    }
}

/// Reads until `buf` is full or the input ends; returns the bytes read.
fn fill(reader: &mut impl Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match reader.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Encrypts everything `plain` yields into `out`; returns the SHA-256 of the
/// plain content and its length.
pub fn encrypt(
    key: &BackupKey,
    plain: &mut impl Read,
    out: &mut impl Write,
) -> Result<(String, u64)> {
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    out.write_all(MAGIC)?;
    out.write_all(&nonce)?;
    let mut enc = EncryptorBE32::from_aead(key.cipher(), GenericArray::from_slice(&nonce));
    let mut hasher = Sha256::new();
    let mut total = 0u64;
    let mut cur = vec![0u8; CHUNK];
    let mut next = vec![0u8; CHUNK];
    let mut cur_len = fill(plain, &mut cur)?;
    loop {
        hasher.update(&cur[..cur_len]);
        total += cur_len as u64;
        // look ahead: the chunk is the last one when nothing follows
        let next_len = if cur_len == CHUNK {
            fill(plain, &mut next)?
        } else {
            0
        };
        if next_len == 0 {
            let sealed = enc
                .encrypt_last(&cur[..cur_len])
                .map_err(|_| anyhow::anyhow!("encryption failed"))?;
            out.write_all(&sealed)?;
            break;
        }
        let sealed = enc
            .encrypt_next(&cur[..cur_len])
            .map_err(|_| anyhow::anyhow!("encryption failed"))?;
        out.write_all(&sealed)?;
        std::mem::swap(&mut cur, &mut next);
        cur_len = next_len;
    }
    out.flush()?;
    Ok((hex::encode(hasher.finalize()), total))
}

/// Decrypts `sealed` into `out` (any authentication failure, truncation or
/// extra data is an error); returns the SHA-256 of the plain content.
pub fn decrypt(key: &BackupKey, sealed: &mut impl Read, out: &mut impl Write) -> Result<String> {
    let mut header = [0u8; 8 + NONCE_LEN];
    if fill(sealed, &mut header)? != header.len() || &header[..8] != MAGIC {
        bail!("not an encrypted TeamCraft backup file");
    }
    let mut dec = DecryptorBE32::from_aead(key.cipher(), GenericArray::from_slice(&header[8..]));
    let mut hasher = Sha256::new();
    let mut cur = vec![0u8; CHUNK + TAG];
    let mut next = vec![0u8; CHUNK + TAG];
    let mut cur_len = fill(sealed, &mut cur)?;
    let damaged = || anyhow::anyhow!("the file is damaged or was changed (authentication failed)");
    loop {
        if cur_len < TAG {
            return Err(damaged());
        }
        let next_len = if cur_len == CHUNK + TAG {
            fill(sealed, &mut next)?
        } else {
            0
        };
        if next_len == 0 {
            let plain = dec.decrypt_last(&cur[..cur_len]).map_err(|_| damaged())?;
            hasher.update(&plain);
            out.write_all(&plain)?;
            break;
        }
        let plain = dec.decrypt_next(&cur[..cur_len]).map_err(|_| damaged())?;
        hasher.update(&plain);
        out.write_all(&plain)?;
        std::mem::swap(&mut cur, &mut next);
        cur_len = next_len;
    }
    out.flush()?;
    Ok(hex::encode(hasher.finalize()))
}

/// Encrypts the file `source` into the new file `target`.
pub fn encrypt_file(key: &BackupKey, source: &Path, target: &Path) -> Result<(String, u64)> {
    let mut input = std::io::BufReader::new(
        fs::File::open(source).with_context(|| format!("cannot read '{}'", source.display()))?,
    );
    let mut output = std::io::BufWriter::new(
        fs::File::create_new(target)
            .with_context(|| format!("cannot create '{}'", target.display()))?,
    );
    let result = encrypt(key, &mut input, &mut output)
        .with_context(|| format!("cannot encrypt '{}'", source.display()))?;
    output
        .into_inner()
        .map_err(|e| anyhow::anyhow!("cannot write '{}': {e}", target.display()))?
        .sync_all()?;
    Ok(result)
}

/// Decrypts the file `source` into `out`; returns the plain SHA-256.
pub fn decrypt_file(key: &BackupKey, source: &Path, out: &mut impl Write) -> Result<String> {
    let mut input = std::io::BufReader::new(
        fs::File::open(source).with_context(|| format!("cannot read '{}'", source.display()))?,
    );
    decrypt(key, &mut input, out).with_context(|| format!("'{}'", source.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick_key(pass: &str) -> BackupKey {
        // cheap parameters for the tests
        BackupKey::derive(pass, [7; 16], 64, 1, 1).unwrap()
    }

    fn roundtrip(len: usize) {
        let key = quick_key("correct horse battery");
        let plain: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
        let mut sealed = Vec::new();
        let (sha, n) = encrypt(&key, &mut plain.as_slice(), &mut sealed).unwrap();
        assert_eq!(n, len as u64);
        assert_eq!(sha, hex::encode(Sha256::digest(&plain)));
        if len > 0 {
            assert_ne!(&sealed[27..27 + len.min(16)], &plain[..len.min(16)]);
        }
        let mut back = Vec::new();
        assert_eq!(
            decrypt(&key, &mut sealed.as_slice(), &mut back).unwrap(),
            sha
        );
        assert_eq!(back, plain, "{len} bytes");
    }

    #[test]
    fn roundtrips_at_chunk_boundaries() {
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK, 3 * CHUNK + 7] {
            roundtrip(len);
        }
    }

    #[test]
    fn tampering_truncation_and_wrong_keys_fail() {
        let key = quick_key("correct horse battery");
        let plain = vec![42u8; 2 * CHUNK + 10];
        let mut sealed = Vec::new();
        encrypt(&key, &mut plain.as_slice(), &mut sealed).unwrap();
        let mut sink = Vec::new();
        let mut flipped = sealed.clone();
        flipped[100] ^= 1;
        assert!(decrypt(&key, &mut flipped.as_slice(), &mut sink).is_err());
        // a whole chunk cut off: the new last chunk was not sealed as last
        let cut = &sealed[..27 + CHUNK + TAG];
        assert!(decrypt(&key, &mut &cut[..], &mut Vec::new()).is_err());
        let mut longer = sealed.clone();
        longer.extend_from_slice(&[0; 20]);
        assert!(decrypt(&key, &mut longer.as_slice(), &mut Vec::new()).is_err());
        let other = quick_key("another passphrase!");
        assert!(decrypt(&other, &mut sealed.as_slice(), &mut Vec::new()).is_err());
        assert!(decrypt(&key, &mut &b"plain dump"[..], &mut Vec::new()).is_err());
    }

    #[test]
    fn manifest_entry_checks_the_passphrase() {
        let key = quick_key("correct horse battery");
        let entry = key.manifest_entry();
        assert_eq!(entry["kdf"], "argon2id");
        assert!(BackupKey::from_manifest("correct horse battery", &entry).is_ok());
        let wrong = BackupKey::from_manifest("wrong horse battery!", &entry);
        assert!(wrong
            .err()
            .unwrap()
            .to_string()
            .contains("wrong backup passphrase"));
        assert!(BackupKey::new("short").is_err(), "at least 12 characters");
        let mut tampered = entry.clone();
        tampered["m_cost"] = json!(0);
        assert!(BackupKey::from_manifest("correct horse battery", &tampered).is_err());
    }
}
