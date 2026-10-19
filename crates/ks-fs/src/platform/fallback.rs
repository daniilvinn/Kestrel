//! Fallbacks for non-Windows hosts (CI and development; Windows is the primary target).

use std::path::{Path, PathBuf};

use ks_core::{DriveInfo, DriveKind};

/// Only Windows has junction reparse points.
pub fn reparse_tag(_path: &Path) -> Option<u32> {
    None
}

/// The file system root as the single "drive".
pub fn list_drives() -> Vec<DriveInfo> {
    let mut root = DriveInfo::new(PathBuf::from("/"));
    root.kind = DriveKind::Fixed;
    vec![root]
}

/// Terminals elsewhere report shortcuts by key already.
pub fn latin_key(_ch: char) -> Option<char> {
    None
}

/// The drive set never changes on other hosts.
pub fn drives_signature() -> u64 {
    0
}

/// Owners are only resolved on Windows.
pub fn file_owner(_path: &Path) -> Option<String> {
    None
}

/// Terminals offer no portable way to read the window title.
pub fn console_title() -> Option<String> {
    None
}

/// Dates are shown in UTC on non-Windows hosts.
pub fn utc_offset_secs() -> i32 {
    0
}

/// Whether a process with this id is running (Linux: `/proc`; elsewhere assumed alive).
pub fn pid_alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{pid}")).exists()
    } else {
        true
    }
}
