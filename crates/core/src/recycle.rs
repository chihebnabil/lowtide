//! Recycle Bin via the Shell API (covers every drive at once).

use crate::fsutil::Usage;

#[cfg(windows)]
pub fn query() -> Option<Usage> {
    use windows_sys::Win32::UI::Shell::{SHQUERYRBINFO, SHQueryRecycleBinW};
    let mut info: SHQUERYRBINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHQUERYRBINFO>() as u32;
    // SAFETY: null root means "all drives"; `info` is a valid, sized out-param.
    let hr = unsafe { SHQueryRecycleBinW(std::ptr::null(), &mut info) };
    let (bytes, files) = (info.i64Size, info.i64NumItems);
    (hr >= 0).then(|| Usage { bytes: bytes.max(0) as u64, files: files.max(0) as u64 })
}

#[cfg(windows)]
pub fn empty() -> bool {
    use windows_sys::Win32::UI::Shell::{SHERB_NOCONFIRMATION, SHERB_NOPROGRESSUI, SHERB_NOSOUND, SHEmptyRecycleBinW};
    // SAFETY: null window and null root (all drives) are documented as valid.
    let hr = unsafe {
        SHEmptyRecycleBinW(std::ptr::null_mut(), std::ptr::null(), SHERB_NOCONFIRMATION | SHERB_NOPROGRESSUI | SHERB_NOSOUND)
    };
    hr >= 0
}

#[cfg(not(windows))]
pub fn query() -> Option<Usage> {
    None
}

#[cfg(not(windows))]
pub fn empty() -> bool {
    false
}
