//! Small Windows shell integrations: elevation, opening URLs, revealing files.

use std::path::Path;

#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn shell_execute(verb: &str, file: &str, params: Option<&str>) -> bool {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let (verb, file) = (wide(verb), wide(file));
    let params = params.map(wide);
    // SAFETY: all strings are NUL-terminated UTF-16 buffers that outlive the call.
    let h = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            verb.as_ptr(),
            file.as_ptr(),
            params.as_ref().map_or(std::ptr::null(), |p| p.as_ptr()),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    // ShellExecute returns a value > 32 on success.
    h as isize > 32
}

/// Whether the current process runs with administrator rights.
#[cfg(windows)]
pub fn is_elevated() -> bool {
    // SAFETY: no arguments, no preconditions.
    unsafe { windows_sys::Win32::UI::Shell::IsUserAnAdmin() != 0 }
}

/// Whether this process runs with MSIX package identity (the Microsoft Store build).
/// The Store build must never elevate, so callers hide "Restart as admin" when this is true.
#[cfg(windows)]
pub fn is_packaged() -> bool {
    use windows_sys::Win32::Foundation::APPMODEL_ERROR_NO_PACKAGE;
    use windows_sys::Win32::Storage::Packaging::Appx::GetCurrentPackageFullName;
    let mut len = 0u32;
    // SAFETY: a zero-length query with a null buffer only reports the required length.
    let rc = unsafe { GetCurrentPackageFullName(&mut len, std::ptr::null_mut()) };
    rc != APPMODEL_ERROR_NO_PACKAGE
}

/// Start this executable again with a UAC prompt. Returns false if the user declined,
/// or if this is the packaged Store build, which never elevates.
#[cfg(windows)]
pub fn relaunch_elevated(args: &str) -> bool {
    if is_packaged() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else { return false };
    shell_execute("runas", &exe.to_string_lossy(), (!args.is_empty()).then_some(args))
}

/// Open an https URL in the default browser.
#[cfg(windows)]
pub fn open_url(url: &str) -> bool {
    url.starts_with("https://") && shell_execute("open", url, None)
}

/// Open Explorer with `path` selected.
pub fn reveal(path: &Path) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let target = if path.exists() { path } else { path.parent().unwrap_or(path) };
        std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", target.display()))
            .spawn()
            .is_ok()
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

#[cfg(not(windows))]
pub fn is_elevated() -> bool {
    false
}

#[cfg(not(windows))]
pub fn is_packaged() -> bool {
    false
}

#[cfg(not(windows))]
pub fn relaunch_elevated(_: &str) -> bool {
    false
}

#[cfg(not(windows))]
pub fn open_url(_: &str) -> bool {
    false
}

/// Free and total bytes of the drive holding `path` (e.g. `C:\`).
#[cfg(windows)]
pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let root = wide(&path.to_string_lossy());
    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: valid NUL-terminated path; out-params are valid u64s.
    let ok = unsafe { GetDiskFreeSpaceExW(root.as_ptr(), &mut free, &mut total, std::ptr::null_mut()) };
    (ok != 0).then_some((free, total))
}

#[cfg(not(windows))]
pub fn disk_space(_: &Path) -> Option<(u64, u64)> {
    None
}
