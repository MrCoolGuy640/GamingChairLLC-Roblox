//! Build script: packs `resources/` into the executable.
//!
//! The UI is shipped *inside* the binary, so a release build is a single
//! self-contained `.exe` (no `resources` folder next to it).
//!
//! Steps:
//!
//!   1. copy `resources/` to a temporary staging folder in `$OUT_DIR`;
//!   2. patch build-specific placeholders (the status bar's Developer/Release
//!      label, the version number) in the staged copy;
//!   3. pack the staged tree into `$OUT_DIR/resources.bin`;
//!   4. delete the staging folder.
//!
//! `src/bundle.rs` then embeds that one blob with `include_bytes!`. Keeping the
//! packed output as a single file is what makes step 4 possible: `include_bytes!`
//! is evaluated by the compiler *after* this script returns, so the compiler
//! must not depend on the staging folder still existing.

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Bumped whenever the blob layout changes.
const FORMAT_VERSION: u32 = 1;
const MAGIC: &[u8; 4] = b"GCRS";

fn main() {
    println!("cargo:rerun-if-changed=resources");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));

    let source = manifest_dir.join("resources");
    let staging = out_dir.join("bundled-resources");
    let blob = out_dir.join("resources.bin");

    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".into());
    let build_type = if profile == "release" { "Release" } else { "Developer" };
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());

    // Expose the same label to the Rust code (status bar / logs).
    println!("cargo:rustc-env=GC_BUILD_TYPE={build_type}");

    if staging.exists() {
        fs::remove_dir_all(&staging).expect("could not clear the resource staging folder");
    }
    fs::create_dir_all(&staging).expect("could not create the resource staging folder");

    copy_dir(&source, &staging);
    patch_build_placeholders(&staging.join("web").join("index.html"), &version, build_type);

    let mut entries = Vec::new();
    collect_files(&staging, &staging, &mut entries);
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    write_blob(&blob, &entries);

    // The staging copy has been packed; it is not needed any more.
    fs::remove_dir_all(&staging).expect("could not remove the resource staging folder");
}

/// Recursively copy `from` into `to`, preserving the directory structure.
fn copy_dir(from: &Path, to: &Path) {
    let entries = fs::read_dir(from)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", from.display()));

    for entry in entries {
        let entry = entry.expect("could not read a resource entry");
        let source = entry.path();
        let target = to.join(entry.file_name());

        if source.is_dir() {
            fs::create_dir_all(&target)
                .unwrap_or_else(|err| panic!("could not create {}: {err}", target.display()));
            copy_dir(&source, &target);
        } else {
            fs::copy(&source, &target).unwrap_or_else(|err| {
                panic!(
                    "could not copy {} to {}: {err}",
                    source.display(),
                    target.display()
                )
            });
        }
    }
}

/// Replace the build-time placeholders in the staged HTML.
fn patch_build_placeholders(path: &Path, version: &str, build_type: &str) {
    let html = fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));

    let patched = html
        .replace("__APP_VERSION__", version)
        .replace("__BUILD_TYPE__", build_type);

    fs::write(path, patched)
        .unwrap_or_else(|err| panic!("could not write {}: {err}", path.display()));
}

/// Collect `(relative/path, absolute path)` pairs. Relative paths always use
/// forward slashes: they are the keys the HTTP server looks up.
fn collect_files(root: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) {
    let entries = fs::read_dir(dir)
        .unwrap_or_else(|err| panic!("could not read {}: {err}", dir.display()));

    for entry in entries {
        let entry = entry.expect("could not read a resource entry");
        let path = entry.path();

        if path.is_dir() {
            collect_files(root, &path, out);
        } else {
            let relative = path
                .strip_prefix(root)
                .expect("resource path is inside the staging folder")
                .components()
                .map(|component| component.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            out.push((relative, path));
        }
    }
}

fn write_blob(path: &Path, entries: &[(String, PathBuf)]) {
    let mut file = fs::File::create(path)
        .unwrap_or_else(|err| panic!("could not create {}: {err}", path.display()));

    file.write_all(MAGIC).expect("could not write the blob header");
    file.write_all(&FORMAT_VERSION.to_le_bytes())
        .expect("could not write the blob header");
    file.write_all(&(entries.len() as u32).to_le_bytes())
        .expect("could not write the file count");

    for (relative, source) in entries {
        let data = fs::read(source)
            .unwrap_or_else(|err| panic!("could not read {}: {err}", source.display()));

        let name = relative.as_bytes();
        let name_len = u16::try_from(name.len())
            .unwrap_or_else(|_| panic!("resource path too long: {relative}"));

        file.write_all(&name_len.to_le_bytes())
            .expect("could not write a path length");
        file.write_all(name).expect("could not write a path");
        file.write_all(&(data.len() as u64).to_le_bytes())
            .expect("could not write a data length");
        file.write_all(&data).expect("could not write file data");
    }
}
