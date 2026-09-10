//! Sync the web console into the crate so `include_dir!` works both locally
//! and inside the crates.io package (which cannot reference parent dirs).
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
        if let Err(e) = copy_tree(&src, &dst) {
            println!("cargo:warning=console sync failed: {e}");
        }
    }
}
