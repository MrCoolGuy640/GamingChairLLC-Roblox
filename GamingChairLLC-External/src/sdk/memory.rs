//! Process memory access and the background memory freezer.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use anyhow::{bail, Result};
use parking_lot::Mutex;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, MODULEENTRY32W, TH32CS_SNAPMODULE,
    TH32CS_SNAPMODULE32,
};
use windows_sys::Win32::System::Threading::{
    GetCurrentThread, OpenProcess, SetThreadPriority, PROCESS_ALL_ACCESS,
    THREAD_PRIORITY_ABOVE_NORMAL,
};

use super::hyperion::SmartDisabler;
use super::ntdll::{NtReadVirtualMemory, NtWriteVirtualMemory};
use super::process;

pub struct Memory {
    handle: HANDLE,
    pid: u32,
    base_address: u64,
    disabler: Arc<SmartDisabler>,
}

// The raw process handle is immutable and the Nt* memory functions are
// thread-safe. This is what lets the freezer, explorer and backend loop share
// one connection.
unsafe impl Send for Memory {}
unsafe impl Sync for Memory {}

impl Memory {
    pub fn attach(pid: u32, module_name: &str) -> Result<Arc<Self>> {
        let disabler = SmartDisabler::new(pid);

        let handle = unsafe { OpenProcess(PROCESS_ALL_ACCESS, 0, pid) };
        if handle.is_null() {
            bail!("OpenProcess failed for PID {pid}");
        }

        let base_address = module_base_address(handle, pid, module_name, &disabler);
        if base_address == 0 {
            unsafe {
                CloseHandle(handle);
            }
            bail!("Could not find module '{module_name}' in process {pid}");
        }

        log::info!(
            target: "GamingChairLLC.memory",
            "Attached to PID {pid} (base 0x{base_address:x})"
        );

        Ok(Arc::new(Self {
            handle,
            pid,
            base_address,
            disabler,
        }))
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn base_address(&self) -> u64 {
        self.base_address
    }

    pub fn read_bytes(&self, address: u64, size: usize) -> Vec<u8> {
        self.disabler.disable();

        if address <= 0x1000 || size == 0 || size > 64 * 1024 * 1024 {
            return Vec::new();
        }

        let mut buffer = vec![0u8; size];
        let mut bytes_read: usize = 0;

        let status = unsafe {
            NtReadVirtualMemory(
                self.handle,
                address as *const c_void,
                buffer.as_mut_ptr() as *mut c_void,
                size,
                &mut bytes_read,
            )
        };

        if status != 0 || bytes_read == 0 {
            log::trace!(
                target: "GamingChairLLC.memory",
                "read failed at 0x{address:x} size={size} status=0x{status:x}"
            );
            return Vec::new();
        }

        buffer.truncate(bytes_read.min(size));
        buffer
    }

    pub fn read_int(&self, address: u64) -> i32 {
        let data = self.read_bytes(address, 4);
        if data.len() < 4 {
            return 0;
        }
        i32::from_le_bytes([data[0], data[1], data[2], data[3]])
    }

    pub fn read_long(&self, address: u64) -> i64 {
        let data = self.read_bytes(address, 8);
        if data.len() < 8 {
            return 0;
        }
        i64::from_le_bytes(data[..8].try_into().unwrap_or([0; 8]))
    }

    pub fn read_ptr(&self, address: u64) -> u64 {
        self.read_long(address) as u64
    }

    pub fn read_float(&self, address: u64) -> f32 {
        let data = self.read_bytes(address, 4);
        if data.len() < 4 {
            return 0.0;
        }
        f32::from_le_bytes([data[0], data[1], data[2], data[3]])
    }

    pub fn read_double(&self, address: u64) -> f64 {
        let data = self.read_bytes(address, 8);
        if data.len() < 8 {
            return 0.0;
        }
        f64::from_le_bytes(data[..8].try_into().unwrap_or([0; 8]))
    }

    pub fn write_double(&self, address: u64, value: f64) -> bool {
        self.write_bytes(address, &value.to_le_bytes())
    }

    pub fn read_bool(&self, address: u64) -> bool {
        let data = self.read_bytes(address, 1);
        data.first().copied().map(|value| value != 0).unwrap_or(false)
    }

    pub fn read_string(&self, address: u64, length: usize) -> String {
        if address == 0 {
            return String::new();
        }

        if length == 0 {
            let mut data = Vec::new();
            let mut offset = 0u64;

            loop {
                let chunk = self.read_bytes(address + offset, 64);
                if chunk.is_empty() {
                    break;
                }

                if let Some(end) = chunk.iter().position(|&byte| byte == 0) {
                    data.extend_from_slice(&chunk[..end]);
                    break;
                }

                data.extend_from_slice(&chunk);
                offset += 64;
                if offset > 1024 {
                    break;
                }
            }

            return String::from_utf8_lossy(&data).into_owned();
        }

        let data = self.read_bytes(address, length.min(4096));
        let end = data
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(data.len());
        String::from_utf8_lossy(&data[..end]).into_owned()
    }

    pub fn write_bytes(&self, address: u64, data: &[u8]) -> bool {
        self.disabler.disable();
        self.write_bytes_direct(address, data)
    }

    /// Read without blinding Hyperion. Used by the freezer thread for COPY
    /// slots, matching the Python worker's separate, never-blinded handle.
    pub fn read_bytes_direct(&self, address: u64, size: usize) -> Vec<u8> {
        let mut buffer = vec![0u8; size.min(64 * 1024 * 1024)];
        let read = self.read_bytes_direct_into(address, &mut buffer);
        buffer.truncate(read);
        buffer
    }

    /// Same read, into a caller-owned buffer. The freezer's COPY slot runs this
    /// hundreds of thousands of times a second, so it must not allocate.
    /// Returns the number of bytes read, or 0 on failure.
    pub fn read_bytes_direct_into(&self, address: u64, buffer: &mut [u8]) -> usize {
        if address <= 0x1000 || buffer.is_empty() || buffer.len() > 64 * 1024 * 1024 {
            return 0;
        }

        let mut bytes_read: usize = 0;

        let status = unsafe {
            NtReadVirtualMemory(
                self.handle,
                address as *const c_void,
                buffer.as_mut_ptr() as *mut c_void,
                buffer.len(),
                &mut bytes_read,
            )
        };

        if status != 0 {
            return 0;
        }

        bytes_read.min(buffer.len())
    }

    /// Write without blinding Hyperion. Used by the freezer thread, matching
    /// the Python worker which owned a separate handle and never blinded.
    pub fn write_bytes_direct(&self, address: u64, data: &[u8]) -> bool {
        if address <= 0x1000 || data.is_empty() || data.len() > 64 * 1024 * 1024 {
            return false;
        }

        let mut bytes_written: usize = 0;
        let status = unsafe {
            NtWriteVirtualMemory(
                self.handle,
                address as *const c_void,
                data.as_ptr() as *const c_void,
                data.len(),
                &mut bytes_written,
            )
        };

        if status != 0 {
            log::trace!(
                target: "GamingChairLLC.memory",
                "write failed at 0x{address:x} size={} status=0x{status:x}",
                data.len()
            );
            return false;
        }

        true
    }

    pub fn write_int(&self, address: u64, value: i32) -> bool {
        self.write_bytes(address, &value.to_le_bytes())
    }

    pub fn write_float(&self, address: u64, value: f32) -> bool {
        self.write_bytes(address, &value.to_le_bytes())
    }

    pub fn write_ptr(&self, address: u64, value: u64) -> bool {
        self.write_bytes(address, &value.to_le_bytes())
    }

    pub fn write_bool(&self, address: u64, value: bool) -> bool {
        self.write_bytes(address, &[u8::from(value)])
    }
}

impl Drop for Memory {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe {
                CloseHandle(self.handle);
            }
            self.handle = std::ptr::null_mut();
        }
    }
}

fn module_base_address(
    _process: HANDLE,
    pid: u32,
    module_name: &str,
    disabler: &SmartDisabler,
) -> u64 {
    disabler.disable();

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
        if snapshot.is_null()
            || snapshot == windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE
        {
            return 0;
        }

        let mut entry = MODULEENTRY32W::default();
        entry.dwSize = std::mem::size_of::<MODULEENTRY32W>() as u32;

        let mut base = 0u64;
        let mut next = Module32FirstW(snapshot, &mut entry) != 0;
        while next {
            let name = process::wide_to_string(&entry.szModule);
            if name.eq_ignore_ascii_case(module_name) {
                base = entry.modBaseAddr as u64;
                break;
            }
            next = Module32NextW(snapshot, &mut entry) != 0;
        }

        CloseHandle(snapshot);
        base
    }
}

// ---------------------------------------------------------------------------
// Memory freezer
// ---------------------------------------------------------------------------

pub struct MemoryFreezer {
    shared: Arc<FreezerShared>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

/// A slot payload never exceeds this (validated on every insert).
const MAX_SLOT_BYTES: usize = 64;

/// How often the write loop re-reads the slot table even when nothing signalled
/// a change. Matches the Python worker's `RESCAN`.
const FREEZER_RESCAN_PASSES: u32 = 64;

/// What a freezer slot does on every pass.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SlotKind {
    /// Write a fixed payload (the classic freeze).
    Hold,
    /// Read `length` bytes from `src` and write them to the slot address.
    Copy,
    /// Fill the slot with random floats in `[lo, hi]`.
    Random,
}

struct FreezerSlot {
    kind: SlotKind,
    /// HOLD payload.
    data: Vec<u8>,
    /// COPY source address.
    src: u64,
    length: usize,
    /// RANDOM range.
    lo: f32,
    hi: f32,
    signed: bool,
    active: bool,
}

/// A slot copied out of the table, so a write pass never touches the mutex or
/// the heap. This is what lets the loop reach the Python worker's rate: the
/// Python worker memmoves the payload out of shared memory on every pass, and
/// this is the same idea with a plain array.
#[derive(Clone, Copy)]
struct HotSlot {
    address: u64,
    kind: SlotKind,
    length: usize,
    payload: [u8; MAX_SLOT_BYTES],
    src: u64,
    lo: f32,
    hi: f32,
    signed: bool,
}

struct FreezerShared {
    memory: Arc<Memory>,
    slots: Mutex<HashMap<u64, FreezerSlot>>,
    /// Bumped on every change the write loop has to notice: a slot appearing or
    /// disappearing, a payload update, an arm/park. The loop rebuilds its hot
    /// snapshot when this moves instead of locking on every pass.
    generation: AtomicU64,
    stop: AtomicBool,
}

impl MemoryFreezer {
    pub fn start(memory: Arc<Memory>) -> Arc<Self> {
        let shared = Arc::new(FreezerShared {
            memory,
            slots: Mutex::new(HashMap::new()),
            generation: AtomicU64::new(0),
            stop: AtomicBool::new(false),
        });

        let thread_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("freezer".into())
            .spawn(move || freezer_loop(thread_shared))
            .ok();

        log::debug!(target: "GamingChairLLC.freezer", "Memory freezer started");

        Arc::new(Self {
            shared,
            thread: Mutex::new(handle),
        })
    }

    /// Freeze `data` at `address` (HOLD slot).
    pub fn freeze(&self, address: u64, data: Vec<u8>) {
        if data.is_empty() || data.len() > MAX_SLOT_BYTES {
            return;
        }
        let length = data.len();
        self.shared.slots.lock().insert(
            address,
            FreezerSlot {
                kind: SlotKind::Hold,
                data,
                src: 0,
                length,
                lo: 0.0,
                hi: 0.0,
                signed: false,
                active: true,
            },
        );
        self.bump();
    }

    /// Copy `length` bytes from `src` into `address` on every pass (COPY slot).
    pub fn freeze_copy(&self, address: u64, src: u64, length: usize) {
        if address == 0 || src == 0 || length == 0 || length > MAX_SLOT_BYTES {
            return;
        }
        self.shared.slots.lock().insert(
            address,
            FreezerSlot {
                kind: SlotKind::Copy,
                data: Vec::new(),
                src,
                length,
                lo: 0.0,
                hi: 0.0,
                signed: false,
                active: true,
            },
        );
        self.bump();
    }

    /// Fill `length` bytes at `address` with fresh random floats each pass
    /// (RANDOM slot). `signed` gives each float a random sign, which is what
    /// makes the fling tumble instead of settling on one axis.
    pub fn freeze_random(&self, address: u64, length: usize, lo: f32, hi: f32, signed: bool) {
        if address == 0 || length == 0 || length > MAX_SLOT_BYTES || !(hi > lo) {
            return;
        }
        self.shared.slots.lock().insert(
            address,
            FreezerSlot {
                kind: SlotKind::Random,
                data: Vec::new(),
                src: 0,
                length,
                lo,
                hi,
                signed,
                active: true,
            },
        );
        self.bump();
    }

    pub fn unfreeze(&self, address: u64) {
        if self.shared.slots.lock().remove(&address).is_some() {
            self.bump();
        }
    }

    /// Replace the payload of a HOLD slot, or the range of a RANDOM slot.
    /// Unknown addresses are ignored.
    pub fn update(&self, address: u64, data: &[u8]) {
        {
            let mut slots = self.shared.slots.lock();
            let Some(slot) = slots.get_mut(&address) else {
                return;
            };
            if slot.kind != SlotKind::Hold {
                return;
            }

            let length = data.len().min(MAX_SLOT_BYTES);
            slot.data.clear();
            slot.data.extend_from_slice(&data[..length]);
            slot.length = length;
        }
        self.bump();
    }

    pub fn update_random(&self, address: u64, lo: f32, hi: f32, signed: bool) {
        {
            let mut slots = self.shared.slots.lock();
            let Some(slot) = slots.get_mut(&address) else {
                return;
            };
            if slot.kind != SlotKind::Random || !(hi > lo) {
                return;
            }
            slot.lo = lo;
            slot.hi = hi;
            slot.signed = signed;
        }
        self.bump();
    }

    /// Arm or park a slot without removing it. Parking is what lets the aim
    /// assist hand the camera back to the game without losing its address.
    pub fn set_active(&self, address: u64, active: bool) {
        {
            let mut slots = self.shared.slots.lock();
            let Some(slot) = slots.get_mut(&address) else {
                return;
            };
            if slot.active == active {
                return;
            }
            slot.active = active;
        }
        self.bump();
    }

    /// Tell the write loop its snapshot is stale.
    fn bump(&self) {
        self.shared.generation.fetch_add(1, Ordering::Release);
    }

    pub fn is_active(&self, address: u64) -> bool {
        self.shared
            .slots
            .lock()
            .get(&address)
            .map(|slot| slot.active)
            .unwrap_or(false)
    }

    pub fn unfreeze_all(&self) {
        self.shared.slots.lock().clear();
        self.bump();
    }

    pub fn is_frozen(&self, address: u64) -> bool {
        self.shared.slots.lock().contains_key(&address)
    }

    pub fn stop(&self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(handle) = self.thread.lock().take() {
            let _ = handle.join();
        }
        self.shared.slots.lock().clear();
    }
}

impl Drop for MemoryFreezer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Write every active slot as fast as the machine allows.
///
/// This is the piece that keeps the character where Fly put it. The engine
/// writes the part's own position and velocity on every physics step, so
/// whatever writes the *most often* wins; the Python version this ports runs a
/// dedicated worker process whose loops do nothing but write, and it measures
/// ~210k passes/sec. A sleep-based loop cannot compete at that scale: at 1 ms
/// per pass the engine gets a whole frame's worth of writes in between ours,
/// which is exactly the "snaps back to the ground" and "flung away" behaviour.
///
/// So: no sleep while anything is active, a snapshot instead of a lock per
/// pass, and preallocated buffers so a pass never touches the allocator.
fn freezer_loop(shared: Arc<FreezerShared>) {
    raise_thread_priority();

    // xorshift32: cheap, non-repeating, and does not need an allocation.
    let mut rng: u32 = 0x9E37_79B9;

    let mut hot: Vec<HotSlot> = Vec::with_capacity(8);
    let mut scratch = [0u8; MAX_SLOT_BYTES];
    let mut buffer = [0u8; MAX_SLOT_BYTES];

    // `u64::MAX` forces a scan on the first pass.
    let mut seen_generation = u64::MAX;
    let mut rescan: u32 = FREEZER_RESCAN_PASSES;

    while !shared.stop.load(Ordering::Acquire) {
        let generation = shared.generation.load(Ordering::Acquire);
        rescan += 1;

        if generation != seen_generation || rescan >= FREEZER_RESCAN_PASSES {
            seen_generation = generation;
            rescan = 0;
            hot.clear();

            let slots = shared.slots.lock();
            for (address, slot) in slots.iter() {
                if !slot.active || slot.length == 0 || slot.length > MAX_SLOT_BYTES {
                    continue;
                }

                let mut entry = HotSlot {
                    address: *address,
                    kind: slot.kind,
                    length: slot.length,
                    payload: [0u8; MAX_SLOT_BYTES],
                    src: slot.src,
                    lo: slot.lo,
                    hi: slot.hi,
                    signed: slot.signed,
                };

                if slot.kind == SlotKind::Hold {
                    entry.payload[..slot.length].copy_from_slice(&slot.data[..slot.length]);
                }

                hot.push(entry);
            }
        }

        if hot.is_empty() {
            // Nothing frozen: no reason to burn a core.
            thread::sleep(Duration::from_millis(10));
            continue;
        }

        for slot in &hot {
            match slot.kind {
                SlotKind::Hold => {
                    shared
                        .memory
                        .write_bytes_direct(slot.address, &slot.payload[..slot.length]);
                }
                SlotKind::Copy => {
                    if slot.src == 0 {
                        continue;
                    }

                    let read = shared
                        .memory
                        .read_bytes_direct_into(slot.src, &mut scratch[..slot.length]);
                    if read == slot.length {
                        shared
                            .memory
                            .write_bytes_direct(slot.address, &scratch[..slot.length]);
                    }
                }
                SlotKind::Random => {
                    let span = slot.hi - slot.lo;
                    let mut offset = 0usize;

                    while offset + 4 <= slot.length {
                        rng ^= rng << 13;
                        rng ^= rng >> 17;
                        rng ^= rng << 5;

                        let mut value = slot.lo + span * ((rng >> 8) as f32 / 16_777_216.0);
                        if slot.signed && (rng & 1) != 0 {
                            value = -value;
                        }
                        buffer[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
                        offset += 4;
                    }

                    shared
                        .memory
                        .write_bytes_direct(slot.address, &buffer[..slot.length]);
                }
            }
        }

        std::hint::spin_loop();
    }
}

/// Above normal, deliberately not high or realtime.
///
/// This is a spin loop, and at normal priority it gets descheduled for whole
/// frames under load; the engine's own value then sits in memory for the whole
/// gap. The Python worker runs at the same priority for the same reason.
fn raise_thread_priority() {
    unsafe {
        SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
}
