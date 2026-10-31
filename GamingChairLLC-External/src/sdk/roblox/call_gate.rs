//! Borrow an engine thread to run one function call.
//!
//! This is the Rust port of the Python `callgate.py`. Roblox's allocator and
//! name interning only behave correctly on an engine thread, so `Instance.new`
//! cannot be called from a thread we create ourselves.
//!
//! The technique:
//!
//! 1. A hot member-function descriptor keeps the address of the code the engine
//!    calls at some offset inside the descriptor. That offset moves between
//!    builds, so the caller discovers it (`instance_new.rs`).
//! 2. We allocate a state page, place an executable stub in the padding of an
//!    already-executable system DLL, then point that slot at the stub.
//! 3. The stub forwards every call straight to the original function, unless a
//!    command is queued. A queued command is claimed with `lock cmpxchg`, run
//!    on the real engine thread, stored, and then the engine's own call still
//!    happens.
//! 4. `remove()` restores the original pointer and the padding bytes.
//!
//! Only `.data`, a system DLL's existing padding and our own state page are
//! written. A fresh private executable page is deliberately not used: the
//! client treats that as a foreign executable region and dies as soon as the
//! patched slot is called. The Python implementation placed the stub in system
//! DLL padding for the same reason.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use windows_sys::Win32::System::Memory::{MEM_COMMIT, PAGE_EXECUTE_READWRITE};

use crate::sdk::memory::Memory;
use crate::sdk::process;

// Shared state page layout (identical to callgate.py).
const ST_PENDING: u64 = 0x00;
const ST_DONE: u64 = 0x04;
const ST_FN: u64 = 0x08;
const ST_A0: u64 = 0x10;
const ST_A1: u64 = 0x18;
const ST_A2: u64 = 0x20;
const ST_A3: u64 = 0x28;
const ST_RET: u64 = 0x30;
const ST_CALLS: u64 = 0x38;
const ST_TID: u64 = 0x40;
const ST_SCRATCH: u64 = 0x100;
const STATE_SIZE: usize = 0x200;

/// Foreign system DLLs whose executable padding we borrow, in preference order.
/// A brand-new private executable page gets the client killed as soon as the
/// patched slot is called; padding in an image that is already executable does
/// not. Same list, same order as `callgate.py`.
const CAVE_MODULES: &[&str] = &[
    "winsta.dll",
    "win32u.dll",
    "uxtheme.dll",
    "dwmapi.dll",
    "msctf.dll",
    "TextInputFramework.dll",
    "CoreMessaging.dll",
];

const CAVE_CHUNK: u64 = 1 << 20;
const PAGE_EXEC: u32 = 0x10 | 0x20 | 0x40 | 0x80;
const PAGE_GUARD: u32 = 0x100;
const PAGE_NOACCESS: u32 = 0x01;

const IMPL_SCAN_FROM: u64 = 0x60;
const IMPL_SCAN_TO: u64 = 0x130;

pub struct CallGate {
    memory: Arc<Memory>,
    slot: u64,
    state: u64,
    stub: u64,
    orig: u64,
    installed: bool,
    /// Whether `stub` points into a system DLL's padding. Those bytes belong to
    /// the image, so `remove()` puts them back instead of freeing the page.
    stub_is_cave: bool,
    cave_bytes: Vec<u8>,
    cave_protect: u32,
    /// Serialises commands: one state page can only hold one command at a time.
    io: Mutex<()>,
    failure: Mutex<String>,
}

impl CallGate {
    /// `descriptor` is the method descriptor address; `impl_offset` is the
    /// offset inside it that holds the implementation pointer.
    pub fn new(memory: Arc<Memory>, descriptor: u64, impl_offset: u64) -> Self {
        Self {
            memory,
            slot: descriptor.wrapping_add(impl_offset),
            state: 0,
            stub: 0,
            orig: 0,
            installed: false,
            stub_is_cave: false,
            cave_bytes: Vec::new(),
            cave_protect: 0,
            io: Mutex::new(()),
            failure: Mutex::new(String::new()),
        }
    }

    /// Scan a descriptor for executable pointers (candidate impl slots).
    pub fn impl_candidates(
        memory: &Memory,
        descriptor: u64,
        is_exec: impl Fn(u64) -> bool,
    ) -> Vec<(u64, u64)> {
        let mut candidates = Vec::new();
        let blob = memory.read_bytes(
            descriptor.wrapping_add(IMPL_SCAN_FROM),
            (IMPL_SCAN_TO - IMPL_SCAN_FROM) as usize,
        );
        if blob.len() < 8 {
            return candidates;
        }

        for offset in (0..=blob.len() - 8).step_by(8) {
            let value = u64::from_le_bytes(blob[offset..offset + 8].try_into().unwrap_or([0; 8]));
            if is_exec(value) {
                candidates.push((IMPL_SCAN_FROM + offset as u64, value));
            }
        }

        candidates
    }

    /// Install the passive listener stub. It only records how often the slot is
    /// called, so it cannot crash the target no matter what the arguments are.
    pub fn install_listener(&mut self) -> bool {
        self.install_inner(true)
    }

    /// Install the command-running stub.
    pub fn install(&mut self) -> bool {
        self.install_inner(false)
    }

    /// Queue `function(a0..a3)` for an engine thread and wait for the result.
    ///
    /// `tid` restricts which thread may claim it; `0` accepts any.
    pub fn invoke(
        &self,
        function: u64,
        a0: u64,
        a1: u64,
        a2: u64,
        a3: u64,
        tid: u64,
        timeout: Duration,
    ) -> (bool, u64) {
        let _io = self.io.lock();

        if !self.installed || self.state == 0 {
            return (false, 0);
        }

        let memory = &self.memory;
        memory.write_int(self.state + ST_DONE, 0);

        let mut command = [0u8; 40];
        command[0..8].copy_from_slice(&function.to_le_bytes());
        command[8..16].copy_from_slice(&a0.to_le_bytes());
        command[16..24].copy_from_slice(&a1.to_le_bytes());
        command[24..32].copy_from_slice(&a2.to_le_bytes());
        command[32..40].copy_from_slice(&a3.to_le_bytes());
        memory.write_bytes(self.state + ST_FN, &command);
        memory.write_ptr(self.state + ST_TID, tid);

        // Count before publishing, so a timeout can tell "cold slot" apart from
        // "the thread never picked the command up".
        let calls_before = self.calls();

        // Pending last: the stub must never observe a half-written command.
        memory.write_int(self.state + ST_PENDING, 1);

        let deadline = Instant::now() + timeout;
        loop {
            if memory.read_int(self.state + ST_DONE) == 1 {
                return (true, memory.read_long(self.state + ST_RET) as u64);
            }

            if Instant::now() >= deadline {
                break;
            }

            thread::sleep(Duration::from_millis(1));
        }

        *self.failure.lock() = if self.calls() == calls_before {
            "cold slot (never called)".to_string()
        } else {
            "timeout (command not picked up)".to_string()
        };

        // Clear pending so a late engine call cannot run the stale command.
        memory.write_int(self.state + ST_PENDING, 0);

        (false, 0)
    }

    pub fn calls(&self) -> u64 {
        if self.state == 0 {
            return 0;
        }
        self.memory.read_long(self.state + ST_CALLS) as u64
    }

    /// Out-parameter scratch area passed to engine functions.
    pub fn scratch(&self) -> u64 {
        self.state.wrapping_add(ST_SCRATCH)
    }

    pub fn last_failure(&self) -> String {
        self.failure.lock().clone()
    }

    pub fn remove(&mut self) {
        if self.installed {
            self.memory.write_ptr(self.slot, self.orig);
            self.installed = false;
        }

        if self.state != 0 {
            self.memory.free(self.state);
            self.state = 0;
        }

        if self.stub != 0 {
            if self.stub_is_cave {
                // Put the padding back exactly as it was, then restore the
                // DLL's own protection. The page must never be freed: it
                // belongs to the image.
                if !self.cave_bytes.is_empty() {
                    let length = self.cave_bytes.len();
                    self.memory
                        .protect(self.stub, length, PAGE_EXECUTE_READWRITE);
                    self.memory.write_bytes(self.stub, &self.cave_bytes);
                    self.memory
                        .protect(self.stub, length, self.cave_protect);
                }
                self.cave_bytes.clear();
                self.stub_is_cave = false;
            } else {
                self.memory.free(self.stub);
            }
            self.stub = 0;
        }
    }

    /// `state` must be the page this stub will report to. It is a parameter
    /// rather than `self.state` on purpose: the state page is allocated and
    /// assembled before `self` records it, and a stub that embeds a zero here
    /// writes to address 0 on the first engine call and kills the client.
    fn build_stub(state: u64) -> (Vec<u8>, usize) {
        let mut asm = Assembler::new();

        // mov r10, state
        asm.emit(&[0x49, 0xBA]);
        asm.emit(&state.to_le_bytes());

        // lock inc qword [r10 + ST_CALLS]
        asm.emit(&[0xF0, 0x49, 0xFF, 0x42, ST_CALLS as u8]);

        // cmp dword [r10], 0 / jne slow
        asm.emit(&[0x41, 0x83, 0x3A, 0x00]);
        asm.jcc(0x85, "slow");

        asm.label("pass");
        asm.jmprip();

        asm.label("slow");
        // mov rax, [r10 + ST_TID] / test rax, rax
        asm.emit(&[0x49, 0x8B, 0x42, ST_TID as u8]);
        asm.emit(&[0x48, 0x85, 0xC0]);
        asm.jcc(0x84, "take");

        // mov r11, gs:[0x48] / cmp rax, r11 / jne pass
        asm.emit(&[0x65, 0x4C, 0x8B, 0x1C, 0x25, 0x48, 0x00, 0x00, 0x00]);
        asm.emit(&[0x4C, 0x39, 0xD8]);
        asm.jcc(0x85, "pass");

        asm.label("take");
        // mov eax, 1 / xor r11d, r11d / lock cmpxchg [r10], r11d / jne pass
        asm.emit(&[0xB8, 0x01, 0x00, 0x00, 0x00]);
        asm.emit(&[0x45, 0x31, 0xDB]);
        asm.emit(&[0xF0, 0x45, 0x0F, 0xB1, 0x1A]);
        asm.jcc(0x85, "pass");

        // sub rsp, 0x88 and save the volatile argument registers plus xmm0-3.
        asm.emit(&[0x48, 0x81, 0xEC, 0x88, 0x00, 0x00, 0x00]);
        asm.emit(&[0x48, 0x89, 0x4C, 0x24, 0x20]);
        asm.emit(&[0x48, 0x89, 0x54, 0x24, 0x28]);
        asm.emit(&[0x4C, 0x89, 0x44, 0x24, 0x30]);
        asm.emit(&[0x4C, 0x89, 0x4C, 0x24, 0x38]);
        asm.emit(&[0x0F, 0x11, 0x44, 0x24, 0x40]);
        asm.emit(&[0x0F, 0x11, 0x4C, 0x24, 0x50]);
        asm.emit(&[0x0F, 0x11, 0x54, 0x24, 0x60]);
        asm.emit(&[0x0F, 0x11, 0x5C, 0x24, 0x70]);
        asm.emit(&[0x4C, 0x89, 0x94, 0x24, 0x80, 0x00, 0x00, 0x00]);

        // rax = fn, rcx..r9 = a0..a3
        asm.emit(&[0x49, 0x8B, 0x42, ST_FN as u8]);
        asm.emit(&[0x49, 0x8B, 0x4A, ST_A0 as u8]);
        asm.emit(&[0x49, 0x8B, 0x52, ST_A1 as u8]);
        asm.emit(&[0x4D, 0x8B, 0x42, ST_A2 as u8]);
        asm.emit(&[0x4D, 0x8B, 0x4A, ST_A3 as u8]);

        asm.emit(&[0xFF, 0xD0]); // call rax

        // Store the result, mark done, restore everything, then forward.
        asm.emit(&[0x4C, 0x8B, 0x94, 0x24, 0x80, 0x00, 0x00, 0x00]);
        asm.emit(&[0x49, 0x89, 0x42, ST_RET as u8]);
        asm.emit(&[0x41, 0xC7, 0x42, ST_DONE as u8, 0x01, 0x00, 0x00, 0x00]);
        asm.emit(&[0x0F, 0x10, 0x44, 0x24, 0x40]);
        asm.emit(&[0x0F, 0x10, 0x4C, 0x24, 0x50]);
        asm.emit(&[0x0F, 0x10, 0x54, 0x24, 0x60]);
        asm.emit(&[0x0F, 0x10, 0x5C, 0x24, 0x70]);
        asm.emit(&[0x48, 0x8B, 0x4C, 0x24, 0x20]);
        asm.emit(&[0x48, 0x8B, 0x54, 0x24, 0x28]);
        asm.emit(&[0x4C, 0x8B, 0x44, 0x24, 0x30]);
        asm.emit(&[0x4C, 0x8B, 0x4C, 0x24, 0x38]);
        asm.emit(&[0x48, 0x81, 0xC4, 0x88, 0x00, 0x00, 0x00]);

        asm.jmp("pass");

        asm.finish()
    }

    fn install_inner(&mut self, listen: bool) -> bool {
        if self.installed {
            return false;
        }

        self.failure.lock().clear();

        self.orig = self.memory.read_ptr(self.slot);
        if !(0x10000 < self.orig && self.orig < 0x7FFF_FFFF_FFFF) {
            *self.failure.lock() = format!(
                "slot at {:#x} holds an implausible pointer {:#x}",
                self.slot, self.orig
            );
            return false;
        }

        let state = self.memory.allocate(STATE_SIZE);
        if state == 0 {
            *self.failure.lock() = "state allocation failed".to_string();
            return false;
        }

        if !self.memory.write_bytes(state, &[0u8; STATE_SIZE]) {
            self.memory.free(state);
            *self.failure.lock() = "state could not be zeroed".to_string();
            return false;
        }

        let (code, orig_offset) = if listen {
            Self::build_listener_stub(state)
        } else {
            Self::build_stub(state)
        };

        // Stub + orig pointer + the helper slack `callgate.py` reserves.
        let need = code.len() + 0x60;

        // The stub must go into padding of an already-executable system DLL. A
        // private executable page is what used to kill the client here: the
        // first engine call through the patched slot terminated the process.
        let Some((stub, cave_protect)) = find_exec_cave(&self.memory, need) else {
            self.memory.free(state);
            *self.failure.lock() = "no executable padding in a system DLL; refusing to install \
                                    the stub in a private page (it kills the client)"
                .to_string();
            return false;
        };

        let original = self.memory.read_bytes(stub, need);
        if original.len() != need {
            self.memory.free(state);
            *self.failure.lock() = format!("cave at {stub:#x} is unreadable");
            return false;
        }

        if self
            .memory
            .protect(stub, need, PAGE_EXECUTE_READWRITE)
            .is_none()
        {
            self.memory.free(state);
            *self.failure.lock() = format!("cave at {stub:#x} could not be made writable");
            return false;
        }

        let orig_slot = stub.wrapping_add(orig_offset as u64);
        let written = self.memory.write_bytes(stub, &code)
            && self.memory.write_ptr(orig_slot, self.orig);

        // Restore the DLL's own protection before anything can call the stub.
        self.memory.protect(stub, need, cave_protect);

        if !written {
            self.memory
                .protect(stub, need, PAGE_EXECUTE_READWRITE);
            self.memory.write_bytes(stub, &original);
            self.memory.protect(stub, need, cave_protect);
            self.memory.free(state);
            *self.failure.lock() = "stub could not be written".to_string();
            return false;
        }

        // Register before the slot points at the stub. This is a no-op on this
        // client (not CFG-enforced) but required on hardened ones.
        let cfg = self.memory.register_call_target(stub);

        if !self.memory.write_ptr(self.slot, stub) {
            self.memory
                .protect(stub, need, PAGE_EXECUTE_READWRITE);
            self.memory.write_bytes(stub, &original);
            self.memory.protect(stub, need, cave_protect);
            self.memory.free(state);
            *self.failure.lock() = "slot could not be pointed at the stub".to_string();
            return false;
        }

        log::debug!(
            target: "GamingChairLLC.instance_new",
            "Call gate stub in cave {stub:#x} (orig slot +{orig_offset:#x}, protect {cave_protect:#x}), \
             cfg {cfg}, slot {slot:#x}",
            slot = self.slot
        );

        self.state = state;
        self.stub = stub;
        self.stub_is_cave = true;
        self.cave_bytes = original;
        self.cave_protect = cave_protect;
        self.installed = true;
        true
    }

    fn build_listener_stub(state: u64) -> (Vec<u8>, usize) {
        let mut asm = Assembler::new();

        // mov r10, state
        asm.emit(&[0x49, 0xBA]);
        asm.emit(&state.to_le_bytes());

        // lock inc qword [r10 + ST_CALLS]
        asm.emit(&[0xF0, 0x49, 0xFF, 0x42, ST_CALLS as u8]);

        // Record the argument registers of the engine's own call.
        asm.emit(&[0x49, 0x89, 0x4A, 0x48]); // mov [r10+0x48], rcx
        asm.emit(&[0x49, 0x89, 0x52, 0x50]); // mov [r10+0x50], rdx
        asm.emit(&[0x4D, 0x89, 0x42, 0x58]); // mov [r10+0x58], r8
        asm.emit(&[0x4D, 0x89, 0x4A, 0x60]); // mov [r10+0x60], r9

        asm.label("pass");
        asm.jmprip();

        asm.finish()
    }
}

impl Drop for CallGate {
    fn drop(&mut self) {
        self.remove();
    }
}

/// Find a run of `0xCC`/`0x00` padding inside an executable system DLL of the
/// target and return `(address, region_protection)`.
///
/// This mirrors `find_exec_cave` in `callgate.py`, including the module order
/// and the `need + 0x10` slack: the returned address is 16-byte aligned and
/// still leaves room for the stub plus its original-pointer slot.
fn find_exec_cave(memory: &Memory, need: usize) -> Option<(u64, u32)> {
    let modules = process::modules(memory.pid());

    for wanted in CAVE_MODULES {
        let Some(module) = modules
            .iter()
            .find(|module| module.name.eq_ignore_ascii_case(wanted))
        else {
            continue;
        };

        let start = (module.base as u64).saturating_add(0x1000); // skip headers
        let end = (module.base as u64).saturating_add(module.size as u64);
        let mut address = start;

        while address < end {
            let Some(region) = memory.region(address) else {
                break;
            };
            let next = region.base.saturating_add(region.size);
            if next <= address {
                break;
            }

            let executable = region.state == MEM_COMMIT
                && region.protect & PAGE_EXEC != 0
                && region.protect & (PAGE_GUARD | PAGE_NOACCESS) == 0;

            if executable {
                let from = region.base.max(start);
                let to = next.min(end);
                if let Some(hit) = scan_cave(memory, from, to, need) {
                    return Some((hit, region.protect));
                }
            }

            address = next;
        }
    }

    None
}

/// First run of at least `need + 0x10` padding bytes in `[from, to)`.
fn scan_cave(memory: &Memory, from: u64, to: u64, need: usize) -> Option<u64> {
    let mut offset = from;

    while offset < to {
        let take = (to - offset).min(CAVE_CHUNK) as usize;
        let blob = memory.read_bytes(offset, take);
        if blob.is_empty() {
            offset += take as u64;
            continue;
        }

        let mut run = 0usize;
        for (index, &byte) in blob.iter().enumerate() {
            if byte == 0xCC || byte == 0x00 {
                run += 1;
                if run >= need + 0x10 {
                    let start = offset + index as u64 + 1 - run as u64;
                    return Some((start + 0xF) & !0xF);
                }
            } else {
                run = 0;
            }
        }

        offset += take as u64;
    }

    None
}

/// Minimal two-pass assembler: raw bytes plus labels for rel32 jumps and one
/// rip-relative `jmp qword [rip+disp]` that points at the saved original.
struct Assembler {
    code: Vec<u8>,
    labels: HashMap<&'static str, usize>,
    jcc_fixups: Vec<(usize, &'static str)>,
    jmp_fixups: Vec<(usize, &'static str)>,
    jmprip_fixups: Vec<usize>,
}

impl Assembler {
    fn new() -> Self {
        Self {
            code: Vec::new(),
            labels: HashMap::new(),
            jcc_fixups: Vec::new(),
            jmp_fixups: Vec::new(),
            jmprip_fixups: Vec::new(),
        }
    }

    fn emit(&mut self, bytes: &[u8]) {
        self.code.extend_from_slice(bytes);
    }

    fn label(&mut self, name: &'static str) {
        self.labels.insert(name, self.code.len());
    }

    fn jcc(&mut self, condition: u8, label: &'static str) {
        let at = self.code.len();
        self.code.extend_from_slice(&[0x0F, condition, 0, 0, 0, 0]);
        self.jcc_fixups.push((at, label));
    }

    fn jmp(&mut self, label: &'static str) {
        let at = self.code.len();
        self.code.extend_from_slice(&[0xE9, 0, 0, 0, 0]);
        self.jmp_fixups.push((at, label));
    }

    fn jmprip(&mut self) {
        let at = self.code.len();
        self.code.extend_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]);
        self.jmprip_fixups.push(at);
    }

    /// Returns the code and the offset of the 8-byte "original pointer" slot
    /// that is written immediately after it.
    fn finish(mut self) -> (Vec<u8>, usize) {
        let code_length = self.code.len();

        for (at, label) in self.jcc_fixups {
            let target = *self.labels.get(label).expect("stub label") as i32;
            write_rel32(&mut self.code, at + 2, target - (at as i32 + 6));
        }

        for (at, label) in self.jmp_fixups {
            let target = *self.labels.get(label).expect("stub label") as i32;
            write_rel32(&mut self.code, at + 1, target - (at as i32 + 5));
        }

        for at in self.jmprip_fixups {
            write_rel32(&mut self.code, at + 2, code_length as i32 - (at as i32 + 6));
        }

        (self.code, code_length)
    }
}

fn write_rel32(code: &mut [u8], at: usize, value: i32) {
    code[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The stub's first instruction must load the real state page. Embedding a
    /// zero here (the old `self.state` read) made every patched call write to
    /// address 0x38 and killed the client on the first engine call.
    #[test]
    fn stubs_embed_the_state_page() {
        let state = 0x1234_5678_9ABC_DEF0u64;

        for (code, orig_offset) in [
            CallGate::build_stub(state),
            CallGate::build_listener_stub(state),
        ] {
            assert_eq!(&code[0..2], &[0x49, 0xBA], "mov r10, imm64");
            assert_eq!(&code[2..10], &state.to_le_bytes());

            // The orig pointer slot lives immediately after the code.
            assert_eq!(orig_offset, code.len());
        }
    }

    /// `jmp qword [rip+disp]` must land on the slot written by `install_inner`.
    #[test]
    fn stubs_jump_through_the_orig_slot() {
        let base = 0x7FF6_0000_0000u64;

        for (code, orig_offset) in [
            CallGate::build_stub(0x1000),
            CallGate::build_listener_stub(0x1000),
        ] {
            let at = code
                .windows(2)
                .position(|bytes| bytes == [0xFF, 0x25])
                .expect("rip-relative jmp");
            let rel = i32::from_le_bytes(code[at + 2..at + 6].try_into().unwrap());
            let target = base + at as u64 + 6 + rel as i64 as u64;

            assert_eq!(target, base + orig_offset as u64);
        }
    }

    /// Byte-for-byte the stub the working Python `callgate.py` emits for the
    /// same state page. These bytes run on an engine thread, so a change here
    /// has to be deliberate (and re-verified in-game).
    #[test]
    fn stubs_match_the_python_reference() {
        let state = 0x1122_3344_5566_7788u64;

        fn from_hex(hex: &str) -> Vec<u8> {
            (0..hex.len())
                .step_by(2)
                .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex"))
                .collect()
        }

        let (code, orig_offset) = CallGate::build_stub(state);
        assert_eq!(
            code,
            from_hex(concat!(
                "49ba8877665544332211f049ff423841833a000f8506000000ff25c7000000",
                "498b42404885c00f8412000000654c8b1c25480000004c39d80f85dbffffff",
                "b8010000004531dbf0450fb11a0f85c8ffffff4881ec8800000048894c2420",
                "48895424284c894424304c894c24380f114424400f114c24500f115424600f115c2470",
                "4c89942480000000498b4208498b4a10498b52184d8b42204d8b4a28ffd0",
                "4c8b9424800000004989423041c7420401000000",
                "0f104424400f104c24500f105424600f105c2470",
                "488b4c2420488b5424284c8b4424304c8b4c2438",
                "4881c488000000e933ffffff",
            ))
        );
        assert_eq!(orig_offset, 230);

        let (code, orig_offset) = CallGate::build_listener_stub(state);
        assert_eq!(
            code,
            from_hex(concat!(
                "49ba8877665544332211f049ff423849894a48498952504d8942584d894a60",
                "ff2500000000",
            ))
        );
        assert_eq!(orig_offset, 37);
    }
}
