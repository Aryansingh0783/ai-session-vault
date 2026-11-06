//! AI Session Vault core: encrypted, conflict-free sync of AI assistant sessions between devices.
//! This crate has no UI; the CLI and the desktop app are thin layers on top of it.

pub mod archive;
pub mod crypto;
pub mod doctor;
pub mod engine;
pub mod env;
pub mod jsonl;
pub mod legacy;
pub mod paths;
pub mod secrets;
pub mod store;
pub mod sync;
pub mod util;
