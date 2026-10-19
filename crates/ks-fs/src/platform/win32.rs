//! Win32 calls. This is the only module in the workspace that uses `unsafe`; each block
//! wraps exactly one FFI call whose arguments are owned, correctly sized buffers.
#![allow(unsafe_code)]

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use ks_core::{DriveInfo, DriveKind};
use windows::core::{PCWSTR, PWSTR};
use windows::Win32::Foundation::{
    CloseHandle, LocalFree, ERROR_ACCESS_DENIED, ERROR_SUCCESS, HLOCAL,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, GetNamedSecurityInfoW, SE_FILE_OBJECT,
};
use windows::Win32::Security::{
    LookupAccountSidW, OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SID_NAME_USE,
};
use windows::Win32::Storage::FileSystem::{
    CopyFileExW, FindClose, FindFirstFileW, GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives,
    GetVolumeInformationW, COPYPROGRESSROUTINE_PROGRESS, COPY_FILE_ALLOW_DECRYPTED_DESTINATION,
    COPY_FILE_FAIL_IF_EXISTS, WIN32_FIND_DATAW,
};
use windows::Win32::System::Console::GetConsoleTitleW;
use windows::Win32::System::Diagnostics::Debug::{SetThreadErrorMode, SEM_FAILCRITICALERRORS};
use windows::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayout, VkKeyScanExW};
use windows::Win32::UI::Shell::{SHChangeNotify, SHCNE_ASSOCCHANGED, SHCNF_IDLIST};
use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId};

use crate::attrs::win;

const DRIVE_REMOVABLE: u32 = 2;
const DRIVE_FIXED: u32 = 3;
const DRIVE_REMOTE: u32 = 4;
const DRIVE_CDROM: u32 = 5;
const DRIVE_RAMDISK: u32 = 6;

/// NUL-terminated UTF-16 for the wide Win32 APIs.
fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

/// Beyond this length the wide Win32 APIs need a `\?\` prefix (MAX_PATH is 260).
const LONG_PATH_THRESHOLD: usize = 240;

/// `path` with a verbatim prefix when it is long and absolute, so `FindFirstFileW` copes
/// with the deep trees `std::fs::read_dir` already handles.
pub(crate) fn long_path_safe(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    // Already verbatim (`\\?\C:\..`) or a device path (`\\.\..`): leave it alone. This must
    // come before the UNC branch, which would otherwise mistake it for `\\server\share`.
    if text.len() < LONG_PATH_THRESHOLD || text.starts_with(r"\\?\") || text.starts_with(r"\\.\") {
        return path.to_owned();
    }
    if let Some(unc) = text.strip_prefix(r"\\") {
        return PathBuf::from(format!(r"\\?\UNC\{unc}"));
    }
    let bytes = text.as_bytes();
    if bytes.len() > 2 && bytes[1] == b':' && bytes[2] == b'\\' {
        return PathBuf::from(format!(r"\\?\{text}"));
    }
    path.to_owned()
}

fn from_wide_until_nul(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    OsString::from_wide(&buf[..end])
        .to_string_lossy()
        .into_owned()
}

/// The reparse tag of the link at `path` (read from `FindFirstFileW`, which reports it in
/// `dwReserved0` without opening the target).
pub fn reparse_tag(path: &Path) -> Option<u32> {
    let name = wide(&long_path_safe(path));
    let mut data = WIN32_FIND_DATAW::default();
    // SAFETY: `name` is NUL-terminated and outlives the call; `data` is a valid out-buffer.
    let handle = unsafe { FindFirstFileW(PCWSTR(name.as_ptr()), &mut data) }.ok()?;
    // SAFETY: `handle` was just returned by a successful `FindFirstFileW`.
    let _ = unsafe { FindClose(handle) };
    (data.dwFileAttributes & win::REPARSE_POINT != 0).then_some(data.dwReserved0)
}

/// Drives with label, file system and free/total space. Never shows the "no disk in
/// drive" system dialog; drives that cannot be queried are listed without details, and a
/// drive that does not answer within [`crate::drives::QUERY_TIMEOUT`] (a dead network
/// share) is listed as unavailable instead of blocking the list.
pub fn list_drives() -> Vec<DriveInfo> {
    list_drives_within(crate::drives::QUERY_TIMEOUT)
}

fn list_drives_within(timeout: std::time::Duration) -> Vec<DriveInfo> {
    // SAFETY: no pointers involved.
    let mask = unsafe { GetLogicalDrives() };
    let roots: Vec<PathBuf> = (0..26u8)
        .filter(|bit| mask & (1 << bit) != 0)
        .map(|bit| PathBuf::from(format!("{}:\\", char::from(b'A' + bit))))
        .collect();
    crate::drives::query_all(
        roots,
        timeout,
        std::sync::Arc::new(|root: &Path| query_drive(root.to_path_buf())),
        |root| {
            let mut info = DriveInfo::new(root);
            info.kind = drive_kind(&wide(root));
            info
        },
    )
}

fn drive_kind(wide_root: &[u16]) -> DriveKind {
    // SAFETY: `wide_root` is NUL-terminated and outlives the call.
    match unsafe { GetDriveTypeW(PCWSTR(wide_root.as_ptr())) } {
        DRIVE_REMOVABLE => DriveKind::Removable,
        DRIVE_FIXED => DriveKind::Fixed,
        DRIVE_REMOTE => DriveKind::Network,
        DRIVE_CDROM => DriveKind::Optical,
        DRIVE_RAMDISK => DriveKind::RamDisk,
        _ => DriveKind::Unknown,
    }
}

/// The character the key that types `ch` in the active keyboard layout has on a US layout:
/// `p` for the `з` of a Russian layout. Shortcuts with ctrl or alt are meant by key position,
/// but the console reports them with the character of the active layout. `None` when the
/// layout has no key for `ch` or that key has no US character.
pub fn latin_key(ch: char) -> Option<char> {
    let mut units = [0u16; 2];
    let [unit] = ch.encode_utf16(&mut units) else {
        return None;
    };
    // The same guess at the active layout as crossterm's: the layout of the foreground
    // window's thread (Windows Terminal's), falling back to ours.
    // SAFETY: no pointers are passed; a null window yields thread 0, which is valid here.
    let scan = unsafe {
        let thread = GetWindowThreadProcessId(GetForegroundWindow(), None);
        VkKeyScanExW(*unit, GetKeyboardLayout(thread))
    };
    if scan == -1 {
        return None;
    }
    crate::keyboard::us_char_for_vk(scan.to_le_bytes()[0])
}

/// A cheap fingerprint of the drives: which letters exist and which removable or optical
/// drives hold a medium. It changes when a drive is plugged in, removed or ejected. Fixed
/// and network drives are not touched, so a dead share cannot block it.
pub fn drives_signature() -> u64 {
    // SAFETY: no pointers involved.
    let mask = unsafe { GetLogicalDrives() };
    let mut signature = u64::from(mask);
    for bit in (0..26u8).filter(|bit| mask & (1 << bit) != 0) {
        let root = wide(Path::new(&format!("{}:\\", char::from(b'A' + bit))));
        if !matches!(drive_kind(&root), DriveKind::Removable | DriveKind::Optical) {
            continue;
        }
        // SAFETY: `root` is NUL-terminated and outlives the call; no out-buffers.
        let ready = without_error_dialogs(|| unsafe {
            GetVolumeInformationW(PCWSTR(root.as_ptr()), None, None, None, None, None)
        })
        .is_ok();
        if ready {
            signature |= 1 << (32 + u32::from(bit));
        }
    }
    signature
}

/// Runs `f` with the "no disk in drive" system dialog suppressed for this thread.
fn without_error_dialogs<T>(f: impl FnOnce() -> T) -> T {
    let mut previous_mode = Default::default();
    // SAFETY: `previous_mode` is a valid out-pointer for the call.
    let mode_set =
        unsafe { SetThreadErrorMode(SEM_FAILCRITICALERRORS, Some(&mut previous_mode)) }.is_ok();
    let result = f();
    if mode_set {
        // SAFETY: restores the value returned by the call above.
        let _ = unsafe { SetThreadErrorMode(previous_mode, None) };
    }
    result
}

/// Queries one drive. Runs on a thread of its own (the error mode is per thread).
fn query_drive(root: PathBuf) -> DriveInfo {
    without_error_dialogs(|| query_drive_inner(root))
}

fn query_drive_inner(root: PathBuf) -> DriveInfo {
    let wide_root = wide(&root);
    let root_ptr = PCWSTR(wide_root.as_ptr());
    let mut info = DriveInfo::new(root);

    info.kind = drive_kind(&wide_root);

    let mut label = [0u16; 261];
    let mut fs = [0u16; 261];
    // SAFETY: the buffers are valid for the lengths implied by the slices.
    let volume = unsafe {
        GetVolumeInformationW(root_ptr, Some(&mut label), None, None, None, Some(&mut fs))
    };
    if volume.is_ok() {
        info.label = from_wide_until_nul(&label);
        info.fs = from_wide_until_nul(&fs);
    }

    let (mut free, mut total) = (0u64, 0u64);
    // SAFETY: the out-pointers reference live locals.
    let space = unsafe { GetDiskFreeSpaceExW(root_ptr, Some(&mut free), Some(&mut total), None) };
    if space.is_ok() {
        info.free = Some(free);
        info.total = Some(total);
    }
    info
}

/// Local time's offset from UTC in seconds, as of now (includes daylight saving).
pub fn utc_offset_secs() -> i32 {
    const TIME_ZONE_ID_DAYLIGHT: u32 = 2;
    let mut tz = TIME_ZONE_INFORMATION::default();
    // SAFETY: `tz` is a valid out-buffer.
    let id = unsafe { GetTimeZoneInformation(&mut tz) };
    // `Bias` is minutes *west* of UTC: UTC = local + bias.
    let bias = tz.Bias
        + if id == TIME_ZONE_ID_DAYLIGHT {
            tz.DaylightBias
        } else {
            tz.StandardBias
        };
    -bias * 60
}

/// The console window's current title (what the shell set), so it can be put back on exit.
pub fn console_title() -> Option<String> {
    let mut buf = vec![0u16; 1024];
    // SAFETY: `buf` is a valid, writable buffer of the given length.
    let len = unsafe { GetConsoleTitleW(&mut buf) } as usize;
    (len > 0).then(|| String::from_utf16_lossy(&buf[..len.min(buf.len())]))
}

/// Whether a process with this id is currently running. Conservative: when the process cannot
/// be queried for another reason than "it does not exist" it counts as alive.
pub fn pid_alive(pid: u32) -> bool {
    const STILL_ACTIVE: u32 = 259;
    // SAFETY: plain value arguments; the returned handle is closed below.
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => handle,
        Err(err) => return err.code() == ERROR_ACCESS_DENIED.to_hresult(),
    };
    let mut code = 0u32;
    // SAFETY: `handle` is a valid process handle and `code` a valid out-pointer.
    let queried = unsafe { GetExitCodeProcess(handle, &mut code) }.is_ok();
    // SAFETY: `handle` came from `OpenProcess` and is closed exactly once.
    let _ = unsafe { CloseHandle(handle) };
    !queried || code == STILL_ACTIVE
}

/// Tells the shell that file associations changed, so Explorer picks up new or removed
/// verbs without a sign-out.
pub fn notify_association_changed() {
    // SAFETY: SHCNE_ASSOCCHANGED takes no items; both item pointers are null.
    unsafe { SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None) };
}

/// Owner of `path` as `DOMAIN\user`, or the SID string when the account cannot be resolved
/// (orphaned SID, offline domain). `None` when the owner cannot be read at all (no security
/// descriptor on FAT/exFAT, access denied). Blocking: the preview worker threads call it.
pub fn file_owner(path: &Path) -> Option<String> {
    let name = wide(path);
    let mut owner = PSID::default();
    let mut descriptor = PSECURITY_DESCRIPTOR::default();
    // SAFETY: `name` is NUL-terminated and outlives the call; the out-pointers reference live
    // locals. On success `owner` points into `descriptor`, which is freed below.
    let status = unsafe {
        GetNamedSecurityInfoW(
            PCWSTR(name.as_ptr()),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION,
            Some(&mut owner),
            None,
            None,
            None,
            &mut descriptor,
        )
    };
    if status != ERROR_SUCCESS {
        return None;
    }
    // A descriptor without an owner still has to be freed.
    let result = if owner.0.is_null() {
        None
    } else {
        account_name(owner).or_else(|| sid_string(owner))
    };
    // SAFETY: `descriptor` was allocated by GetNamedSecurityInfoW with LocalAlloc.
    let _ = unsafe { LocalFree(Some(HLOCAL(descriptor.0))) };
    result
}

/// `DOMAIN\user` for a SID.
fn account_name(sid: PSID) -> Option<String> {
    let (mut name_len, mut domain_len) = (0u32, 0u32);
    let mut kind = SID_NAME_USE::default();
    // SAFETY: the first call only asks for the buffer sizes (null buffers, zero lengths).
    let _ = unsafe {
        LookupAccountSidW(
            PCWSTR::null(),
            sid,
            None,
            &mut name_len,
            None,
            &mut domain_len,
            &mut kind,
        )
    };
    if name_len == 0 {
        return None;
    }
    let mut name = vec![0u16; name_len as usize];
    let mut domain = vec![0u16; domain_len.max(1) as usize];
    // SAFETY: both buffers have the lengths reported by the first call.
    unsafe {
        LookupAccountSidW(
            PCWSTR::null(),
            sid,
            Some(PWSTR(name.as_mut_ptr())),
            &mut name_len,
            Some(PWSTR(domain.as_mut_ptr())),
            &mut domain_len,
            &mut kind,
        )
    }
    .ok()?;
    let user = from_wide_until_nul(&name);
    let domain = from_wide_until_nul(&domain);
    Some(if domain.is_empty() {
        user
    } else {
        format!("{domain}\\{user}")
    })
}

/// `S-1-5-21-...` for a SID.
fn sid_string(sid: PSID) -> Option<String> {
    let mut text = PWSTR::null();
    // SAFETY: `text` receives a LocalAlloc'd NUL-terminated string, freed below.
    unsafe { ConvertSidToStringSidW(sid, &mut text) }.ok()?;
    // SAFETY: on success `text` is a valid NUL-terminated wide string.
    let out = unsafe { text.to_string() }.ok();
    // SAFETY: allocated by ConvertSidToStringSidW with LocalAlloc.
    let _ = unsafe { LocalFree(Some(HLOCAL(text.0.cast()))) };
    out
}

/// Progress sink for [`copy_file_ex`]: receives the number of bytes transferred since the
/// previous call and returns `false` to cancel the copy.
pub type CopyProgress<'a> = dyn FnMut(u64) -> bool + 'a;

struct CopyState<'a> {
    progress: &'a mut CopyProgress<'a>,
    last: i64,
}

unsafe extern "system" fn copy_routine(
    _total_size: i64,
    total_transferred: i64,
    _stream_size: i64,
    _stream_transferred: i64,
    _stream_number: u32,
    _reason: windows::Win32::Storage::FileSystem::LPPROGRESS_ROUTINE_CALLBACK_REASON,
    _src: windows::Win32::Foundation::HANDLE,
    _dst: windows::Win32::Foundation::HANDLE,
    data: *const core::ffi::c_void,
) -> COPYPROGRESSROUTINE_PROGRESS {
    // SAFETY: `data` is the `CopyState` pointer handed to CopyFileExW by `copy_file_ex`,
    // which outlives the call and is only touched from this (the calling) thread.
    let state = unsafe { &mut *(data as *mut CopyState<'_>) };
    let delta = (total_transferred - state.last).max(0) as u64;
    state.last = total_transferred;
    if (state.progress)(delta) {
        windows::Win32::Storage::FileSystem::PROGRESS_CONTINUE
    } else {
        windows::Win32::Storage::FileSystem::PROGRESS_CANCEL
    }
}

/// Copy a file with `CopyFileExW`, which carries alternate data streams, attributes, creation
/// time and security info along. Fails if `dst` exists. Returns `Ok(false)` if `progress`
/// cancelled (the partial `dst` is removed by the system or left for the caller to remove).
pub fn copy_file_ex(
    src: &Path,
    dst: &Path,
    progress: &mut CopyProgress<'_>,
) -> std::io::Result<bool> {
    let wide = |p: &Path| -> Vec<u16> { p.as_os_str().encode_wide().chain(Some(0)).collect() };
    // Verbatim (`\?\`) paths so deep trees beyond MAX_PATH copy like they do with std.
    // Only the parent is canonicalized: the final component must stay as given so a link is
    // never silently resolved to its target here.
    let verbatim = |p: &Path| -> std::io::Result<std::path::PathBuf> {
        match (p.parent(), p.file_name()) {
            (Some(parent), Some(name)) if !parent.as_os_str().is_empty() => {
                Ok(std::fs::canonicalize(parent)?.join(name))
            }
            _ => Ok(p.to_path_buf()),
        }
    };
    let src = verbatim(src)?;
    let dst = verbatim(dst)?;
    let (s, d) = (wide(&src), wide(&dst));
    let mut state = CopyState { progress, last: 0 };
    // SAFETY: both paths are NUL-terminated and live across the call; `state` outlives it and
    // the routine only dereferences it as a `CopyState`.
    let r = unsafe {
        CopyFileExW(
            PCWSTR(s.as_ptr()),
            PCWSTR(d.as_ptr()),
            Some(copy_routine),
            Some((&mut state as *mut CopyState<'_>).cast_const().cast()),
            None,
            // A decrypted destination is allowed so EFS files still copy to FAT/exFAT or shares.
            COPY_FILE_FAIL_IF_EXISTS | COPY_FILE_ALLOW_DECRYPTED_DESTINATION,
        )
    };
    match r {
        Ok(()) => Ok(true),
        // ERROR_REQUEST_ABORTED (1235): the progress routine asked to cancel.
        Err(e) if e.code().0 as u32 & 0xFFFF == 1235 => Ok(false),
        Err(e) => Err(std::io::Error::from_raw_os_error(
            (e.code().0 as u32 & 0xFFFF) as i32,
        )),
    }
}

#[cfg(test)]
mod long_path_tests {
    use super::*;

    fn long(prefix: &str) -> String {
        format!("{prefix}{}", "a\\".repeat(130))
    }

    #[test]
    fn long_drive_and_unc_paths_get_a_two_backslash_verbatim_prefix() {
        let drive = long("C:\\");
        let out = long_path_safe(Path::new(&drive));
        assert_eq!(out.to_string_lossy(), format!("\\\\?\\{drive}"));
        let unc = long("\\\\srv\\share\\");
        let out = long_path_safe(Path::new(&unc));
        assert_eq!(out.to_string_lossy(), format!("\\\\?\\UNC\\{}", &unc[2..]));
    }

    #[test]
    fn verbatim_short_and_relative_paths_are_unchanged() {
        let verbatim = long("\\\\?\\C:\\");
        assert_eq!(long_path_safe(Path::new(&verbatim)), Path::new(&verbatim));
        assert_eq!(
            long_path_safe(Path::new("C:\\short")),
            Path::new("C:\\short")
        );
        let rel = "a\\".repeat(150);
        assert_eq!(long_path_safe(Path::new(&rel)), Path::new(&rel));
    }

    #[test]
    fn junction_in_a_250_char_path_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let mut deep = dir.path().to_path_buf();
        while deep.as_os_str().len() < 260 {
            deep.push("segment_of_some_length_0123456789");
        }
        let verbatim = PathBuf::from(format!("\\\\?\\{}", deep.display()));
        std::fs::create_dir_all(&verbatim).unwrap();
        let target = verbatim.join("target");
        std::fs::create_dir(&target).unwrap();
        let link = verbatim.join("link");
        // mklink /J needs no privileges; skip when cmd is unavailable.
        let status = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&link)
            .arg(&target)
            .output();
        if !status.is_ok_and(|o| o.status.success()) {
            return;
        }
        let plain = deep.join("link");
        assert!(plain.as_os_str().len() >= LONG_PATH_THRESHOLD);
        assert_eq!(
            reparse_tag(&plain),
            Some(0xA000_0003),
            "IO_REPARSE_TAG_MOUNT_POINT"
        );
    }
}

#[cfg(test)]
mod owner_tests {
    use super::*;

    #[test]
    fn owner_of_a_temp_file_is_a_domain_user_or_sid() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let owner = file_owner(&file).expect("owner of a file we just made");
        // The creator normally owns it (or BUILTIN\Administrators for an elevated token).
        let user = std::env::var("USERNAME").unwrap_or_default();
        assert!(
            owner.contains('\\') && owner.to_lowercase().contains(&user.to_lowercase())
                || owner.starts_with("S-1-")
                || owner.contains("Administrators"),
            "{owner}"
        );
        assert_eq!(file_owner(&dir.path().join("missing")), None);
    }
}
