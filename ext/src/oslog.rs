//! Public logging to the unified log via `os_log`.
//!
//! `NSLog` output is stored as a redacted argument, so it shows up as `<private>`
//! in `log stream`/`log show` unless the machine has private-data logging turned
//! on — useless for a headless extension whose only debugging channel is the log.
//! `os_log` with an explicit `%{public}s` format marks the string public, so the
//! text is always visible. All `unsafe` FFI is wrapped in the checked
//! [`log_public`] below; callers never touch it directly.

use core::ffi::{c_char, c_void};
use std::ffi::CString;
use std::sync::OnceLock;

extern "C" {
    fn os_log_create(subsystem: *const c_char, category: *const c_char) -> *mut c_void;
    fn _os_log_impl(
        dso: *mut c_void,
        log: *mut c_void,
        log_type: u8,
        format: *const c_char,
        buf: *mut u8,
        size: u32,
    );
    // Linker-provided per-image handle that `os_log` needs to resolve the format.
    static __dso_handle: c_void;
}

/// Our cached `os_log_t` (raw pointer as usize so the `OnceLock` is `Send`/`Sync`).
/// `os_log_create` returns a process-lifetime object, so we make it once.
static LOG: OnceLock<usize> = OnceLock::new();

fn log_handle() -> *mut c_void {
    let ptr = *LOG.get_or_init(|| {
        // Static, NUL-terminated C strings — construction can't fail.
        let subsystem = c"dev.lucsoft.fskit-s3";
        let category = c"ext";
        // SAFETY: both args are valid NUL-terminated C strings; the returned
        // os_log_t is owned by the system and lives for the process.
        let handle = unsafe { os_log_create(subsystem.as_ptr(), category.as_ptr()) };
        handle as usize
    });
    ptr as *mut c_void
}

/// Emit `message` to the unified log as PUBLIC text (subsystem
/// `dev.lucsoft.fskit-s3`, category `ext`), so it's readable in `log stream`
/// without private-data mode.
pub fn log_public(message: &str) {
    // os_log stores a pointer to the string; interior NULs would truncate the C
    // string, so replace them. `unwrap_or_default` keeps this panic-free.
    let sanitized: String = message.replace('\0', "?");
    let c_message = CString::new(sanitized).unwrap_or_default();

    // `_os_log_impl`'s encoded argument buffer for a single `%{public}s`:
    //   [0] 0x02  buffer has a non-scalar (string) argument
    //   [1] 0x01  one argument
    //   [2] 0x22  argument is a public (0x20) string (0x02)
    //   [3] 0x08  argument payload size = 8 (a pointer)
    //   [4..12]   the char* pointer, little-endian
    let mut buf = [0u8; 12];
    buf[0] = 0x02;
    buf[1] = 0x01;
    buf[2] = 0x22;
    buf[3] = 0x08;
    let ptr_bytes = (c_message.as_ptr() as usize as u64).to_le_bytes();
    buf[4..12].copy_from_slice(&ptr_bytes);

    let format = c"%{public}s";
    // SAFETY: `log_handle()` is a valid os_log_t; `__dso_handle` is this image's
    // handle; `format` is a static NUL-terminated string; `buf` is a 12-byte
    // buffer matching the single-`%{public}s` layout above; `c_message` outlives
    // the call, so the pointer packed into `buf` stays valid throughout.
    unsafe {
        _os_log_impl(
            core::ptr::addr_of!(__dso_handle) as *mut c_void,
            log_handle(),
            0, // OS_LOG_TYPE_DEFAULT
            format.as_ptr(),
            buf.as_mut_ptr(),
            buf.len() as u32,
        );
    }
    // Keep `c_message` alive until after the call.
    drop(c_message);
}
