//! Win32 process / thread / module helpers.
//!
//! Replaces the psutil + ctypes code from the Python SDK with direct
//! Toolhelp32 / psapi calls.

use std::ffi::c_void;
use std::mem::size_of;
use std::path::PathBuf;

use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, Thread32First, Thread32Next,
    PROCESSENTRY32W, TH32CS_SNAPPROCESS, TH32CS_SNAPTHREAD, THREADENTRY32,
};
use windows_sys::Win32::System::ProcessStatus::{
    EnumProcessModules, GetModuleBaseNameW, GetModuleInformation, MODULEINFO,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, OpenThread, QueryFullProcessImageNameW, PROCESS_QUERY_INFORMATION,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ, THREAD_QUERY_INFORMATION,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use super::ntdll::NtQueryInformationThread;

const STILL_ACTIVE: u32 = 259;
const THREAD_QUERY_SET_WIN32_START_ADDRESS: u32 = 9;

#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub exe_name: String,
    pub exe_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct ModuleInfo {
    pub name: String,
    pub base: usize,
    pub size: usize,
}

/// Find the first process whose executable name matches `name` (case-insensitive).
pub fn find_process(name: &str) -> Option<ProcessInfo> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
            return None;
        }

        let mut entry = PROCESSENTRY32W::default();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;

        let mut next = Process32FirstW(snapshot, &mut entry) != 0;
        while next {
            let exe_name = wide_to_string(&entry.szExeFile);
            if exe_name.eq_ignore_ascii_case(name) {
                let pid = entry.th32ProcessID;
                let exe_path = process_image_path(pid);
                CloseHandle(snapshot);
                return Some(ProcessInfo {
                    pid,
                    exe_name,
                    exe_path,
                });
            }
            next = Process32NextW(snapshot, &mut entry) != 0;
        }

        CloseHandle(snapshot);
    }

    None
}

/// Full path to the executable of `pid`, if it can be queried.
pub fn process_image_path(pid: u32) -> Option<PathBuf> {
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }

        let mut buffer = vec![0u16; 32_768];
        let mut size = buffer.len() as u32;
        let ok = QueryFullProcessImageNameW(handle, 0, buffer.as_mut_ptr(), &mut size);
        CloseHandle(handle);

        if ok == 0 {
            return None;
        }

        Some(PathBuf::from(String::from_utf16_lossy(
            &buffer[..size as usize],
        )))
    }
}

/// PID that owns the foreground (focused) window, if any.
pub fn foreground_pid() -> Option<u32> {
    unsafe {
        let window = GetForegroundWindow();
        if window.is_null() {
            return None;
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(window, &mut pid);
        (pid != 0).then_some(pid)
    }
}

/// Cheap liveness check: the process exists and has not exited.
pub fn is_pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }

        let mut exit_code = 0u32;
        let ok = GetExitCodeProcess(handle, &mut exit_code);
        CloseHandle(handle);

        // If the exit code cannot be read, assume the process is alive rather
        // than dropping a valid attachment.
        ok == 0 || exit_code == STILL_ACTIVE
    }
}

/// All thread IDs owned by `pid`.
pub fn thread_ids(pid: u32) -> Vec<u32> {
    let mut out = Vec::new();

    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot.is_null() || snapshot == INVALID_HANDLE_VALUE {
            return out;
        }

        let mut entry = THREADENTRY32::default();
        entry.dwSize = size_of::<THREADENTRY32>() as u32;

        let mut next = Thread32First(snapshot, &mut entry) != 0;
        while next {
            if entry.th32OwnerProcessID == pid {
                out.push(entry.th32ThreadID);
            }
            next = Thread32Next(snapshot, &mut entry) != 0;
        }

        CloseHandle(snapshot);
    }

    out
}

/// The Win32 start address of a thread (used to tell Hyperion threads apart).
pub fn thread_start_address(tid: u32) -> Option<usize> {
    unsafe {
        let handle = OpenThread(THREAD_QUERY_INFORMATION, 0, tid);
        if handle.is_null() {
            return None;
        }

        let mut address: usize = 0;
        let status = NtQueryInformationThread(
            handle,
            THREAD_QUERY_SET_WIN32_START_ADDRESS,
            &mut address as *mut usize as *mut c_void,
            size_of::<usize>() as u32,
            std::ptr::null_mut(),
        );
        CloseHandle(handle);

        if status == 0 {
            Some(address)
        } else {
            None
        }
    }
}

/// Modules loaded by `pid` with their base address and image size.
pub fn modules(pid: u32) -> Vec<ModuleInfo> {
    let mut out = Vec::new();

    unsafe {
        let process = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid);
        if process.is_null() {
            return out;
        }

        let mut needed = 0u32;
        if EnumProcessModules(
            process,
            std::ptr::null_mut(),
            0,
            &mut needed,
        ) != 0
        {
            let count = (needed as usize / size_of::<HANDLE>()).max(1);
            let mut handles: Vec<HANDLE> = vec![std::ptr::null_mut(); count];

            if EnumProcessModules(process, handles.as_mut_ptr(), needed, &mut needed) != 0 {
                let count = (needed as usize / size_of::<HANDLE>()).min(handles.len());
                for module in handles.into_iter().take(count) {
                    if module.is_null() {
                        continue;
                    }

                    let mut name = [0u16; 260];
                    let name_len =
                        GetModuleBaseNameW(process, module, name.as_mut_ptr(), name.len() as u32);
                    if name_len == 0 {
                        continue;
                    }

                    let mut info = MODULEINFO::default();
                    if GetModuleInformation(
                        process,
                        module,
                        &mut info,
                        size_of::<MODULEINFO>() as u32,
                    ) != 0
                    {
                        out.push(ModuleInfo {
                            name: String::from_utf16_lossy(&name[..name_len as usize]),
                            base: info.lpBaseOfDll as usize,
                            size: info.SizeOfImage as usize,
                        });
                    }
                }
            }
        }

        CloseHandle(process);
    }

    out
}

/// `ntdll.dll+0x1234` style name for an address, or `0x1234` if unmapped.
pub fn resolve_address(address: usize, modules: &[ModuleInfo]) -> String {
    for module in modules {
        if address >= module.base && address < module.base.saturating_add(module.size) {
            return format!("{}+0x{:x}", module.name, address - module.base);
        }
    }

    format!("0x{address:x}")
}

pub fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|&value| value == 0)
        .unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}
