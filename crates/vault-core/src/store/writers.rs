//! Streaming writers: hash a file, or encrypt it into 1 MiB chunks, without holding it in memory.

use super::*;

/// Computes the content hash without storing anything (dry runs, change detection).
pub struct HashWriter {
    pub(super) hasher: blake3::Hasher,
    pub size: u64,
}

impl HashWriter {
    pub fn new(store: &Store) -> Self {
        HashWriter { hasher: store.keys.hasher(), size: 0 }
    }
    pub fn finish(self) -> (String, u64) {
        (self.hasher.finalize().to_hex().to_string(), self.size)
    }
}

impl Write for HashWriter {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.hasher.update(b);
        self.size += b.len() as u64;
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Splits a byte stream into encrypted chunk objects. Chunks the vault already has are not re-uploaded.
pub struct ChunkWriter<'a> {
    pub(super) store: &'a Store,
    pub(super) buf: Vec<u8>,
    pub(super) hasher: blake3::Hasher,
    pub(super) chunks: Vec<String>,
    pub(super) size: u64,
    pub(super) new_bytes: u64,
}

pub struct Stored {
    pub hash: String,
    pub chunks: Vec<String>,
    pub size: u64,
    /// Plaintext bytes of chunks that were new to the vault.
    pub new_bytes: u64,
}

impl<'a> ChunkWriter<'a> {
    pub fn new(store: &'a Store) -> Self {
        ChunkWriter { store, buf: Vec::with_capacity(CHUNK), hasher: store.keys.hasher(), chunks: vec![], size: 0, new_bytes: 0 }
    }

    pub(super) fn flush_chunk(&mut self, n: usize) -> io::Result<()> {
        let chunk: Vec<u8> = self.buf.drain(..n).collect();
        let id = self.store.keys.object_id(&chunk);
        if !self.store.has_object(&id) {
            self.store.put(&chunk).map_err(|e| io::Error::other(e.to_string()))?;
            self.new_bytes += n as u64;
        }
        self.chunks.push(id);
        Ok(())
    }

    pub fn finish(mut self) -> Result<Stored> {
        if !self.buf.is_empty() || self.chunks.is_empty() {
            let n = self.buf.len();
            self.flush_chunk(n)?;
        }
        let hash = self.hasher.finalize().to_hex().to_string();
        if self.chunks.len() == 1 {
            self.chunks.clear(); // the single chunk's id equals the whole-file hash
        }
        Ok(Stored { hash, chunks: self.chunks, size: self.size, new_bytes: self.new_bytes })
    }
}

impl Write for ChunkWriter<'_> {
    fn write(&mut self, b: &[u8]) -> io::Result<usize> {
        self.hasher.update(b);
        self.size += b.len() as u64;
        self.buf.extend_from_slice(b);
        while self.buf.len() >= CHUNK {
            self.flush_chunk(CHUNK)?;
        }
        Ok(b.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
