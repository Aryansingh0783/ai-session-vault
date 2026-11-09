//! Key material: sealing, opening and deriving object ids.

use super::*;

impl Keys {
    /// Wrap the master key under another key (enrollment codes, device keys).
    pub fn wrap_master(&self, kek: &[u8; 32], aad: &str) -> String {
        b64(&seal(kek, aad.as_bytes(), &self.master))
    }

    pub fn from_wrapped(kek: &[u8; 32], aad: &str, wrapped: &str) -> Result<Keys> {
        let m = open(kek, aad.as_bytes(), &unb64(wrapped)?)?;
        Ok(Keys::from_master(m.try_into().map_err(|_| anyhow!("corrupt slot"))?))
    }

    pub(super) fn from_master(master: [u8; 32]) -> Keys {
        Keys { enc: blake3::derive_key("ai-session-vault v2 object encryption", &master), id: blake3::derive_key("ai-session-vault v2 object id", &master), master }
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
