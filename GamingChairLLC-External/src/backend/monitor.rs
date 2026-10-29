//! Roblox session monitors.
//!
//! Two independent signals feed the same state:
//!
//! * **Log tail** – the client writes one Player log per process. A join emits
//!   `! Joining game '<job>' place <placeId> at <ip>`; leaving returns to the
//!   Lua app with `returnToLuaApp:` / `Entered app session.`. The tail starts
//!   at the end of the newest log so historical lines never replay.
//! * **Live reads** – after a join the tail keeps polling `DataModel.PlaceId`
//!   and `GameLoaded` until the place is loaded. While the place is loaded it
//!   also samples the local humanoid to detect death and respawn.
//!
//! The monitor never acts on cheats itself: it returns *transitions* and the
//! backend loop decides what to do with them (see `worker.rs`).

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sdk::roblox::utils;
use crate::sdk::roblox::Roblox;

use super::state::{GamePhase, GameStatus, LifeState};

/// How often the loop samples the log and the local character.
pub const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// A character that disappears while the place is still loaded is only counted
/// as a death after this long. The log's leave line normally wins first.
const MISSING_DEBOUNCE: Duration = Duration::from_millis(300);

/// `Enum.HumanoidStateType.Dead`.
const DEAD_STATE_ID: i32 = 15;

/// One transition produced by a monitor tick.
#[derive(Clone, Copy, Debug, Default)]
pub struct MonitorTick {
    pub game_changed: bool,
    pub life_changed: bool,
}

impl MonitorTick {
    pub fn changed(self) -> bool {
        self.game_changed || self.life_changed
    }
}

// ---------------------------------------------------------------------------
// Log parsing
// ---------------------------------------------------------------------------

/// The only log lines the monitors care about.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogEvent {
    /// `! Joining game '<job>' place <place_id> at <ip>`.
    Joined { place_id: i64 },
    /// The client started returning to the Lua app / home screen.
    Left,
}

/// Parse one Player-log line. Continuation lines and noise return `None`.
pub fn parse_line(line: &str) -> Option<LogEvent> {
    if let Some((_, rest)) = line.split_once("! Joining game '") {
        if let Some((_, after)) = rest.split_once("' place ") {
            let place_id = after
                .split_whitespace()
                .next()
                .and_then(|digits| digits.parse::<i64>().ok());
            if let Some(place_id) = place_id {
                return Some(LogEvent::Joined { place_id });
            }
        }
    }

    // `returnToLuaAppInternal:` (startup) deliberately does not match: only the
    // real transition has a colon directly after `returnToLuaApp`. The very
    // first line of every Player log is `returnToLuaApp: (stage:
    // InitializedLuaApp).`, which is the app starting up, not a departure.
    if line.contains("returnToLuaApp:") && !line.contains("stage:InitializedLuaApp") {
        return Some(LogEvent::Left);
    }

    if line.contains("leaveUGCGameInternal") {
        return Some(LogEvent::Left);
    }

    if line.contains("Entered app session.") {
        return Some(LogEvent::Left);
    }

    None
}

// ---------------------------------------------------------------------------
// Log tail
// ---------------------------------------------------------------------------

/// Incremental reader for the Player log of the process we attached to.
pub struct LogTail {
    directory: Option<PathBuf>,
    path: Option<PathBuf>,
    position: u64,
    partial: Vec<u8>,
}

impl LogTail {
    /// Bind to the newest Player log and skip everything already in it.
    pub fn start_at_end() -> Self {
        let mut tail = Self {
            directory: log_directory(),
            path: None,
            position: 0,
            partial: Vec::new(),
        };
        tail.bind_newest(true);
        tail
    }

    /// The log file being followed, for diagnostics.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Read every line appended since the last call.
    pub fn poll(&mut self) -> Vec<LogEvent> {
        if self.path.is_none() {
            // No log at attach time: the first one that appears is ours, so
            // read it from the top and do not miss an early join line.
            self.bind_newest(false);
        }

        let Some(path) = self.path.clone() else {
            return Vec::new();
        };

        let size = match fs::metadata(&path) {
            Ok(metadata) => metadata.len(),
            Err(_) => {
                // Removed or replaced: try again with whatever is newest.
                self.path = None;
                self.position = 0;
                self.partial.clear();
                return Vec::new();
            }
        };

        if size < self.position {
            // Truncated or rewritten in place.
            self.position = 0;
            self.partial.clear();
        }
        if size == self.position {
            return Vec::new();
        }

        let mut data = Vec::new();
        let read = File::open(&path).and_then(|mut file| {
            file.seek(SeekFrom::Start(self.position))?;
            file.take(size - self.position).read_to_end(&mut data)
        });
        if read.is_err() || data.is_empty() {
            return Vec::new();
        }

        self.position += data.len() as u64;

        // Split on raw bytes so a multi-byte character split across two reads
        // is never decoded half-way.
        let mut buffer = std::mem::take(&mut self.partial);
        buffer.extend_from_slice(&data);

        let mut events = Vec::new();
        let mut start = 0;
        for (index, byte) in buffer.iter().enumerate() {
            if *byte != b'\n' {
                continue;
            }

            let line = &buffer[start..index];
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if let Some(event) = parse_line(&String::from_utf8_lossy(line)) {
                events.push(event);
            }
            start = index + 1;
        }
        self.partial = buffer[start..].to_vec();

        events
    }

    fn bind_newest(&mut self, start_at_end: bool) {
        let Some(directory) = self.directory.clone() else {
            return;
        };
        let Some(path) = newest_player_log(&directory) else {
            return;
        };

        let length = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);

        self.position = if start_at_end { length } else { 0 };
        self.partial.clear();
        self.path = Some(path);
    }
}

/// `%LOCALAPPDATA%\Roblox\logs`, with the Microsoft Store layout as a fallback.
fn log_directory() -> Option<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;

    let standard = local.join("Roblox").join("logs");
    if standard.is_dir() {
        return Some(standard);
    }

    let packages = local.join("Packages");
    if let Ok(entries) = fs::read_dir(&packages) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.starts_with("ROBLOXCORPORATION.ROBLOX_") {
                continue;
            }

            let logs = entry.path().join("LocalState").join("logs");
            if logs.is_dir() {
                return Some(logs);
            }
        }
    }

    Some(standard)
}

/// True for the client's own Player log, and false for the crash-handler
/// companion (`..._Player_<id>_CrashHandler_last.log`) that Roblox writes next
/// to it.
fn is_player_log(name: &str) -> bool {
    name.contains("_Player_") && name.ends_with(".log") && !name.contains("CrashHandler")
}

/// Newest `*_Player_*.log`, selecting by modification time.
fn newest_player_log(directory: &Path) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;

    for entry in fs::read_dir(directory).ok()?.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !is_player_log(name) {
            continue;
        }

        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

        if newest
            .as_ref()
            .map(|(seen, _)| modified >= *seen)
            .unwrap_or(true)
        {
            newest = Some((modified, path));
        }
    }

    newest.map(|(_, path)| path)
}

// ---------------------------------------------------------------------------
// Monitor
// ---------------------------------------------------------------------------

/// Combines the log tail with live reads into one `GameStatus`.
pub struct GameMonitor {
    log: LogTail,
    status: GameStatus,
    missing_since: Option<Instant>,
    /// True once the tail has reported a join or a leave. From then on the log
    /// is authoritative; live reads only finish a join the log already saw.
    saw_log_event: bool,
}

impl GameMonitor {
    /// Start from the live DataModel, so attaching mid-game needs no history.
    pub fn new(roblox: &Arc<Roblox>) -> Self {
        let place_id = roblox.place_id();
        let loaded = roblox.game_loaded();

        let phase = if loaded {
            GamePhase::InGame
        } else if place_id != 0 {
            GamePhase::Joining
        } else {
            GamePhase::Home
        };

        Self {
            log: LogTail::start_at_end(),
            status: GameStatus {
                phase,
                place_id,
                life: LifeState::Unknown,
            },
            missing_since: None,
            saw_log_event: false,
        }
    }

    pub fn status(&self) -> GameStatus {
        self.status
    }

    /// The log file being followed, for diagnostics.
    pub fn log_path(&self) -> Option<&Path> {
        self.log.path()
    }

    /// Advance the state machine. Cheap enough to call on a 100 ms cadence.
    pub fn poll(&mut self, roblox: &Arc<Roblox>) -> MonitorTick {
        let mut tick = MonitorTick::default();

        for event in self.log.poll() {
            self.saw_log_event = true;
            match event {
                LogEvent::Joined { place_id } => {
                    if self.status.phase != GamePhase::Joining
                        || self.status.place_id != place_id
                    {
                        self.status.phase = GamePhase::Joining;
                        self.status.place_id = place_id;
                        self.reset_life();
                        tick.game_changed = true;
                    }
                }
                LogEvent::Left => {
                    if self.status.phase != GamePhase::Home {
                        self.status.phase = GamePhase::Home;
                        self.status.place_id = 0;
                        self.reset_life();
                        tick.game_changed = true;
                    }
                }
            }
        }

        // A join is only finished once the DataModel says the place loaded.
        if self.status.phase == GamePhase::Joining && roblox.game_loaded() {
            let place_id = roblox.place_id();
            if place_id != 0 {
                self.status.place_id = place_id;
            }
            self.status.phase = GamePhase::InGame;
            self.reset_life();
            tick.game_changed = true;
        } else if self.status.phase == GamePhase::Home && !self.saw_log_event {
            // The tail starts at EOF, so a join line written just before the
            // app attached was never seen. With no log transition yet, trust a
            // live DataModel that already has a place.
            let place_id = roblox.place_id();
            if place_id != 0 {
                let next = if !roblox.game_loaded() {
                    Some(GamePhase::Joining)
                } else if roblox.get_local_character().is_some() {
                    Some(GamePhase::InGame)
                } else {
                    // Loaded but still spawning; wait for the character.
                    None
                };

                if let Some(phase) = next {
                    self.status.place_id = place_id;
                    self.status.phase = phase;
                    self.reset_life();
                    tick.game_changed = true;
                }
            }
        }

        if self.status.phase == GamePhase::InGame {
            let before = self.status.life;
            self.sample_life(roblox);
            if self.status.life != before {
                tick.life_changed = true;
            }
        } else if self.status.life != LifeState::Unknown {
            self.reset_life();
            tick.life_changed = true;
        }

        tick
    }

    fn reset_life(&mut self) {
        self.status.life = LifeState::Unknown;
        self.missing_since = None;
    }

    fn sample_life(&mut self, roblox: &Arc<Roblox>) {
        let Some(character) = roblox.get_local_character() else {
            // A character that is already gone when the place unloads was a
            // leave, not a death; the log transition handles that case.
            if self.status.life != LifeState::Alive || !roblox.game_loaded() {
                return;
            }

            let since = *self.missing_since.get_or_insert_with(Instant::now);
            if since.elapsed() >= MISSING_DEBOUNCE {
                self.status.life = LifeState::Dead;
            }
            return;
        };

        self.missing_since = None;

        // Health and the humanoid state are independent signals; either one is
        // enough, which covers games that zero health without setting `Dead`.
        let health = utils::get_humanoid_health(&character);
        let state = character.get_humanoid_state_id();
        let dead = state == Some(DEAD_STATE_ID)
            || health.map(|health| health <= 0.0).unwrap_or(false);

        self.status.life = if dead {
            LifeState::Dead
        } else {
            LifeState::Alive
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{is_player_log, parse_line, LogEvent};

    #[test]
    fn crash_handler_logs_are_never_selected() {
        assert!(is_player_log(
            "0.745.0.7451137_20261028T233337Z_Player_E0E44_last.log"
        ));
        assert!(!is_player_log(
            "0.745.0.7451137_20261028T233337Z_Player_1EA8E_CrashHandler_last.log"
        ));
        assert!(!is_player_log("0.745.0.7451137_20261028T233337Z_Studio.log"));
    }

    #[test]
    fn join_lines_carry_the_place_id() {
        assert_eq!(
            parse_line(
                "2026-10-28T10:03:44.431Z,21.43,0ad8,6 [FLog::Output] ! Joining game \
                 '35914af2-61bf-410f-a45b-cc9487a18d5c' place 123974602339071 at 10.32.3.208"
            ),
            Some(LogEvent::Joined {
                place_id: 123974602339071
            })
        );
    }

    #[test]
    fn leaving_lines_are_recognised_but_startup_noise_is_not() {
        assert_eq!(
            parse_line(
                "2026-10-28T10:04:00.000Z,1.0,5f34,6 [FLog::SingleSurfaceApp] \
                 returnToLuaApp: (stage:UGCGame)."
            ),
            Some(LogEvent::Left)
        );
        assert_eq!(
            parse_line(
                "2026-10-28T10:04:01.000Z,1.0,5f34,6 [FLog::SessionTransitionFSM] \
                 Entered app session."
            ),
            Some(LogEvent::Left)
        );

        // Real logs end with `leaveUGCGameInternal` too.
        assert_eq!(
            parse_line(
                "2026-10-28T23:51:13.916Z,1056.9,2158,6 [FLog::SingleSurfaceApp] \
                 leaveUGCGameInternal"
            ),
            Some(LogEvent::Left)
        );

        // Startup-only lines must not be mistaken for a real leave. The first
        // line of every Player log is the app initialising.
        assert_eq!(
            parse_line(
                "2026-10-28T23:33:37.723Z,0.7,2158,6 [FLog::SingleSurfaceApp] \
                 returnToLuaApp: (stage:InitializedLuaApp)."
            ),
            None
        );
        assert_eq!(
            parse_line(
                "2026-10-28T10:03:23.843Z,0.5,5f34,6 [FLog::SingleSurfaceApp] \
                 returnToLuaAppInternal: ... App has been initialized, returning from game."
            ),
            None
        );
        assert_eq!(
            parse_line(
                "2026-10-28T10:03:23.843Z,0.5,5f34,6 [FLog::SingleSurfaceApp] \
                 setStage: (stage:InitializedLuaApp)"
            ),
            None
        );
        assert_eq!(parse_line("just some continuation line"), None);
    }
}
