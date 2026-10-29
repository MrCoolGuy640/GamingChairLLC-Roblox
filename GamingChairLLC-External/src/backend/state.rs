//! State shared by the UI/API threads and the backend loop.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::sdk::roblox::Roblox;

/// Which screen the client is on, as far as the log monitor can tell.
///
/// `Joining` means the log reported a join but `DataModel.GameLoaded` has not
/// turned true yet. Only `InGame` is a state where the character normally
/// exists.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum GamePhase {
    #[default]
    Unknown,
    /// The Lua app / home screen (no place loaded).
    Home,
    /// A place is loading; keep polling `GameLoaded`.
    Joining,
    /// The place finished loading.
    InGame,
}

impl GamePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Home => "home",
            Self::Joining => "joining",
            Self::InGame => "in_game",
        }
    }
}

/// Local-player life state from the death monitor.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LifeState {
    /// No character yet (still loading, or the character has not spawned).
    #[default]
    Unknown,
    /// A character exists and reports a positive health / a non-dead state.
    Alive,
    /// Health reached zero, the humanoid reported `Dead`, or the character was
    /// removed while the place was still loaded.
    Dead,
}

impl LifeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unknown => "unknown",
            Self::Alive => "alive",
            Self::Dead => "dead",
        }
    }
}

/// What the monitors currently believe about the Roblox session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct GameStatus {
    pub phase: GamePhase,
    pub place_id: i64,
    pub life: LifeState,
}

impl GameStatus {
    pub fn is_live_game(&self) -> bool {
        self.phase == GamePhase::InGame && self.life == LifeState::Alive
    }
}

pub struct BackendState {
    attached: AtomicBool,
    loop_connected: AtomicBool,
    /// f64 bits. 0.0 while the loop is parked.
    loop_hz: AtomicU64,
    /// Bumped every time the loop drops the attachment on its own.
    drop_seq: AtomicU64,
    drop_reason: Mutex<String>,
    /// Bumped whenever a cheat's desired state changes.
    cheat_epoch: AtomicU64,
    roblox: Mutex<Option<Arc<Roblox>>>,
    game: Mutex<GameStatus>,
    /// True while hacks are held off because the character is gone (home
    /// screen or death). Desired states are kept; the loop skips syncing.
    hacks_suspended: AtomicBool,
    /// Settings that decide whether a leave/death clears the switches.
    disable_hacks_on_leave: AtomicBool,
    disable_hacks_on_death: AtomicBool,
}

impl BackendState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    pub fn attached(&self) -> bool {
        self.attached.load(Ordering::Acquire)
    }

    pub fn set_attached(&self, attached: bool) {
        self.attached.store(attached, Ordering::Release);
    }

    pub fn loop_connected(&self) -> bool {
        self.loop_connected.load(Ordering::Acquire)
    }

    pub fn set_loop_connected(&self, connected: bool) {
        self.loop_connected.store(connected, Ordering::Release);
    }

    pub fn loop_hz(&self) -> f64 {
        f64::from_bits(self.loop_hz.load(Ordering::Relaxed))
    }

    pub fn set_loop_hz(&self, hz: f64) {
        self.loop_hz.store(hz.to_bits(), Ordering::Relaxed);
    }

    pub fn drop_seq(&self) -> u64 {
        self.drop_seq.load(Ordering::Acquire)
    }

    pub fn drop_reason(&self) -> String {
        self.drop_reason.lock().clone()
    }

    /// Record a self-initiated disconnect (game exited, loop crashed...).
    pub fn dropped(&self, reason: &str) {
        *self.drop_reason.lock() = reason.to_string();
        self.attached.store(false, Ordering::Release);
        *self.roblox.lock() = None;
        *self.game.lock() = GameStatus::default();
        self.hacks_suspended.store(false, Ordering::Release);
        self.drop_seq.fetch_add(1, Ordering::AcqRel);
    }

    pub fn game_status(&self) -> GameStatus {
        *self.game.lock()
    }

    pub fn set_game_status(&self, status: GameStatus) {
        *self.game.lock() = status;
    }

    pub fn hacks_suspended(&self) -> bool {
        self.hacks_suspended.load(Ordering::Acquire)
    }

    pub fn set_hacks_suspended(&self, suspended: bool) {
        self.hacks_suspended.store(suspended, Ordering::Release);
    }

    pub fn disable_hacks_on_leave(&self) -> bool {
        self.disable_hacks_on_leave.load(Ordering::Acquire)
    }

    pub fn disable_hacks_on_death(&self) -> bool {
        self.disable_hacks_on_death.load(Ordering::Acquire)
    }

    /// Mirror the two persisted gameplay settings into the shared state so the
    /// backend loop can read them without touching the settings file.
    pub fn set_gameplay_settings(&self, disable_on_leave: bool, disable_on_death: bool) {
        self.disable_hacks_on_leave
            .store(disable_on_leave, Ordering::Release);
        self.disable_hacks_on_death
            .store(disable_on_death, Ordering::Release);
    }

    pub fn epoch(&self) -> u64 {
        self.cheat_epoch.load(Ordering::Acquire)
    }

    pub fn bump_epoch(&self) {
        self.cheat_epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub fn roblox(&self) -> Option<Arc<Roblox>> {
        self.roblox.lock().clone()
    }

    pub fn set_roblox(&self, roblox: Option<Arc<Roblox>>) {
        *self.roblox.lock() = roblox;
    }
}

impl Default for BackendState {
    fn default() -> Self {
        // `new` returns an Arc, so the Default impl cannot be derived cleanly.
        // This exists only to satisfy tools that expect it.
        Self {
            attached: AtomicBool::new(false),
            loop_connected: AtomicBool::new(false),
            loop_hz: AtomicU64::new(0),
            drop_seq: AtomicU64::new(0),
            drop_reason: Mutex::new(String::new()),
            cheat_epoch: AtomicU64::new(0),
            roblox: Mutex::new(None),
            game: Mutex::new(GameStatus::default()),
            hacks_suspended: AtomicBool::new(false),
            disable_hacks_on_leave: AtomicBool::new(false),
            disable_hacks_on_death: AtomicBool::new(false),
        }
    }
}
