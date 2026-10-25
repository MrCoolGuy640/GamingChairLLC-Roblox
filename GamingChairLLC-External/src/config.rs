//! Central configuration: app identity, paths and tunables.

use std::path::{Path, PathBuf};

pub const APP_NAME: &str = "GamingChairLLC";
pub const APP_VERSION: &str = "0.1.0";
pub const BUILD_TYPE: &str = "Developer";

/// Enables the WebView2 devtools and verbose (DEBUG) logging.
pub const DEBUG: bool = true;

pub const ROBLOX_PROCESS_NAME: &str = "RobloxPlayerBeta.exe";
pub const OFFSETS_BASE_URL: &str = "https://offsets.imtheo.lol";
/// How long a downloaded offsets file is considered fresh.
pub const OFFSETS_REFRESH_SECS: u64 = 600;

/// Local resource server (serves ./resources to the webview).
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
#[derive(Clone, Debug)]
pub struct Paths {
    pub resources: PathBuf,
    pub icon_file: PathBuf,
    pub log_dir: PathBuf,
    pub log_file: PathBuf,
    pub data_dir: PathBuf,
    pub offsets_cache: PathBuf,
    pub offsets_version_file: PathBuf,
}

impl Paths {
    pub fn discover() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let exe = std::env::current_exe().ok();

        let resources = find_resources(&cwd, exe.as_deref());
        let project_dir = resources
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| cwd.clone());

        let log_dir = project_dir.join("logs");
        let data_dir = project_dir.join("data");

        Self {
            icon_file: resources.join("icons").join("icon.ico"),
            log_file: log_dir.join("gamingchairllc.log"),
            offsets_cache: data_dir.join("offsets.json"),
            offsets_version_file: data_dir.join("last_checked_offsets.txt"),
            resources,
            log_dir,
            data_dir,
        }
    }
}

/// Look for `resources/` next to the working directory first, then around the
/// executable (so `cargo run` and a copied release build both work).
fn find_resources(cwd: &Path, exe: Option<&Path>) -> PathBuf {
    let mut candidates = vec![cwd.join("resources")];

    if let Some(exe_dir) = exe.and_then(Path::parent) {
        candidates.push(exe_dir.join("resources"));
        candidates.push(exe_dir.join("..").join("resources"));
        candidates.push(exe_dir.join("..").join("..").join("resources"));
    }

    candidates
        .iter()
        .find(|candidate| candidate.is_dir())
        .cloned()
        .unwrap_or_else(|| cwd.join("resources"))
}
