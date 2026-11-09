//! The vault header: passphrase and recovery slots around the master key.

use super::*;

impl Header {
    /// Create a new vault header. Returns `(header, keys, recovery_key)`.
    pub fn create(passphrase: &str, kdf: KdfParams) -> Result<(Header, Keys, String)> {
        if passphrase.chars().count() < MIN_PASSPHRASE {
            bail!("passphrase must be at least {MIN_PASSPHRASE} characters");
        }
        let master = new_master();
        let mut h = Header { format: FORMAT, vault_id: hex::encode(&new_master()[..8]), created: crate::util::now_iso(), kdf, slots: vec![] };
        h.add_passphrase_slot(&master, passphrase)?;
        let mut rec = [0u8; 32];
        OsRng.fill_bytes(&mut rec);
        let recovery = format_code(&rec);
        let kek = kek_from_recovery(&rec);
        h.slots.push(Slot { kind: "recovery".into(), salt: String::new(), wrapped: b64(&seal(&kek, b"slot:recovery", &master)) });
        rec.zeroize();
        Ok((h, Keys::from_master(master), recovery))
    }

    pub(super) fn add_passphrase_slot(&mut self, master: &[u8; 32], pass: &str) -> Result<()> {
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
