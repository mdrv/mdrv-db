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

    /// Strict hash form: 64 lowercase hex chars (sha256 hex, as produced
    /// by [`BlobStore::hash`]). Guards filesystem paths in delete paths.
    pub fn valid_hash(hash_hex: &str) -> bool {
        hash_hex.len() == 64
            && hash_hex
                .as_bytes()
                .iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
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

    /// Begin an incremental upload: chunks stream into a temp file under
    /// `.staging` while sha256 runs alongside. finish() promotes the temp
    /// file straight into the CAS path. Crash-safe: leftovers are swept
    /// at open (sweep_staging). Dedup: if the final blob already exists,
    /// finish() just discards the temp file.
    pub fn upload_begin(&self, id: &str) -> io::Result<BlobUpload> {
        let p = self.root.join(".staging").join(format!("upload-{id}"));
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::File::create(&p)?;
        Ok(BlobUpload {
            path: p,
            file,
            sha: Sha256::new(),
            written: 0,
        })
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

/// Incremental blob upload (streaming). Feed chunks with write(); finish()
/// computes the sha256, syncs, and promotes the temp file into the CAS.
/// Abandoned uploads (abort() or process death) leave a `.staging/upload-*`
/// file that sweep_staging removes at the next open.
pub struct BlobUpload {
    path: PathBuf,
    file: fs::File,
    sha: Sha256,
    written: u64,
}

impl BlobUpload {
    pub fn write(&mut self, chunk: &[u8]) -> io::Result<()> {
        use std::io::Write;
        self.file.write_all(chunk)?;
        self.sha.update(chunk);
        self.written += chunk.len() as u64;
        Ok(())
    }

    pub fn written(&self) -> u64 {
        self.written
    }

    /// Finalize: sync + rename temp → `blobs/<hex[0..2]>/<hash>`.
    /// Returns (hash_hex, bytes_written). Idempotent under dedup.
    pub fn finish(self, store: &BlobStore) -> io::Result<(String, u64)> {
        let Self {
            path,
            mut file,
            sha,
            written,
        } = self;
        use std::io::Write;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        let hash = hex(&sha.finalize());
        let final_p = store.final_path(&hash);
        if final_p.is_file() {
            let _ = fs::remove_file(&path);
        } else {
            if let Some(parent) = final_p.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::rename(&path, &final_p)?;
            if let Some(p) = final_p.parent() {
                sync_dir(p)?;
            }
        }
        Ok((hash, written))
    }

    /// Explicit abandon: delete the temp file now.
    pub fn abort(self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn hex(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len() * 2);
    for byte in b {
        s.push_str(&format!("{byte:02x}"));
    }
    s
}
