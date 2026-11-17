//! Sync the web console into the crate so `include_dir!` works both locally
//! and inside the crates.io package (which cannot reference parent dirs).
//!
//! The destination is cleared before copying: vite emits content-hashed
//! asset names, so merge-copying would accumulate every historical bundle
//! in git and in the embedded binary.
use std::fs;
use std::path::{Path, PathBuf};

fn copy_tree(src: &Path, dst: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else {
            fs::copy(entry.path(), &to)?;
        }
    }
    Ok(())
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let src = manifest
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("packages/console/dist");
    let dst = manifest.join("console-dist");
    println!(
        "cargo:rerun-if-changed={}",
        src.join("index.html").display()
    );
    println!("cargo:rerun-if-changed={}", src.join("assets").display());
    if src.join("index.html").is_file() {
        if dst.exists() {
            // hashed names change every build: a merge would keep stale
            // bundles embedded forever
            if let Err(e) = fs::remove_dir_all(&dst) {
                panic!("console-dist cleanup failed: {e}");
            }
        }
        if let Err(e) = copy_tree(&src, &dst) {
            // a half-synced embed ships a broken console; fail the build
            panic!("console sync failed: {e}");
        }
    }
}
