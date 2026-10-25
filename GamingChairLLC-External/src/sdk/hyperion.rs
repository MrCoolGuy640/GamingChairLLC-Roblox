//! Hyperion (Roblox's anti-cheat) thread blinding.
//!
//! Roblox's Hyperion protection runs in `ntdll.dll` threads. Before touching the
//! target process we suspend those threads; a background monitor keeps new ones
//! suspended for a short window and then resumes exactly the threads it
//! suspended. This mirrors the Python `SmartDisabler` without spawning helper
//! processes.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::{Condvar, Mutex};
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::System::Threading::{
    OpenThread, ResumeThread, SuspendThread, THREAD_SUSPEND_RESUME,
};

use super::process;

/// How long threads stay suspended after the last memory operation.
const BLIND_WINDOW_MS: u64 = 1_000;
const MONITOR_TICK: Duration = Duration::from_millis(100);
const SUSPEND_WAIT: Duration = Duration::from_millis(100);

pub struct SmartDisabler {
    shared: Arc<Shared>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

struct Shared {
    pid: u32,
    last_disable_ms: AtomicU64,
    stop: AtomicBool,
    suspended: Mutex<HashSet<u32>>,
    anti_detection: Mutex<bool>,
    anti_detection_cv: Condvar,
    wake: Mutex<()>,
    wake_cv: Condvar,
}

impl SmartDisabler {
    pub fn new(pid: u32) -> Arc<Self> {
        let shared = Arc::new(Shared {
            pid,
            // Initialised in the past so the monitor starts in the "resumed"
            // state and does not suspend anything until the first read/write.
            last_disable_ms: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            suspended: Mutex::new(HashSet::new()),
            anti_detection: Mutex::new(false),
            anti_detection_cv: Condvar::new(),
            wake: Mutex::new(()),
            wake_cv: Condvar::new(),
        });

        let thread_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("hyperion".into())
            .spawn(move || monitor(thread_shared))
            .ok();

        Arc::new(Self {
            shared,
            thread: Mutex::new(handle),
        })
    }

    /// Called before every target memory access. Returns once Hyperion is
    /// blinded (or after a short timeout if it cannot be blinded).
    pub fn disable(&self) {
        self.shared
            .last_disable_ms
            .store(now_ms(), Ordering::Relaxed);

        if !self.shared.suspended.lock().is_empty() {
            return;
        }

        self.shared.wake_cv.notify_all();

        let deadline = Instant::now() + SUSPEND_WAIT;
        let mut anti = self.shared.anti_detection.lock();
        while !*anti {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            self.shared
                .anti_detection_cv
                .wait_for(&mut anti, deadline - now);
        }
    }

    pub fn is_suspended(&self) -> bool {
        !self.shared.suspended.lock().is_empty()
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.wake_cv.notify_all();

        if let Some(handle) = self.thread.lock().take() {
            let _ = handle.join();
        }

        self.resume_all();
    }
}

impl Drop for SmartDisabler {
    fn drop(&mut self) {
        self.stop();
    }
}

fn monitor(shared: Arc<Shared>) {
    let mut suspended: HashSet<u32> = HashSet::new();
    let mut examined: HashSet<u32> = HashSet::new();
    let mut blinded = false;

    loop {
        if shared.stop.load(Ordering::Acquire) {
            break;
        }

        let elapsed = now_ms().saturating_sub(shared.last_disable_ms.load(Ordering::Relaxed));

        if elapsed < BLIND_WINDOW_MS {
            let modules = process::modules(shared.pid);
            let tids = process::thread_ids(shared.pid);

            for &tid in &tids {
                if suspended.contains(&tid) || examined.contains(&tid) {
                    continue;
                }
                examined.insert(tid);

                if let Some(address) = process::thread_start_address(tid) {
                    let resolved = process::resolve_address(address, &modules);
                    if resolved.contains("ntdll.dll") && suspend_thread(tid) {
                        suspended.insert(tid);
                        shared.suspended.lock().insert(tid);
                    }
                }
            }

            // Forget threads that no longer exist. They cannot be resumed, and
            // keeping stale IDs risks resuming an unrelated reused thread.
            suspended.retain(|tid| tids.contains(tid));
            shared.suspended.lock().retain(|tid| tids.contains(tid));

            if suspended.is_empty() {
                *shared.anti_detection.lock() = false;
                shared.anti_detection_cv.notify_all();
            } else {
                *shared.anti_detection.lock() = true;
                shared.anti_detection_cv.notify_all();
                if !blinded {
                    blinded = true;
                    log::debug!(
                        target: "GamingChairLLC.hyperion",
                        "Blinding Hyperion ({} threads suspended)",
                        suspended.len()
                    );
                }
            }
        } else {
            if !suspended.is_empty() {
                for tid in suspended.drain() {
                    resume_thread(tid);
                }
                shared.suspended.lock().clear();
                log::debug!(target: "GamingChairLLC.hyperion", "Unblinding Hyperion");
            }

            examined.clear();
            blinded = false;
            *shared.anti_detection.lock() = false;
            shared.anti_detection_cv.notify_all();
        }

        let mut wake = shared.wake.lock();
        shared.wake_cv.wait_for(&mut wake, MONITOR_TICK);
    }

    for tid in suspended {
        resume_thread(tid);
    }
    shared.suspended.lock().clear();
    *shared.anti_detection.lock() = false;
    shared.anti_detection_cv.notify_all();
}

impl Shared {
    fn resume_all(&self) {
        let tids: Vec<u32> = self.suspended.lock().drain().collect();
        for tid in tids {
            resume_thread(tid);
        }
        *self.anti_detection.lock() = false;
        self.anti_detection_cv.notify_all();
    }
}

impl SmartDisabler {
    fn resume_all(&self) {
        self.shared.resume_all();
    }
}

fn suspend_thread(tid: u32) -> bool {
    unsafe {
        let handle = OpenThread(THREAD_SUSPEND_RESUME, 0, tid);
        if handle.is_null() {
            return false;
        }
        let result = SuspendThread(handle) != u32::MAX;
        CloseHandle(handle);
        result
    }
}

fn resume_thread(tid: u32) {
    unsafe {
        let handle = OpenThread(THREAD_SUSPEND_RESUME, 0, tid);
        if !handle.is_null() {
            ResumeThread(handle);
            CloseHandle(handle);
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
