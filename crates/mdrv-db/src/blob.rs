//! Content-addressed blob store: `blobs/<hex[0..2]>/<hex>` final,
//! `blobs/.staging/<hex>` pre-commit. Same bytes ⇒ same path ⇒ idempotent.

use sha2::{Digest, Sha256};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(root.join(".staging"))?;
        Ok(BlobStore { root })
    }

    pub fn hash(bytes: &[u8]) -> String {
        let d = Sha256::digest(bytes);
        hex(&d)
    }

    pub fn final_path(&self, hash_hex: &str) -> PathBuf {
        self.root.join(&hash_hex[..2]).join(hash_hex)
    }

    pub fn staging_path(&self, hash_hex: &str) -> PathBuf {
        self.root.join(".staging").join(hash_hex)
    }

    /// Write bytes to staging (fsync). If final already exists (dedup),
    /// staging is skipped entirely.
    pub fn stage(&self, hash_hex: &str, bytes: &[u8]) -> io::Result<()> {
        let final_p = self.final_path(hash_hex);
        if final_p.is_file() {
            return Ok(());
        }
        let staging_p = self.staging_path(hash_hex);
        if let Some(parent) = staging_p.parent() {
            fs::create_dir_all(parent)?;
        }
        temp_write_and_sync(&staging_p, bytes)?;
        Ok(())
    }

    /// Atomic promote staging → final. Safe to call repeatedly.
    pub fn promote(&self, hash_hex: &str) -> io::Result<()> {
        let final_p = self.final_path(hash_hex);
        let staging_p = self.staging_path(hash_hex);
        if final_p.is_file() {
            let _ = fs::remove_file(&staging_p); // orphan from a crash mid-promote
            return Ok(());
        }
        if let Some(parent) = final_p.parent() {
            fs::create_dir_all(parent)?;
        }
        match fs::rename(&staging_p, &final_p) {
            Ok(()) => match final_p.parent() {
                Some(p) => sync_dir(p),
                None => Ok(()),
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()), // nothing staged (dedup hit earlier)
            Err(e) => Err(e),
        }
    }

    pub fn drop_blob(&self, hash_hex: &str) -> io::Result<()> {
        let _ = fs::remove_file(self.final_path(hash_hex));
        let _ = fs::remove_file(self.staging_path(hash_hex));
        Ok(())
    }

    /// Remove staged files (call only before any execute() in this boot).
    pub fn sweep_staging(&self) {
        if let Ok(rd) = fs::read_dir(self.root.join(".staging")) {
            for e in rd.flatten() {
                let _ = fs::remove_file(e.path());
            }
        }
    }

    /// Walk final blobs: yields (hash_from_filename, path). Skips staging.
    pub fn walk_final(&self) -> io::Result<Vec<(String, PathBuf)>> {
        let mut out = Vec::new();
        for e in fs::read_dir(&self.root)?.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') || !e.path().is_dir() {
                continue;
            }
            for f in fs::read_dir(e.path())?.flatten() {
                if let Some(h) = f.file_name().to_str() {
                    if h.len() == 64 {
                        out.push((h.to_string(), f.path()));
                    }
                }
            }
        }
        Ok(out)
    }
}

fn temp_write_and_sync(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut f = fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}

pub fn sync_dir(dir: impl AsRef<Path>) -> io::Result<()> {
    fs::File::open(dir)?.sync_all()
}

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}
