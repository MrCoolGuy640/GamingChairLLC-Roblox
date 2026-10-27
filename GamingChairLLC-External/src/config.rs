//! Central configuration: app identity, paths and tunables.

use std::path::PathBuf;

pub const APP_NAME: &str = "GamingChairLLC";
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// `Developer` for debug builds, `Release` for release builds. Supplied by
/// `build.rs` so it always matches the text patched into the UI.
pub const BUILD_TYPE: &str = env!("GC_BUILD_TYPE");

/// Enables verbose (DEBUG) logging.
pub const DEBUG: bool = true;

/// WebView2 devtools, browser accelerator keys (Ctrl+R, F12, ...) and the
/// right-click menu. Development builds only.
pub const DEVTOOLS: bool = cfg!(debug_assertions);

pub const ROBLOX_PROCESS_NAME: &str = "RobloxPlayerBeta.exe";
pub const OFFSETS_BASE_URL: &str = "https://offsets.imtheo.lol";
/// How long a downloaded offsets file is considered fresh.
pub const OFFSETS_REFRESH_SECS: u64 = 600;

/// Local resource server (serves the bundled UI to the webview).
pub const HOST: &str = "127.0.0.1";
/// Falls back to an OS-assigned free port when this one is taken.
pub const PORT: u16 = 8765;

pub const WINDOW_TITLE: &str = APP_NAME;
pub const WINDOW_WIDTH: f64 = 900.0;
pub const WINDOW_HEIGHT: f64 = 650.0;
pub const WINDOW_BG: (u8, u8, u8, u8) = (14, 14, 17, 255);

/// Backend loop rate. 0 means "uncapped" (uses a full core); 1000 Hz keeps the
/// loop responsive without wasting CPU. No built-in cheat needs more than that.
pub const LOOP_TARGET_HZ: u32 = 1000;

/// All filesystem locations the app needs.
///
/// Everything the app writes lives under `%LOCALAPPDATA%\GamingChairLLC` so a
/// build can be copied anywhere (or run from a read-only folder) without
/// mixing user data into the executable's directory.
#[derive(Clone, Debug)]
pub struct Paths {
    /// `%LOCALAPPDATA%\GamingChairLLC`.
    pub app_data: PathBuf,
    pub log_dir: PathBuf,
    pub log_file: PathBuf,
    /// Offsets cache (kept in a `data` subfolder of the app data directory).
    pub data_dir: PathBuf,
    pub offsets_cache: PathBuf,
    pub offsets_version_file: PathBuf,
}

impl Paths {
    pub fn discover() -> Self {
        let app_data = local_app_data().join(APP_NAME);

        let log_dir = app_data.join("logs");
        let data_dir = app_data.join("data");

        Self {
            log_file: log_dir.join("gamingchairllc.log"),
            offsets_cache: data_dir.join("offsets.json"),
            offsets_version_file: data_dir.join("last_checked_offsets.txt"),
            app_data,
            log_dir,
            data_dir,
        }
    }

    /// Create the writable directories. Safe to call repeatedly.
    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.app_data)?;
        std::fs::create_dir_all(&self.log_dir)?;
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(self.app_data.join("profiles"))?;
        Ok(())
    }
}

/// `%LOCALAPPDATA%`, falling back to `%USERPROFILE%\AppData\Local` and finally
/// the current directory. `paths` are resolved once, at startup.
fn local_app_data() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .map(PathBuf::from)
                .map(|profile| profile.join("AppData").join("Local"))
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}
