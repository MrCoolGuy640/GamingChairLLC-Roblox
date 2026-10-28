//! Dump / decompile actions for `ModuleScript` and `LocalScript` instances.
//!
//! Two flavours, for both actions:
//!
//! * **As…** (`ask = true`) — a native Windows save dialog anchors the output.
//! * **Automatic** (`ask = false`) — the file lands in the app data folder:
//!   `%LOCALAPPDATA%\GamingChairLLC\Workspace\Dumps\<name>_<date>.bin`
//!   `%LOCALAPPDATA%\GamingChairLLC\Workspace\Decompiles\<name>_<date>.luau`
//!
//! A "dump" is the decrypted, decompressed Luau bytecode — byte-for-byte what
//! the Python tool uploads to lua.expert — while "decompile" runs it through the
//! in-process Tovek decompiler and writes the reconstructed Luau source.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};

use crate::config::Paths;
use crate::decompiler::{Decompiler, EncodeKey};
use crate::sdk::roblox::{Roblox, Script};

use super::explorer::parse_address;

/// Which file the action produces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Raw bytecode (`.bin`).
    Dump,
    /// Reconstructed Luau source (`.luau`).
    Decompile,
}

impl Action {
    /// Name used in logs and toasts.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Dump => "Dump",
            Self::Decompile => "Decompile",
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Dump => "bin",
            Self::Decompile => "luau",
        }
    }

    fn filter_label(self) -> &'static str {
        match self {
            Self::Dump => "Luau bytecode",
            Self::Decompile => "Luau source",
        }
    }

    fn directory(self, paths: &Paths) -> &Path {
        match self {
            Self::Dump => &paths.dumps_dir,
            Self::Decompile => &paths.decompiles_dir,
        }
    }
}

/// What happened, so the UI can tell "user cancelled" apart from "it worked".
#[derive(Debug)]
pub enum Outcome {
    Written { path: PathBuf, bytes: usize },
    Cancelled,
}

/// Resolve `address`, read the script's bytecode and write the requested file.
///
/// `hwnd` is only used for the save dialog; `0` still works (the dialog is then
/// owned by the desktop rather than the app window).
pub fn run(
    roblox: &Roblox,
    address: &str,
    action: Action,
    ask: bool,
    hwnd: isize,
    paths: &Paths,
) -> Result<Outcome> {
    let address = parse_address(address).context("invalid instance address")?;
    let script = Script::new(roblox.instance(address))?;

    let name = script.name();
    let stem = format!("{}_{}", sanitize(&name), timestamp());
    let dir = action.directory(paths);
    std::fs::create_dir_all(dir)
        .with_context(|| format!("could not create {}", dir.display()))?;

    let path = if ask {
        let default_name = format!("{stem}.{}", action.extension());
        match save_dialog(hwnd, action, &default_name, dir)? {
            Some(path) => path,
            None => {
                log::debug!(
                    target: "GamingChairLLC.scripts",
                    "{} cancelled for '{}'",
                    action.verb(),
                    name
                );
                return Ok(Outcome::Cancelled);
            }
        }
    } else {
        unique_path(dir, &stem, action.extension())
    };

    // Read the bytecode once; the dump writes it as-is, the decompiler consumes
    // exactly the same bytes.
    let bytecode = script.bytecode()?;

    let data = match action {
        Action::Dump => bytecode,
        Action::Decompile => {
            let decompiler = Decompiler::builder()
                .encode_key(EncodeKey::Auto)
                .build()
                .map_err(|err| anyhow!("could not start the decompiler: {err}"))?;

            decompiler
                .decompile_named(&bytecode, &name)
                .map_err(|err| anyhow!("decompilation failed: {err}"))?
                .into_bytes()
        }
    };

    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("could not create {}", parent.display()))?;
        }
    }

    std::fs::write(&path, &data)
        .with_context(|| format!("could not write {}", path.display()))?;

    log::info!(
        target: "GamingChairLLC.scripts",
        "{} of '{}' (0x{:x}) written to {} ({} bytes)",
        action.verb(),
        name,
        script.address(),
        path.display(),
        data.len()
    );

    Ok(Outcome::Written {
        path,
        bytes: data.len(),
    })
}

// ---------------------------------------------------------------------------
// Filenames
// ---------------------------------------------------------------------------

/// `2026-10-28_14-05-09`, local time (`chrono` is already a dependency).
fn timestamp() -> String {
    chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string()
}

/// Turn an arbitrary in-game name into one Windows accepts.
///
/// Reserved device names (`CON`, `NUL`, ...) are prefixed so the file can
/// actually be created — a script really can be called `CON`.
fn sanitize(name: &str) -> String {
    let mut cleaned = String::with_capacity(name.len());

    for ch in name.chars() {
        if ch.is_control() || matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') {
            cleaned.push('_');
        } else {
            cleaned.push(ch);
        }
    }

    let trimmed = cleaned.trim().trim_end_matches('.').trim();
    let mut result: String = trimmed.chars().take(80).collect();

    if result.is_empty() {
        result.push_str("Script");
    }

    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7",
        "COM8", "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if RESERVED
        .iter()
        .any(|reserved| result.eq_ignore_ascii_case(reserved))
    {
        result.insert(0, '_');
    }

    result
}

/// `<dir>\<stem>.<ext>`, with `_2`, `_3`, ... appended while it exists, so two
/// dumps of the same script in the same second never overwrite each other.
fn unique_path(dir: &Path, stem: &str, extension: &str) -> PathBuf {
    let mut candidate = dir.join(format!("{stem}.{extension}"));
    let mut counter = 2;

    while candidate.exists() {
        candidate = dir.join(format!("{stem}_{counter}.{extension}"));
        counter += 1;
    }

    candidate
}

// ---------------------------------------------------------------------------
// Save dialog
// ---------------------------------------------------------------------------

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Native `GetSaveFileNameW` prompt. `Ok(None)` means the user cancelled.
fn save_dialog(
    owner: isize,
    action: Action,
    default_name: &str,
    initial_dir: &Path,
) -> Result<Option<PathBuf>> {
    use windows_sys::Win32::UI::Controls::Dialogs::{
        CommDlgExtendedError, GetSaveFileNameW, OFN_EXPLORER, OFN_HIDEREADONLY, OFN_NOCHANGEDIR,
        OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };

    let extension = action.extension();
    let filter = format!(
        "{} (*.{extension})\0*.{extension}\0All files (*.*)\0*.*\0",
        action.filter_label()
    );
    let filter: Vec<u16> = filter.encode_utf16().chain(std::iter::once(0)).collect();
    let title = wide(&format!("{} As", action.verb()));
    let def_ext = wide(extension);
    let initial_dir = wide(&initial_dir.to_string_lossy());

    // MAX_PATH is not enough for long paths; the common dialog accepts more.
    let mut file = vec![0u16; 32 * 1024];
    let name: Vec<u16> = default_name.encode_utf16().collect();
    file[..name.len()].copy_from_slice(&name);

    let mut ofn = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner as *mut core::ffi::c_void,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrFile: file.as_mut_ptr(),
        nMaxFile: file.len() as u32,
        lpstrInitialDir: initial_dir.as_ptr(),
        lpstrTitle: title.as_ptr(),
        Flags: OFN_EXPLORER
            | OFN_OVERWRITEPROMPT
            | OFN_PATHMUSTEXIST
            | OFN_NOCHANGEDIR
            | OFN_HIDEREADONLY,
        lpstrDefExt: def_ext.as_ptr(),
        ..Default::default()
    };

    let accepted = unsafe { GetSaveFileNameW(&mut ofn) };
    if accepted == 0 {
        let code = unsafe { CommDlgExtendedError() };
        if code == 0 {
            return Ok(None);
        }
        bail!("the save dialog failed (error 0x{code:x})");
    }

    let end = file.iter().position(|&unit| unit == 0).unwrap_or(file.len());
    if end == 0 {
        return Ok(None);
    }

    let path = String::from_utf16_lossy(&file[..end]);
    Ok(Some(PathBuf::from(path)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitises_names_windows_rejects() {
        assert_eq!(sanitize("Foo/Bar:Baz"), "Foo_Bar_Baz");
        assert_eq!(sanitize("  spaced  "), "spaced");
        assert_eq!(sanitize("trailing..."), "trailing");
        assert_eq!(sanitize(""), "Script");
        assert_eq!(sanitize("CON"), "_CON");
    }

    #[test]
    fn keeps_normal_names_untouched() {
        assert_eq!(sanitize("MainModule"), "MainModule");
        assert_eq!(sanitize("Player Controller"), "Player Controller");
    }

    #[test]
    fn picks_a_free_filename() {
        let dir = std::env::temp_dir().join(format!("gc-scripts-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let _ = std::fs::remove_file(dir.join("a.bin"));

        assert_eq!(unique_path(&dir, "a", "bin"), dir.join("a.bin"));
        std::fs::write(dir.join("a.bin"), b"x").unwrap();
        assert_eq!(unique_path(&dir, "a", "bin"), dir.join("a_2.bin"));

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
