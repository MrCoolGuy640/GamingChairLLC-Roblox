//! The embedded copy of `resources/`.
//!
//! `build.rs` packs the resource tree (after the build-specific patches) into a
//! single blob and the compiler embeds it here. The HTTP server and the window
//! icon read from this module, so the executable never looks for a `resources`
//! folder on disk.
//!
//! Blob layout (little-endian):
//!
//! ```text
//! "GCRS" | u32 version | u32 file count
//! repeat: u16 path length | path (UTF-8, '/' separated) | u64 data length | data
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::sync::OnceLock;

static BLOB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/resources.bin"));

const MAGIC: &[u8; 4] = b"GCRS";
const FORMAT_VERSION: u32 = 1;
const ICON_PATH: &str = "icons/icon.ico";

static FILES: OnceLock<HashMap<&'static str, &'static [u8]>> = OnceLock::new();

/// Look up a bundled file by its path relative to `resources/` (for example
/// `web/index.html`). Unknown paths return `None`.
pub fn get(path: &str) -> Option<&'static [u8]> {
    files().get(path).copied()
}

/// Number of files in the bundle (used by one startup log line).
pub fn len() -> usize {
    files().len()
}

/// The window icon, if the build included one.
pub fn icon() -> Option<&'static [u8]> {
    get(ICON_PATH)
}

/// Write the embedded icon to `path` so Win32's `LoadImageW` can read it.
/// The file is only rewritten when it is missing or out of date.
pub fn write_icon(path: &Path) -> std::io::Result<()> {
    let Some(bytes) = icon() else {
        return Ok(());
    };

    if let Ok(existing) = fs::read(path) {
        if existing == bytes {
            return Ok(());
        }
    }

    fs::write(path, bytes)
}

fn files() -> &'static HashMap<&'static str, &'static [u8]> {
    FILES.get_or_init(|| parse().unwrap_or_else(|err| panic!("corrupt resource bundle: {err}")))
}

fn parse() -> Result<HashMap<&'static str, &'static [u8]>, String> {
    let mut reader = Reader::new(BLOB);

    if reader.take(4)? != MAGIC {
        return Err("bad magic".to_string());
    }

    let version = reader.u32()?;
    if version != FORMAT_VERSION {
        return Err(format!(
            "unsupported version {version} (expected {FORMAT_VERSION})"
        ));
    }

    let count = reader.u32()? as usize;
    let mut files = HashMap::with_capacity(count);

    for _ in 0..count {
        let name_len = reader.u16()? as usize;
        let name = std::str::from_utf8(reader.take(name_len)?)
            .map_err(|err| format!("invalid UTF-8 in a file name: {err}"))?;

        let data_len = reader.u64()? as usize;
        let data = reader.take(data_len)?;

        files.insert(name, data);
    }

    Ok(files)
}

struct Reader {
    data: &'static [u8],
    position: usize,
}

impl Reader {
    fn new(data: &'static [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'static [u8], String> {
        let end = self
            .position
            .checked_add(length)
            .ok_or_else(|| "length overflow".to_string())?;

        let slice = self
            .data
            .get(self.position..end)
            .ok_or_else(|| "unexpected end of bundle".to_string())?;

        self.position = end;
        Ok(slice)
    }

    fn u16(&mut self) -> Result<u16, String> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::{get, icon, len, parse};

    #[test]
    fn the_bundle_contains_the_interface() {
        assert!(len() > 100, "the bundle should contain the whole UI");

        let html = get("web/index.html").expect("web/index.html is bundled");
        let html = std::str::from_utf8(html).expect("index.html is UTF-8");

        assert!(html.contains("<title>GamingChairLLC</title>"));
        assert!(
            html.contains(env!("GC_BUILD_TYPE")),
            "the status bar should show the build type"
        );
        assert!(
            !html.contains("__BUILD_TYPE__") && !html.contains("__APP_VERSION__"),
            "build placeholders should have been replaced"
        );

        for path in [
            "web/css/style.css",
            "web/js/startup.js",
            "web/js/profiles.js",
            "icons/icon.ico",
        ] {
            assert!(get(path).is_some(), "{path} is bundled");
        }

        assert!(icon().is_some());
        assert!(parse().is_ok());
    }

    #[test]
    fn unknown_paths_are_not_served() {
        assert!(get("").is_none());
        assert!(get("../Cargo.toml").is_none());
        assert!(get("web/does-not-exist.js").is_none());
    }
}
