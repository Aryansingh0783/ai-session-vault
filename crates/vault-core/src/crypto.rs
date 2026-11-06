//! Vault encryption. Everything stored in the vault (session logs, configs, manifests) is
//! compressed then sealed with XChaCha20-Poly1305. A random *master key* is wrapped by one or more
//! "slots" (passphrase via Argon2id, or a recovery key), so the passphrase can be changed or
//! recovered without re-encrypting any data.

use anyhow::{anyhow, bail, Result};
use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{aead::{Aead, KeyInit, Payload}, XChaCha20Poly1305, XNonce};
use data_encoding::BASE32_NOPAD;
use flate2::{read::DeflateDecoder, write::DeflateEncoder, Compression};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use zeroize::Zeroize;

pub const FORMAT: u32 = 2;
pub const MIN_PASSPHRASE: usize = 12;
const NONCE: usize = 24;

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct KdfParams {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Default for KdfParams {
    fn default() -> Self {
        Self { m_kib: 64 * 1024, t: 3, p: 1 }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Slot {
    /// "passphrase" or "recovery"
    pub kind: String,
    pub salt: String,
    pub wrapped: String,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Header {
    pub format: u32,
    pub vault_id: String,
    pub created: String,
    pub kdf: KdfParams,
    pub slots: Vec<Slot>,
}

/// Subkeys derived from the master key. Wiped on drop.
pub struct Keys {
    master: [u8; 32],
    enc: [u8; 32],
    id: [u8; 32],
}

impl Drop for Keys {
    fn drop(&mut self) {
        self.master.zeroize();
        self.enc.zeroize();
        self.id.zeroize();
    }
}

fn b64(b: &[u8]) -> String {
    data_encoding::BASE64URL_NOPAD.encode(b)
}
fn unb64(s: &str) -> Result<Vec<u8>> {
    data_encoding::BASE64URL_NOPAD.decode(s.as_bytes()).map_err(|e| anyhow!("bad base64: {e}"))
}

fn kek_from_passphrase(pass: &str, salt: &[u8], k: &KdfParams) -> Result<[u8; 32]> {
    let params = Params::new(k.m_kib, k.t, k.p, Some(32)).map_err(|e| anyhow!("kdf params: {e}"))?;
    let mut out = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(pass.as_bytes(), salt, &mut out)
        .map_err(|e| anyhow!("kdf: {e}"))?;
    Ok(out)
}

pub fn kek_from_enrollment(raw: &[u8]) -> [u8; 32] {
    blake3::derive_key("ai-session-vault v2 enrollment kek", raw)
}

pub fn kek_from_device(raw: &[u8]) -> [u8; 32] {
    blake3::derive_key("ai-session-vault v2 device kek", raw)
}

fn kek_from_recovery(raw: &[u8]) -> [u8; 32] {
    blake3::derive_key("ai-session-vault v2 recovery kek", raw)
}

/// AEAD seal: returns nonce || ciphertext.
pub fn seal(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let c = XChaCha20Poly1305::new(key.into());
    let mut nonce = [0u8; NONCE];
    OsRng.fill_bytes(&mut nonce);
    let ct = c
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .expect("encryption cannot fail");
    let mut out = nonce.to_vec();
    out.extend(ct);
    out
}

pub fn open(key: &[u8; 32], aad: &[u8], blob: &[u8]) -> Result<Vec<u8>> {
    if blob.len() < NONCE + 16 {
        bail!("object too short");
    }
    let c = XChaCha20Poly1305::new(key.into());
    c.decrypt(XNonce::from_slice(&blob[..NONCE]), Payload { msg: &blob[NONCE..], aad })
        .map_err(|_| anyhow!("authentication failed (wrong key or tampered data)"))
}

fn new_master() -> [u8; 32] {
    let mut m = [0u8; 32];
    OsRng.fill_bytes(&mut m);
    m
}

/// Human friendly recovery key, e.g. `ABCD-EFGH-...` (shown once at init).
pub fn format_code(raw: &[u8]) -> String {
    let s = BASE32_NOPAD.encode(raw);
    s.as_bytes().chunks(4).map(|c| std::str::from_utf8(c).unwrap()).collect::<Vec<_>>().join("-")
}

pub fn parse_code(s: &str) -> Result<Vec<u8>> {
    let clean: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_uppercase();
    BASE32_NOPAD.decode(clean.as_bytes()).map_err(|_| anyhow!("that doesn't look like a recovery key"))
}

impl Header {
    /// Create a new vault header. Returns `(header, keys, recovery_key)`.
    pub fn create(passphrase: &str, kdf: KdfParams) -> Result<(Header, Keys, String)> {
        if passphrase.chars().count() < MIN_PASSPHRASE {
            bail!("passphrase must be at least {MIN_PASSPHRASE} characters");
        }
        let master = new_master();
        let mut h = Header {
            format: FORMAT,
            vault_id: hex::encode(&new_master()[..8]),
            created: crate::util::now_iso(),
            kdf,
            slots: vec![],
        };
        h.add_passphrase_slot(&master, passphrase)?;
        let mut rec = [0u8; 32];
        OsRng.fill_bytes(&mut rec);
        let recovery = format_code(&rec);
        let kek = kek_from_recovery(&rec);
        h.slots.push(Slot { kind: "recovery".into(), salt: String::new(), wrapped: b64(&seal(&kek, b"slot:recovery", &master)) });
        rec.zeroize();
        Ok((h, Keys::from_master(master), recovery))
    }

    fn add_passphrase_slot(&mut self, master: &[u8; 32], pass: &str) -> Result<()> {
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        let mut kek = kek_from_passphrase(pass, &salt, &self.kdf)?;
        let wrapped = seal(&kek, b"slot:passphrase", master);
        kek.zeroize();
        self.slots.retain(|s| s.kind != "passphrase");
        self.slots.push(Slot { kind: "passphrase".into(), salt: b64(&salt), wrapped: b64(&wrapped) });
        Ok(())
    }

    /// Unlock with the passphrase, or with the recovery key when `secret` parses as one.
    pub fn unlock(&self, secret: &str) -> Result<Keys> {
        if self.format != FORMAT {
            bail!("unsupported vault format {}", self.format);
        }
        for s in self.slots.iter().filter(|s| s.kind == "passphrase") {
            let mut kek = kek_from_passphrase(secret, &unb64(&s.salt)?, &self.kdf)?;
            let r = open(&kek, b"slot:passphrase", &unb64(&s.wrapped)?);
            kek.zeroize();
            if let Ok(m) = r {
                return Ok(Keys::from_master(m.try_into().map_err(|_| anyhow!("corrupt slot"))?));
            }
        }
        if let Ok(raw) = parse_code(secret) {
            let kek = kek_from_recovery(&raw);
            for s in self.slots.iter().filter(|s| s.kind == "recovery") {
                if let Ok(m) = open(&kek, b"slot:recovery", &unb64(&s.wrapped)?) {
                    return Ok(Keys::from_master(m.try_into().map_err(|_| anyhow!("corrupt slot"))?));
                }
            }
        }
        bail!("wrong passphrase or recovery key")
    }

    /// Replace the passphrase slot. The data is untouched because only the master key is re-wrapped.
    pub fn change_passphrase(&mut self, keys: &Keys, new_pass: &str) -> Result<()> {
        if new_pass.chars().count() < MIN_PASSPHRASE {
            bail!("passphrase must be at least {MIN_PASSPHRASE} characters");
        }
        let m = keys.master;
        self.add_passphrase_slot(&m, new_pass)
    }

    /// Issue a fresh recovery key (invalidates the old one).
    pub fn rotate_recovery(&mut self, keys: &Keys) -> String {
        let mut rec = [0u8; 32];
        OsRng.fill_bytes(&mut rec);
        let kek = kek_from_recovery(&rec);
        self.slots.retain(|s| s.kind != "recovery");
        self.slots.push(Slot { kind: "recovery".into(), salt: String::new(), wrapped: b64(&seal(&kek, b"slot:recovery", &keys.master)) });
        let out = format_code(&rec);
        rec.zeroize();
        out
    }
}

impl Keys {
    /// Wrap the master key under another key (enrollment codes, device keys).
    pub fn wrap_master(&self, kek: &[u8; 32], aad: &str) -> String {
        b64(&seal(kek, aad.as_bytes(), &self.master))
    }

    pub fn from_wrapped(kek: &[u8; 32], aad: &str, wrapped: &str) -> Result<Keys> {
        let m = open(kek, aad.as_bytes(), &unb64(wrapped)?)?;
        Ok(Keys::from_master(m.try_into().map_err(|_| anyhow!("corrupt slot"))?))
    }

    fn from_master(master: [u8; 32]) -> Keys {
        Keys {
            enc: blake3::derive_key("ai-session-vault v2 object encryption", &master),
            id: blake3::derive_key("ai-session-vault v2 object id", &master),
            master,
        }
    }

    /// Keyed content id: equal plaintext -> equal id inside one vault, unlinkable across vaults.
    pub fn object_id(&self, plaintext: &[u8]) -> String {
        blake3::keyed_hash(&self.id, plaintext).to_hex().to_string()
    }

    pub fn hasher(&self) -> blake3::Hasher {
        blake3::Hasher::new_keyed(&self.id)
    }

    /// Compress then encrypt. `aad` binds the blob to its name (so blobs can't be swapped).
    pub fn pack(&self, aad: &str, plaintext: &[u8]) -> Result<Vec<u8>> {
        let mut e = DeflateEncoder::new(Vec::new(), Compression::fast());
        e.write_all(plaintext)?;
        Ok(seal(&self.enc, aad.as_bytes(), &e.finish()?))
    }

    pub fn unpack(&self, aad: &str, blob: &[u8]) -> Result<Vec<u8>> {
        let comp = open(&self.enc, aad.as_bytes(), blob)?;
        let mut out = Vec::new();
        DeflateDecoder::new(&comp[..]).read_to_end(&mut out)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast() -> KdfParams {
        KdfParams { m_kib: 64, t: 1, p: 1 }
    }

    #[test]
    fn passphrase_and_recovery_both_unlock() {
        let (h, k, rec) = Header::create("correct horse battery", fast()).unwrap();
        let blob = k.pack("x", b"hello hello hello").unwrap();
        let k2 = h.unlock("correct horse battery").unwrap();
        assert_eq!(k2.unpack("x", &blob).unwrap(), b"hello hello hello");
        let k3 = h.unlock(&rec.to_lowercase()).unwrap();
        assert_eq!(k3.unpack("x", &blob).unwrap(), b"hello hello hello");
        assert!(h.unlock("wrong passphrase!!").is_err());
    }

    #[test]
    fn aad_prevents_swapping_blobs() {
        let (_, k, _) = Header::create("correct horse battery", fast()).unwrap();
        let blob = k.pack("a", b"data").unwrap();
        assert!(k.unpack("b", &blob).is_err());
    }

    #[test]
    fn changing_passphrase_keeps_data_readable() {
        let (mut h, k, _) = Header::create("correct horse battery", fast()).unwrap();
        let blob = k.pack("x", b"data").unwrap();
        h.change_passphrase(&k, "another long passphrase").unwrap();
        assert!(h.unlock("correct horse battery").is_err());
        assert_eq!(h.unlock("another long passphrase").unwrap().unpack("x", &blob).unwrap(), b"data");
    }

    #[test]
    fn short_passphrase_rejected() {
        assert!(Header::create("short", fast()).is_err());
    }
}
