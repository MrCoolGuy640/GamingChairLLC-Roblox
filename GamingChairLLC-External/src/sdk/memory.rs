//! Process memory access and the background memory freezer.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
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
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_ALL_ACCESS};

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

struct FreezerShared {
    memory: Arc<Memory>,
    slots: Mutex<HashMap<u64, Vec<u8>>>,
    stop: AtomicBool,
}

impl MemoryFreezer {
    pub fn start(memory: Arc<Memory>) -> Arc<Self> {
        let shared = Arc::new(FreezerShared {
            memory,
            slots: Mutex::new(HashMap::new()),
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

    pub fn freeze(&self, address: u64, data: Vec<u8>) {
        if data.is_empty() || data.len() > 64 {
            return;
        }
        self.shared.slots.lock().insert(address, data);
    }

    pub fn unfreeze(&self, address: u64) {
        self.shared.slots.lock().remove(&address);
    }

    pub fn update(&self, address: u64, data: &[u8]) {
        if let Some(slot) = self.shared.slots.lock().get_mut(&address) {
            slot.clear();
            slot.extend_from_slice(data);
        }
    }

    pub fn unfreeze_all(&self) {
        self.shared.slots.lock().clear();
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

fn freezer_loop(shared: Arc<FreezerShared>) {
    while !shared.stop.load(Ordering::Acquire) {
        let empty = {
            let slots = shared.slots.lock();
            for (address, data) in slots.iter() {
                shared.memory.write_bytes_direct(*address, data);
            }
            slots.is_empty()
        };

        thread::sleep(if empty {
            Duration::from_millis(10)
        } else {
            Duration::from_millis(1)
        });
    }
}
