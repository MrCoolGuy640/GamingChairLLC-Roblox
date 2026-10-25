//! Direct ntdll imports. `NtReadVirtualMemory` / `NtWriteVirtualMemory` are
//! used instead of the kernel32 wrappers for consistency with the Python SDK.

#![allow(non_snake_case)]

use std::ffi::c_void;

use windows_sys::Win32::Foundation::HANDLE;

#[link(name = "ntdll")]
extern "system" {
    pub fn NtReadVirtualMemory(
        ProcessHandle: HANDLE,
        BaseAddress: *const c_void,
        Buffer: *mut c_void,
        NumberOfBytesToRead: usize,
        NumberOfBytesRead: *mut usize,
    ) -> i32;

    pub fn NtWriteVirtualMemory(
        ProcessHandle: HANDLE,
        BaseAddress: *const c_void,
        Buffer: *const c_void,
        NumberOfBytesToWrite: usize,
        NumberOfBytesWritten: *mut usize,
    ) -> i32;

    pub fn NtQueryInformationThread(
        ThreadHandle: HANDLE,
        ThreadInformationClass: u32,
        ThreadInformation: *mut c_void,
        ThreadInformationLength: u32,
        ReturnLength: *mut u32,
    ) -> i32;
}
