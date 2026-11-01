//! Process image name and `Name.exe (Description)` labels.

use std::ffi::c_void;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{PCWSTR, PWSTR, w};

/// Lowercased file name of `pid`.
pub fn exe_name(pid: u32) -> Option<String> {
    let path = image_path(pid)?;
    Some(base_name(&path)?.to_ascii_lowercase())
}

/// `"Name.exe (Description)"`, or just `"Name.exe"` when there is none.
pub fn label(pid: u32) -> Option<String> {
    let path = image_path(pid)?;
    let name = base_name(&path)?;
    Some(match description(&path) {
        Some(d) => format!("{name} ({d})"),
        None => name.to_string(),
    })
}

fn image_path(pid: u32) -> Option<String> {
    // SAFETY: handle is closed below; buffer and length are valid.
    let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buf = [0u16; 520];
    let mut len = buf.len() as u32;
    let r = unsafe {
        QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &mut len)
    };
    unsafe {
        let _ = CloseHandle(h);
    };
    r.ok()?;
    Some(String::from_utf16_lossy(&buf[..len as usize]))
}

fn base_name(path: &str) -> Option<&str> {
    path.rsplit(['\\', '/']).next().filter(|s| !s.is_empty())
}

fn description(path: &str) -> Option<String> {
    let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
    let name = PCWSTR(wide.as_ptr());
    let mut dummy = 0u32;
    // SAFETY: fixed query on a NUL-terminated path.
    let size = unsafe { GetFileVersionInfoSizeW(name, Some(&mut dummy)) };
    if size == 0 {
        return None;
    }
    // Version data must be at least DWORD aligned.
    let mut data = vec![0u32; (size as usize).div_ceil(4)];
    // SAFETY: `data` holds at least `size` bytes, matching the requested size.
    if unsafe { GetFileVersionInfoW(name, Some(0), size, data.as_mut_ptr().cast()) }.is_err() {
        return None;
    }

    let mut trans: *mut c_void = std::ptr::null_mut();
    let mut trans_len = 0u32;
    // SAFETY: block pointer is valid; translation block is two u16s.
    if !unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            w!("\\VarFileInfo\\Translation"),
            &mut trans,
            &mut trans_len,
        )
    }
    .as_bool()
        || trans_len < 4
    {
        return None;
    }
    // SAFETY: `trans` points at one or more LANGID/CODEPAGE pairs; first one used.
    let (lang, cp) = unsafe {
        (
            std::ptr::read_unaligned(trans.cast::<u16>()),
            std::ptr::read_unaligned((trans as *const u8).add(2).cast::<u16>()),
        )
    };
    let sub = format!("\\StringFileInfo\\{lang:04x}{cp:04x}\\FileDescription");
    let sub: Vec<u16> = sub.encode_utf16().chain([0]).collect();

    let mut ptr: *mut c_void = std::ptr::null_mut();
    let mut len = 0u32;
    // SAFETY: sub-block is NUL-terminated and queried against the same block.
    if !unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            PCWSTR(sub.as_ptr()),
            &mut ptr,
            &mut len,
        )
    }
    .as_bool()
        || ptr.is_null()
        || len == 0
    {
        return None;
    }
    // SAFETY: len counts UTF-16 units (including NUL) in the version block.
    let s = unsafe {
        let units: Vec<u16> = (0..len as usize)
            .map(|i| std::ptr::read_unaligned((ptr as *const u8).add(i * 2).cast::<u16>()))
            .collect();
        String::from_utf16_lossy(&units)
    };
    let s = s.trim_matches(['\0', ' ']);
    (!s.is_empty()).then(|| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn own_process_label() {
        let me = std::process::id();
        let label = label(me).expect("own process must resolve");
        let exe = exe_name(me).expect("own exe name");
        assert!(
            label.to_ascii_lowercase().starts_with(&exe),
            "{label} vs {exe}"
        );
    }

    #[test]
    fn missing_pid_is_none() {
        assert_eq!(exe_name(0xFFFF_FFF0), None);
        assert_eq!(label(0xFFFF_FFF0), None);
    }
}
