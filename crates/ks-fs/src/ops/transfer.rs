//! Copy, move and rename: the recursive transfer algorithm with conflict handling.
//!
//! Policies (also documented in the module docs of [`crate::ops`]):
//! - Symlinks and junctions are never followed. Copying a link recreates the link (as a
//!   symlink; on Windows this needs the symlink privilege / developer mode) and is reported as
//!   an item error ("link skipped") if that fails. Moving a link on the same volume renames the
//!   link itself.
//! - A directory transferred onto an existing directory is merged; conflicts are decided per
//!   file. Other kinds of conflict (file onto file, file onto directory, ...) go through the
//!   conflict policy.
//! - Files are copied to a hidden temporary sibling (`.ks-<job>-<n>.part`) and renamed into
//!   place, so cancelling or failing never leaves a partial target and never destroys a file
//!   that was being overwritten.
//! - Any other overwrite (moves, directories) first renames the existing destination to a
//!   hidden sibling (`.<name>.ks-old-*`), then transfers, then deletes the sibling; on cancel
//!   or failure the old item is put back (or kept beside the result if the transfer only
//!   partly failed).

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use super::names::{keep_both_path, validate_name};
use super::paths::{clear_readonly, is_cross_device, link_is_dir, path_key};
use super::runner::{Flow, Runner, CHUNK};
use super::types::{ConflictChoice, FileMeta, JobEvent, JobKind, Outcome, TransferItem};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Op {
    Copy,
    Move,
}

#[derive(Default)]
pub(crate) struct Counts {
    pub files: u64,
    pub bytes: u64,
}

/// Rename `path` to a unique hidden sibling name and return it.
fn park_aside(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    for i in 0..100u32 {
        let cand = path.with_file_name(format!(".{name}.ks-old-{nanos}-{i}"));
        if fs::symlink_metadata(&cand).is_err() {
            fs::rename(path, &cand)?;
            return Ok(cand);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free backup name",
    ))
}

/// Best-effort, non-cancellable removal of a file, link or tree (clears read-only first).
fn force_remove(path: &Path) {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return;
    };
    if meta.is_dir() && !meta.file_type().is_symlink() {
        if let Ok(rd) = fs::read_dir(path) {
            for e in rd.flatten() {
                force_remove(&e.path());
            }
        }
        clear_readonly(path);
        let _ = fs::remove_dir(path);
    } else {
        clear_readonly(path);
        if fs::remove_file(path).is_err() && link_is_dir(&meta) {
            let _ = fs::remove_dir(path);
        }
    }
}

impl Runner {
    /// Walk `path` counting files and bytes without following links. Cancellable.
    pub(crate) fn scan_path(
        &mut self,
        path: &Path,
        counts: &mut Counts,
        report: bool,
        last: &mut Instant,
    ) -> Flow {
        self.check()?;
        let Ok(meta) = fs::symlink_metadata(path) else {
            return Ok(());
        };
        if meta.is_dir() {
            if let Ok(rd) = fs::read_dir(path) {
                for entry in rd.flatten() {
                    self.scan_path(&entry.path(), counts, report, last)?;
                }
            }
        } else {
            counts.files += 1;
            counts.bytes += meta.len();
            if report && last.elapsed() >= std::time::Duration::from_millis(100) {
                *last = Instant::now();
                self.emit(JobEvent::Scanning {
                    job: self.id,
                    generation: self.generation,
                    files: counts.files,
                    bytes: counts.bytes,
                });
            }
        }
        Ok(())
    }

    pub(crate) fn run_copy(&mut self, items: Vec<TransferItem>) -> Flow {
        let mut counts = Counts::default();
        let mut last = Instant::now();
        for it in &items {
            self.scan_path(&it.src, &mut counts, true, &mut last)?;
        }
        self.files_total = counts.files;
        self.bytes_total = counts.bytes;
        // Always report the final totals so the UI sees at least one Scanning event.
        self.emit(JobEvent::Scanning {
            job: self.id,
            generation: self.generation,
            files: counts.files,
            bytes: counts.bytes,
        });
        self.tick(true);
        for it in items {
            self.transfer(Op::Copy, &it.src, it.dst)?;
        }
        Ok(())
    }

    /// Move / rename. No pre-scan: same-volume moves are O(1) renames, so totals start as the
    /// item count and grow when an item falls back to copy + delete.
    pub(crate) fn run_move(&mut self, items: Vec<TransferItem>) -> Flow {
        self.files_total = items.len() as u64;
        self.tick(true);
        for it in items {
            self.transfer(Op::Move, &it.src, it.dst)?;
        }
        Ok(())
    }

    /// Ordered renames that never overwrite and stop at the first failure.
    pub(crate) fn run_steps(&mut self, steps: Vec<TransferItem>) -> Flow {
        self.files_total = steps.len() as u64;
        self.tick(true);
        for s in steps {
            self.check()?;
            self.set_current(&s.src);
            let case_only = path_key(&s.src) == path_key(&s.dst);
            if !case_only && fs::symlink_metadata(&s.dst).is_ok() {
                self.error(&s.dst, "already exists");
                break;
            }
            match fs::rename(&s.src, &s.dst) {
                Ok(()) => {
                    self.outcomes.push(Outcome::Renamed {
                        from: s.src,
                        to: s.dst,
                    });
                    self.items_done += 1;
                    self.file_done();
                }
                Err(e) => {
                    self.error(&s.src, e);
                    break;
                }
            }
        }
        Ok(())
    }

    fn account_skipped(&mut self, op: Op, src: &Path, meta: &Metadata) {
        self.items_skipped += 1;
        if op == Op::Move {
            self.files_done += 1;
        } else if meta.is_dir() {
            let mut c = Counts::default();
            let mut t = Instant::now();
            let _ = self.scan_path(src, &mut c, false, &mut t);
            self.files_done += c.files;
            self.bytes_done += c.bytes;
        } else {
            self.files_done += 1;
            self.bytes_done += meta.len();
        }
        self.tick(false);
    }

    fn record(&mut self, op: Op, src: &Path, dst: &Path) {
        let (src, dst) = (src.to_path_buf(), dst.to_path_buf());
        self.outcomes.push(match (op, self.kind) {
            (Op::Copy, _) => Outcome::Copied { src, dst },
            (Op::Move, JobKind::Rename | JobKind::BulkRename) => {
                Outcome::Renamed { from: src, to: dst }
            }
            (Op::Move, _) => Outcome::Moved { src, dst },
        });
    }

    /// Transfer one source to an exact destination path, resolving conflicts.
    pub(crate) fn transfer(&mut self, op: Op, src: &Path, mut dst: PathBuf) -> Flow {
        self.check()?;
        self.set_current(src);
        let smeta = match fs::symlink_metadata(src) {
            Ok(m) => m,
            Err(e) => {
                self.error(src, e);
                return Ok(());
            }
        };
        if smeta.is_dir() {
            let (sk, dk) = (path_key(src), path_key(&dst));
            if dk.starts_with(&format!("{sk}\\")) || dk.starts_with(&format!("{sk}/")) {
                self.error(src, "cannot transfer a folder into itself");
                return Ok(());
            }
        }
        let mut replaced = false;
        let mut backup: Option<PathBuf> = None;
        loop {
            let dmeta = match fs::symlink_metadata(&dst) {
                Ok(m) => m,
                Err(e) if e.kind() == io::ErrorKind::NotFound => break,
                Err(e) => {
                    self.error(&dst, e);
                    return Ok(());
                }
            };
            let same = path_key(src) == path_key(&dst)
                || matches!((dunce::canonicalize(src), dunce::canonicalize(&dst)),
                    (Ok(a), Ok(b)) if path_key(&a) == path_key(&b));
            if same && op == Op::Move {
                if src != dst && path_key(src) == path_key(&dst) {
                    break; // case-only rename
                }
                if src != dst {
                    // Different paths, same file (e.g. via a link): nothing sensible to do.
                    self.error(src, "source and destination are the same item");
                    return Ok(());
                }
                // Moving onto itself is a no-op.
                self.items_done += 1;
                self.file_done();
                return Ok(());
            }
            if smeta.is_dir() && dmeta.is_dir() && !same {
                return self.merge_dirs(op, src, &dst);
            }
            let (sm, dm) = (
                FileMeta::from_metadata(&smeta),
                FileMeta::from_metadata(&dmeta),
            );
            match self.resolve_conflict(src, &dst, sm, dm)? {
                ConflictChoice::Skip | ConflictChoice::Cancel => {
                    self.account_skipped(op, src, &smeta);
                    return Ok(());
                }
                ConflictChoice::Overwrite => {
                    if same {
                        self.error(src, "cannot overwrite an item with itself");
                        return Ok(());
                    }
                    replaced = true;
                    let plain_files = smeta.is_file() && dmeta.is_file();
                    if !(op == Op::Copy && plain_files) {
                        // Park the old item instead of deleting it; it is dropped only once
                        // the replacement is in place, and restored on cancel/failure.
                        match park_aside(&dst) {
                            Ok(p) => backup = Some(p),
                            Err(e) => {
                                self.error(&dst, format!("cannot replace: {e}"));
                                return Ok(());
                            }
                        }
                    }
                    break;
                }
                ConflictChoice::Rename(name) => {
                    if let Err(e) = validate_name(&name) {
                        self.error(&dst, format!("invalid name {name:?}: {e}"));
                        return Ok(());
                    }
                    dst = dst.with_file_name(name);
                }
                ConflictChoice::KeepBoth => dst = keep_both_path(&dst),
            }
        }
        if let Some(parent) = dst.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                self.error(parent, e);
                return Ok(());
            }
        }
        self.copy_complete = false;
        let res = match op {
            Op::Copy => self.copy_node(src, &smeta, &dst, replaced),
            Op::Move => self.move_node(src, &smeta, &dst),
        };
        if let Some(bak) = backup {
            match &res {
                Ok(true) => force_remove(&bak),
                Ok(false) if self.copy_complete => {
                    // The new copy is complete (only removing a move's source failed).
                    force_remove(&bak);
                }
                Ok(false) => {
                    // All-or-nothing: drop whatever partial result exists and put the old
                    // item back under its name.
                    force_remove(&dst);
                    match fs::rename(&bak, &dst) {
                        Ok(()) => self.error(&dst, "replace failed; previous version restored"),
                        Err(e) => self.error(
                            &dst,
                            format!(
                                "replace failed and restore failed ({e}); previous version kept at {}",
                                bak.display()
                            ),
                        ),
                    }
                }
                Err(_) if self.copy_complete => force_remove(&bak), // new copy is complete
                Err(_) => {
                    force_remove(&dst); // cancelled mid-way: drop any partial result
                    if let Err(e) = fs::rename(&bak, &dst) {
                        tracing::error!(error = %e, backup = %bak.display(), "cannot restore replaced item");
                    }
                }
            }
        }
        let ok = res?;
        if ok {
            self.items_done += 1;
            if op == Op::Move {
                self.consumed_sources.push(src.to_path_buf());
            }
            if !replaced {
                self.record(op, src, &dst);
            }
        }
        Ok(())
    }

    fn merge_dirs(&mut self, op: Op, src: &Path, dst: &Path) -> Flow {
        let children: Vec<_> = match fs::read_dir(src) {
            Ok(rd) => rd.flatten().map(|e| e.file_name()).collect(),
            Err(e) => {
                self.error(src, e);
                return Ok(());
            }
        };
        if op == Op::Move {
            self.files_total += children.len() as u64;
        }
        for name in children {
            self.transfer(op, &src.join(&name), dst.join(&name))?;
        }
        if op == Op::Move {
            // Succeeds only if everything moved out; leftovers (skipped/failed) stay.
            if fs::remove_dir(src).is_ok() {
                self.consumed_sources.push(src.to_path_buf());
            }
            self.files_done += 1;
        }
        Ok(())
    }

    fn move_node(&mut self, src: &Path, smeta: &Metadata, dst: &Path) -> Flow<bool> {
        match fs::rename(src, dst) {
            Ok(()) => {
                self.file_done();
                Ok(true)
            }
            Err(e) if is_cross_device(&e) => {
                let mut c = Counts::default();
                let mut t = Instant::now();
                self.scan_path(src, &mut c, false, &mut t)?;
                self.files_total = (self.files_total + c.files).saturating_sub(1);
                self.bytes_total += c.bytes;
                let errors_before = self.errors.len();
                let copied = self.copy_node(src, smeta, dst, false)?;
                if copied && self.errors.len() == errors_before {
                    self.copy_complete = true;
                    Ok(self.delete_tree(src, false)?)
                } else {
                    Ok(false)
                }
            }
            Err(e) => {
                self.error(src, e);
                Ok(false)
            }
        }
    }

    /// Copy a file, link or directory tree to a destination that does not exist (or, for
    /// `replace`, a regular file that is replaced atomically). Returns `true` if everything
    /// was copied without error.
    pub(crate) fn copy_node(
        &mut self,
        src: &Path,
        smeta: &Metadata,
        dst: &Path,
        replace: bool,
    ) -> Flow<bool> {
        self.check()?;
        if smeta.file_type().is_symlink() {
            return Ok(self.copy_link(src, smeta, dst));
        }
        if smeta.is_dir() {
            if let Err(e) = fs::create_dir(dst) {
                self.error(dst, e);
                return Ok(false);
            }
            let rd = match fs::read_dir(src) {
                Ok(rd) => rd,
                Err(e) => {
                    self.error(src, e);
                    return Ok(false);
                }
            };
            let mut ok = true;
            for entry in rd {
                self.check()?;
                match entry.and_then(|en| en.metadata().map(|m| (en, m))) {
                    Ok((en, m)) => {
                        ok &= self.copy_node(&en.path(), &m, &dst.join(en.file_name()), false)?;
                    }
                    Err(e) => {
                        self.error(src, e);
                        ok = false;
                    }
                }
            }
            return Ok(ok);
        }
        self.copy_file(src, smeta, dst, replace)
    }

    fn copy_link(&mut self, src: &Path, smeta: &Metadata, dst: &Path) -> bool {
        let target = match fs::read_link(src) {
            Ok(t) => t,
            Err(e) => {
                self.error(src, format!("link skipped: {e}"));
                return false;
            }
        };
        #[cfg(windows)]
        let r = if link_is_dir(smeta) {
            std::os::windows::fs::symlink_dir(&target, dst)
        } else {
            std::os::windows::fs::symlink_file(&target, dst)
        };
        #[cfg(unix)]
        let r = {
            let _ = smeta;
            let _ = link_is_dir;
            std::os::unix::fs::symlink(&target, dst)
        };
        match r {
            Ok(()) => {
                self.file_done();
                true
            }
            Err(e) => {
                self.error(src, format!("link skipped: {e}"));
                false
            }
        }
    }

    fn copy_file(&mut self, src: &Path, smeta: &Metadata, dst: &Path, replace: bool) -> Flow<bool> {
        self.set_current(src);
        let tmp = dst.with_file_name(self.next_temp_name());
        let mut buf = std::mem::take(&mut self.buf);
        if buf.len() != CHUNK {
            buf = vec![0u8; CHUNK];
        }
        let result = self.copy_file_inner(src, smeta, &tmp, &mut buf);
        self.buf = buf;
        match result {
            Ok(true) => {
                if replace {
                    clear_readonly(dst);
                }
                match fs::rename(&tmp, dst) {
                    Ok(()) => {
                        self.file_done();
                        Ok(true)
                    }
                    Err(e) => {
                        let _ = fs::remove_file(&tmp);
                        self.error(dst, e);
                        Ok(false)
                    }
                }
            }
            Ok(false) => {
                let _ = fs::remove_file(&tmp);
                Err(super::runner::Cancelled)
            }
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                self.error(src, e);
                Ok(false)
            }
        }
    }

    /// `Ok(false)` means cancelled.
    fn copy_file_inner(
        &mut self,
        src: &Path,
        smeta: &Metadata,
        tmp: &Path,
        buf: &mut [u8],
    ) -> io::Result<bool> {
        #[cfg(windows)]
        {
            let _ = &buf;
            self.copy_file_native(src, smeta, tmp)
        }
        #[cfg(not(windows))]
        self.copy_file_portable(src, smeta, tmp, buf)
    }

    /// CopyFileExW keeps alternate data streams, attributes and ACLs, which matters most for
    /// the cross-volume move (copy, then delete the source).
    #[cfg(windows)]
    fn copy_file_native(&mut self, src: &Path, smeta: &Metadata, tmp: &Path) -> io::Result<bool> {
        let mut cancelled = false;
        let res = crate::platform::copy_file_ex(src, tmp, &mut |n| {
            if n > 0 {
                self.add_bytes(n);
            }
            if self.check().is_err() {
                cancelled = true;
            }
            !cancelled
        });
        match res {
            Ok(true) => {
                // CopyFileExW does not carry the creation time over.
                use std::os::windows::fs::FileTimesExt;
                if let Ok(created) = smeta.created() {
                    let readonly = fs::metadata(tmp).is_ok_and(|m| m.permissions().readonly());
                    if readonly {
                        clear_readonly(tmp);
                    }
                    if let Ok(f) = OpenOptions::new().write(true).open(tmp) {
                        let _ = f.set_times(fs::FileTimes::new().set_created(created));
                    }
                    if readonly {
                        let _ = fs::set_permissions(tmp, smeta.permissions());
                    }
                }
                Ok(true)
            }
            Ok(false) => {
                let _ = fs::remove_file(tmp);
                Ok(false)
            }
            Err(e) => Err(e),
        }
    }

    #[cfg_attr(windows, allow(dead_code))]
    fn copy_file_portable(
        &mut self,
        src: &Path,
        smeta: &Metadata,
        tmp: &Path,
        buf: &mut [u8],
    ) -> io::Result<bool> {
        let mut input = File::open(src)?;
        let mut out = OpenOptions::new().write(true).create_new(true).open(tmp)?;
        loop {
            if self.check().is_err() {
                return Ok(false);
            }
            let n = input.read(buf)?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            self.add_bytes(n as u64);
        }
        if let Ok(m) = smeta.modified() {
            let _ = out.set_modified(m);
        }
        drop(out);
        let _ = fs::set_permissions(tmp, smeta.permissions());
        Ok(true)
    }
}
