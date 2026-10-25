//! State shared by the UI/API threads and the backend loop.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::sdk::roblox::Roblox;

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
        self.drop_seq.fetch_add(1, Ordering::AcqRel);
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
        }
    }
}
