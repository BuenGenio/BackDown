//! The few Win32 calls std doesn't make: a privilege for this process, and a file's streams.
//! Declared here rather than through a bindings crate; kernel32 and advapi32 ship with Windows.

use std::ffi::c_void;
use std::io;
use std::path::Path;

type Handle = *mut c_void;
const INVALID_HANDLE_VALUE: Handle = -1isize as Handle;
const TOKEN_ADJUST_PRIVILEGES: u32 = 0x0020;
const TOKEN_QUERY: u32 = 0x0008;
const SE_PRIVILEGE_ENABLED: u32 = 0x0000_0002;
const ERROR_HANDLE_EOF: i32 = 38;
const ERROR_INVALID_PARAMETER: i32 = 87;
const ERROR_NOT_ALL_ASSIGNED: i32 = 1300;
/// `FindStreamInfoStandard`
const FIND_STREAM_INFO_STANDARD: i32 = 0;

#[repr(C)]
#[derive(Clone, Copy)]
struct Luid {
    low: u32,
    high: i32,
}

#[repr(C)]
struct LuidAndAttributes {
    luid: Luid,
    attributes: u32,
}

#[repr(C)]
struct TokenPrivileges {
    count: u32,
    privileges: [LuidAndAttributes; 1],
}

/// `WIN32_FIND_STREAM_DATA`: the stream's size, and its name (`:name:$DATA`), MAX_PATH + 36 units
#[repr(C)]
struct FindStreamData {
    size: i64,
    name: [u16; 260 + 36],
}

#[link(name = "kernel32")]
unsafe extern "system" {
    safe fn GetCurrentProcess() -> Handle;
    fn CloseHandle(handle: Handle) -> i32;
    fn FindFirstStreamW(
        file_name: *const u16,
        info_level: i32,
        data: *mut FindStreamData,
        flags: u32,
    ) -> Handle;
    fn FindNextStreamW(find: Handle, data: *mut FindStreamData) -> i32;
    fn FindClose(find: Handle) -> i32;
}

#[link(name = "advapi32")]
unsafe extern "system" {
    fn OpenProcessToken(process: Handle, access: u32, token: *mut Handle) -> i32;
    fn LookupPrivilegeValueW(system: *const u16, name: *const u16, luid: *mut Luid) -> i32;
    fn AdjustTokenPrivileges(
        token: Handle,
        disable_all: i32,
        new_state: *const TokenPrivileges,
        buffer_length: u32,
        previous: *mut c_void,
        return_length: *mut u32,
    ) -> i32;
}

fn wide_z(s: &std::ffi::OsStr) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    s.encode_wide().chain(std::iter::once(0)).collect()
}

/// Enable one of this process's privileges (`SeBackupPrivilege`). Whether it's now on: false
/// when the process doesn't hold it (not elevated).
pub fn enable_privilege(name: &str) -> bool {
    let name = wide_z(std::ffi::OsStr::new(name));
    let mut token: Handle = std::ptr::null_mut();
    // SAFETY: plain Win32 calls with valid pointers to locals; the token is closed below
    unsafe {
        if OpenProcessToken(
            GetCurrentProcess(),
            TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY,
            &mut token,
        ) == 0
        {
            return false;
        }
        let mut luid = Luid { low: 0, high: 0 };
        let ok = LookupPrivilegeValueW(std::ptr::null(), name.as_ptr(), &mut luid) != 0 && {
            let tp = TokenPrivileges {
                count: 1,
                privileges: [LuidAndAttributes {
                    luid,
                    attributes: SE_PRIVILEGE_ENABLED,
                }],
            };
            let adjusted =
                AdjustTokenPrivileges(token, 0, &tp, 0, std::ptr::null_mut(), std::ptr::null_mut())
                    != 0;
            // it "succeeds" without the privilege, saying so only through the last error
            adjusted && io::Error::last_os_error().raw_os_error() != Some(ERROR_NOT_ALL_ASSIGNED)
        };
        CloseHandle(token);
        ok
    }
}

/// Every stream of a file as Windows names it (`::$DATA` for the main one, `:name:$DATA` for
/// alternate ones), with its size. Empty on a filesystem without streams (FAT, exFAT).
pub fn streams(path: &Path) -> io::Result<Vec<(String, u64)>> {
    let p = crate::paths::os(path);
    let name = wide_z(p.as_os_str());
    let mut data = FindStreamData {
        size: 0,
        name: [0; 260 + 36],
    };
    let mut out = Vec::new();
    // SAFETY: `name` is NUL-terminated, `data` is a valid WIN32_FIND_STREAM_DATA, and the find
    // handle is closed on every path out
    unsafe {
        let find = FindFirstStreamW(name.as_ptr(), FIND_STREAM_INFO_STANDARD, &mut data, 0);
        if find == INVALID_HANDLE_VALUE {
            let e = io::Error::last_os_error();
            return match e.raw_os_error() {
                Some(ERROR_HANDLE_EOF) | Some(ERROR_INVALID_PARAMETER) => Ok(out),
                _ => Err(e),
            };
        }
        loop {
            let len = data
                .name
                .iter()
                .position(|&c| c == 0)
                .unwrap_or(data.name.len());
            out.push((
                String::from_utf16_lossy(&data.name[..len]),
                data.size.max(0) as u64,
            ));
            if FindNextStreamW(find, &mut data) == 0 {
                let e = io::Error::last_os_error();
                FindClose(find);
                return match e.raw_os_error() {
                    Some(ERROR_HANDLE_EOF) => Ok(out),
                    _ => Err(e),
                };
            }
        }
    }
}
