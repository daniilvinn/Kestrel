//! Archive extraction: ZIP (the `zip` crate) and RAR (UnRAR, through the `unrar` crate).
//!
//! Policies:
//! - The format is detected from the file's magic bytes, not its extension.
//! - Smart destination: an archive whose entries share one top-level name is extracted straight
//!   into the destination directory; anything else goes into a new folder named after the
//!   archive (`name (1)` when that name is taken), so extracting never scatters files.
//! - Entry paths are sanitized: absolute paths, drive prefixes and `..` components that would
//!   escape the destination are rejected as item errors (zip-slip). Links are skipped.
//! - Existing directories are merged; an existing file goes through the conflict policy.
//! - Every file is written to a hidden temporary sibling and renamed into place, like copies.
//! - Password-protected entries are reported as item errors.
//! - Selecting several parts of one multi-volume RAR extracts it once.
//! - Cancellation is checked between 4 MiB chunks for ZIP and between entries for RAR (UnRAR
//!   writes a whole entry per call).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::SystemTime;

use super::names::{keep_both_path, validate_name};
use super::paths::{clear_readonly, path_key};
use super::runner::{Cancelled, Flow, Runner, CHUNK};
use super::types::{ConflictChoice, FileMeta, JobEvent, Outcome};

/// Windows `FILE_ATTRIBUTE_REPARSE_POINT`, set on RAR entries that are links.
const RAR_ATTR_REPARSE_POINT: u32 = 0x400;

/// Archive formats the engine can extract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveKind {
    /// PKZIP (also .jar, .cbz, ...).
    Zip,
    /// RAR 1.5 to RAR 5.
    Rar,
}

impl ArchiveKind {
    /// Detect the format from the first bytes of `path`.
    pub fn detect(path: &Path) -> io::Result<Option<Self>> {
        let mut head = [0u8; 8];
        let mut file = File::open(path)?;
        let mut n = 0;
        while n < head.len() {
            match file.read(&mut head[n..])? {
                0 => break,
                k => n += k,
            }
        }
        let head = &head[..n];
        Ok(
            if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
                Some(ArchiveKind::Zip)
            } else if head.starts_with(b"Rar!\x1a\x07") {
                Some(ArchiveKind::Rar)
            } else {
                None
            },
        )
    }
}

/// One entry of the archive, as planned before anything is written.
struct Member {
    /// Safe relative path below the extraction root.
    rel: PathBuf,
    is_dir: bool,
    size: u64,
    modified: Option<SystemTime>,
}

/// What an archive holds, after sanitizing. `members[i]` is `None` for entries that are
/// skipped (an error was already recorded for them).
struct Listing {
    members: Vec<Option<Member>>,
}

impl Listing {
    fn files(&self) -> impl Iterator<Item = &Member> {
        self.members.iter().flatten().filter(|m| !m.is_dir)
    }

    /// Distinct first path components.
    fn roots(&self) -> BTreeSet<OsString> {
        self.members
            .iter()
            .flatten()
            .filter_map(|m| m.rel.components().next())
            .map(|c| c.as_os_str().to_ascii_lowercase())
            .collect()
    }
}

/// `name` as a relative path that stays below the extraction root, or `None`.
///
/// Both separators count (RAR and ZIP writers disagree), `.` is dropped and `..` may only
/// climb back out of components it entered.
pub fn sanitize_entry_path(name: &str) -> Option<PathBuf> {
    if name.contains('\0') {
        return None;
    }
    let unified = name.replace('\\', "/");
    if unified.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = Vec::new();
    for part in unified.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            // `C:` style prefixes and alternate data streams.
            p if p.contains(':') => return None,
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return None;
    }
    let path: PathBuf = parts.iter().collect();
    path.components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then_some(path)
}

/// Archive name without the extension and without a `.partN` volume suffix.
fn folder_name(archive: &Path) -> String {
    let stem = archive
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "archive".to_owned());
    let lower = stem.to_ascii_lowercase();
    if let Some(pos) = lower.rfind(".part") {
        if lower[pos + 5..].chars().all(|c| c.is_ascii_digit()) && pos + 5 < lower.len() {
            return stem[..pos].to_owned();
        }
    }
    stem
}

/// MS-DOS date/time (local time) to a system time.
fn dos_time(date: u16, time: u16) -> Option<SystemTime> {
    let dt = zip::DateTime::try_from_msdos(date, time).ok()?;
    local_to_system(time::PrimitiveDateTime::try_from(dt).ok()?)
}

fn local_to_system(local: time::PrimitiveDateTime) -> Option<SystemTime> {
    let guess = local.assume_utc();
    let offset = time::UtcOffset::local_offset_at(guess).unwrap_or(time::UtcOffset::UTC);
    Some(local.assume_offset(offset).into())
}

fn zip_mtime<R: Read>(file: &zip::read::ZipFile<'_, R>) -> Option<SystemTime> {
    let unix = file.extra_data_fields().find_map(|f| match f {
        zip::ExtraField::ExtendedTimestamp(ts) => ts.mod_time(),
        _ => None,
    });
    if let Some(secs) = unix {
        return Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(u64::from(secs)));
    }
    let dt = file.last_modified()?;
    local_to_system(time::PrimitiveDateTime::try_from(dt).ok()?)
}

fn list_zip(r: &mut Runner, archive: &Path) -> Result<Listing, String> {
    let file = File::open(archive).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).map_err(|e| e.to_string())?;
    let mut members = Vec::with_capacity(zip.len());
    for i in 0..zip.len() {
        let f = match zip.by_index_raw(i) {
            Ok(f) => f,
            Err(e) => {
                r.error(archive, format!("entry {i}: {e}"));
                members.push(None);
                continue;
            }
        };
        let name = String::from_utf8_lossy(f.name_raw()).into_owned();
        let member = if f.is_symlink() {
            r.error(&archive.join(&name), "link skipped");
            None
        } else if f.encrypted() {
            r.error(
                &archive.join(&name),
                "password-protected entries are not supported",
            );
            None
        } else if let Some(rel) = sanitize_entry_path(&name) {
            Some(Member {
                rel,
                is_dir: f.is_dir(),
                size: f.size(),
                modified: zip_mtime(&f),
            })
        } else {
            r.error(&archive.join(&name), "unsafe path skipped");
            None
        };
        members.push(member);
    }
    Ok(Listing { members })
}

fn rar_error(e: unrar::error::UnrarError) -> String {
    use unrar::error::Code;
    match e.code {
        Code::MissingPassword | Code::BadPassword => {
            "password-protected archives are not supported".to_owned()
        }
        _ => e.to_string(),
    }
}

/// The plan for one RAR header, or why it is skipped.
fn rar_member(h: &unrar::FileHeader) -> Result<Member, &'static str> {
    if h.file_attr & RAR_ATTR_REPARSE_POINT != 0 {
        return Err("link skipped");
    }
    if h.is_encrypted() {
        return Err("password-protected entries are not supported");
    }
    let rel = sanitize_entry_path(&h.filename.to_string_lossy()).ok_or("unsafe path skipped")?;
    Ok(Member {
        rel,
        is_dir: h.is_directory(),
        size: h.unpacked_size,
        modified: dos_time((h.file_time >> 16) as u16, h.file_time as u16),
    })
}

fn list_rar(r: &mut Runner, archive: &Path) -> Result<Listing, String> {
    let open = unrar::Archive::new(archive)
        .open_for_listing()
        .map_err(rar_error)?;
    let mut members = Vec::new();
    for header in open {
        let header = header.map_err(rar_error)?;
        match rar_member(&header) {
            Ok(m) => members.push(Some(m)),
            Err(e) => {
                r.error(&archive.join(&header.filename), e);
                members.push(None);
            }
        }
    }
    Ok(Listing { members })
}

/// How an entry ends up being written.
enum Target {
    Write { path: PathBuf, replace: bool },
    Skip,
}

impl Runner {
    /// Extract each archive into `dest_dir` (see the module docs for the layout rules).
    pub(crate) fn run_extract(&mut self, archives: Vec<PathBuf>, dest_dir: PathBuf) -> Flow {
        // Several parts of one multi-volume RAR are one archive.
        let mut seen = BTreeSet::new();
        let mut jobs = Vec::new();
        for archive in archives {
            let kind = match ArchiveKind::detect(&archive) {
                Ok(Some(k)) => k,
                Ok(None) => {
                    self.error(&archive, "not a ZIP or RAR archive");
                    continue;
                }
                Err(e) => {
                    self.error(&archive, e);
                    continue;
                }
            };
            let archive = match kind {
                ArchiveKind::Rar => {
                    let a = unrar::Archive::new(&archive);
                    match a.first_part_option() {
                        Some(first) if a.is_multipart() && first.exists() => first,
                        _ => archive,
                    }
                }
                ArchiveKind::Zip => archive,
            };
            if seen.insert(path_key(&archive)) {
                jobs.push((archive, kind));
            }
        }

        let mut listings = Vec::with_capacity(jobs.len());
        for (archive, kind) in jobs {
            self.check()?;
            self.set_current(&archive);
            let listing = match kind {
                ArchiveKind::Zip => list_zip(self, &archive),
                ArchiveKind::Rar => list_rar(self, &archive),
            };
            match listing {
                Ok(l) => {
                    self.files_total += l.files().count() as u64;
                    self.bytes_total += l.files().map(|m| m.size).sum::<u64>();
                    listings.push((archive, kind, l));
                }
                Err(e) => self.error(&archive, e),
            }
        }
        self.emit(JobEvent::Scanning {
            job: self.id,
            generation: self.generation,
            files: self.files_total,
            bytes: self.bytes_total,
        });
        self.tick(true);

        for (archive, kind, listing) in listings {
            self.extract_one(&archive, kind, &listing, &dest_dir)?;
        }
        Ok(())
    }

    fn extract_one(
        &mut self,
        archive: &Path,
        kind: ArchiveKind,
        listing: &Listing,
        dest_dir: &Path,
    ) -> Flow {
        if listing.members.iter().all(Option::is_none) {
            if listing.members.is_empty() {
                self.error(archive, "the archive is empty");
            }
            return Ok(());
        }
        let roots = listing.roots();
        // Top-level paths that did not exist before become the job's (undoable) outcomes.
        let mut fresh: Vec<(PathBuf, bool)> = Vec::new();
        let root = if roots.len() == 1 {
            let first = listing
                .members
                .iter()
                .flatten()
                .find_map(|m| m.rel.components().next())
                .map(|c| dest_dir.join(c.as_os_str()));
            if let Some(top) = first.filter(|t| fs::symlink_metadata(t).is_err()) {
                let is_dir = listing
                    .members
                    .iter()
                    .flatten()
                    .any(|m| m.is_dir || m.rel.components().count() > 1);
                fresh.push((top, is_dir));
            }
            dest_dir.to_path_buf()
        } else {
            let mut folder = dest_dir.join(folder_name(archive));
            if fs::symlink_metadata(&folder).is_ok() {
                folder = keep_both_path(&folder);
            }
            if let Err(e) = fs::create_dir_all(&folder) {
                self.error(&folder, e);
                return Ok(());
            }
            fresh.push((folder.clone(), true));
            folder
        };

        let errors_before = self.errors.len();
        let result = match kind {
            ArchiveKind::Zip => self.extract_zip(archive, listing, &root),
            ArchiveKind::Rar => self.extract_rar(archive, &root),
        };
        // Record what was created even when cancelled, so undo can clean it up.
        for (path, is_dir) in fresh {
            if fs::symlink_metadata(&path).is_ok() {
                self.outcomes.push(Outcome::Created { path, is_dir });
            }
        }
        result?;
        if self.errors.len() == errors_before {
            self.items_done += 1;
        }
        Ok(())
    }

    /// Where `member` goes, after directories are created and conflicts are decided.
    fn prepare(&mut self, archive: &Path, member: &Member, root: &Path) -> Flow<Target> {
        let mut dst = root.join(&member.rel);
        if member.is_dir {
            if let Err(e) = fs::create_dir_all(&dst) {
                self.error(&dst, e);
            }
            return Ok(Target::Skip);
        }
        if let Some(parent) = dst.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                self.error(parent, e);
                return Ok(self.skip_file(member));
            }
        }
        let src = archive.join(&member.rel);
        let mut replace = false;
        while let Ok(existing) = fs::symlink_metadata(&dst) {
            if existing.is_dir() {
                self.error(&dst, "a directory with this name already exists");
                return Ok(self.skip_file(member));
            }
            let src_meta = FileMeta {
                is_dir: false,
                is_link: false,
                len: member.size,
                modified: member.modified,
            };
            let dst_meta = FileMeta::from_metadata(&existing);
            match self.resolve_conflict(&src, &dst, src_meta, dst_meta)? {
                ConflictChoice::Skip | ConflictChoice::Cancel => {
                    self.items_skipped += 1;
                    return Ok(self.skip_file(member));
                }
                ConflictChoice::Overwrite => {
                    replace = true;
                    break;
                }
                ConflictChoice::Rename(name) => {
                    if let Err(e) = validate_name(&name) {
                        self.error(&dst, format!("invalid name {name:?}: {e}"));
                        return Ok(self.skip_file(member));
                    }
                    dst = dst.with_file_name(name);
                }
                ConflictChoice::KeepBoth => dst = keep_both_path(&dst),
            }
        }
        self.set_current(&src);
        Ok(Target::Write { path: dst, replace })
    }

    fn skip_file(&mut self, member: &Member) -> Target {
        self.bytes_done += member.size;
        self.file_done();
        Target::Skip
    }

    /// Rename the finished temporary file into place.
    fn commit(&mut self, tmp: &Path, dst: &Path, replace: bool) {
        if replace {
            clear_readonly(dst);
        }
        match fs::rename(tmp, dst) {
            Ok(()) => self.file_done(),
            Err(e) => {
                let _ = fs::remove_file(tmp);
                self.error(dst, e);
            }
        }
    }

    fn extract_zip(&mut self, archive: &Path, listing: &Listing, root: &Path) -> Flow {
        let opened = File::open(archive)
            .map_err(|e| e.to_string())
            .and_then(|f| zip::ZipArchive::new(BufReader::new(f)).map_err(|e| e.to_string()));
        let mut zip = match opened {
            Ok(z) => z,
            Err(e) => {
                self.error(archive, e);
                return Ok(());
            }
        };
        let mut buf = std::mem::take(&mut self.buf);
        if buf.len() != CHUNK {
            buf = vec![0u8; CHUNK];
        }
        let mut result = Ok(());
        for (i, member) in listing.members.iter().enumerate() {
            let Some(member) = member else { continue };
            if let Err(c) = self.check() {
                result = Err(c);
                break;
            }
            let (dst, replace) = match self.prepare(archive, member, root) {
                Ok(Target::Write { path, replace }) => (path, replace),
                Ok(Target::Skip) => continue,
                Err(c) => {
                    result = Err(c);
                    break;
                }
            };
            let tmp = dst.with_file_name(self.next_temp_name());
            let written = match zip.by_index(i) {
                Ok(mut entry) => self.write_stream(&mut entry, &tmp, member.modified, &mut buf),
                Err(e) => Err(io::Error::other(e)),
            };
            match written {
                Ok(true) => self.commit(&tmp, &dst, replace),
                Ok(false) => {
                    let _ = fs::remove_file(&tmp);
                    result = Err(Cancelled);
                    break;
                }
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    self.error(&archive.join(&member.rel), e);
                }
            }
        }
        self.buf = buf;
        result
    }

    /// Copy `reader` into a new file at `tmp`. `Ok(false)` means cancelled.
    fn write_stream(
        &mut self,
        reader: &mut impl Read,
        tmp: &Path,
        modified: Option<SystemTime>,
        buf: &mut [u8],
    ) -> io::Result<bool> {
        let mut out = File::create(tmp)?;
        loop {
            if self.check().is_err() {
                return Ok(false);
            }
            let n = match reader.read(buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            out.write_all(&buf[..n])?;
            self.add_bytes(n as u64);
        }
        if let Some(t) = modified {
            let _ = out.set_modified(t);
        }
        out.sync_all()?;
        Ok(true)
    }

    fn extract_rar(&mut self, archive: &Path, root: &Path) -> Flow {
        let mut cursor = match unrar::Archive::new(archive).open_for_processing() {
            Ok(c) => c,
            Err(e) => {
                self.error(archive, rar_error(e));
                return Ok(());
            }
        };
        loop {
            self.check()?;
            let entry = match cursor.read_header() {
                Ok(Some(entry)) => entry,
                Ok(None) => return Ok(()),
                Err(e) => {
                    self.error(archive, rar_error(e));
                    return Ok(());
                }
            };
            // Skipped entries were reported while listing.
            let member = rar_member(entry.entry()).ok();
            let target = match &member {
                Some(m) => self.prepare(archive, m, root)?,
                None => Target::Skip,
            };
            let (next, failed) = match (target, &member) {
                (Target::Write { path, replace }, Some(m)) => {
                    let tmp = path.with_file_name(self.next_temp_name());
                    self.tick(false);
                    match entry.extract_to(&tmp) {
                        Ok(c) => {
                            self.bytes_done += m.size;
                            self.commit(&tmp, &path, replace);
                            (Ok(c), None)
                        }
                        Err(e) => {
                            let _ = fs::remove_file(&tmp);
                            (Err(e), Some(archive.join(&m.rel)))
                        }
                    }
                }
                _ => (entry.skip(), None),
            };
            cursor = match next {
                Ok(c) => c,
                Err(e) => {
                    // UnRAR cannot continue after a failed entry.
                    let at = failed.unwrap_or_else(|| archive.to_path_buf());
                    self.error(&at, rar_error(e));
                    return Ok(());
                }
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_keeps_normal_paths() {
        assert_eq!(
            sanitize_entry_path("a/b.txt"),
            Some(PathBuf::from("a").join("b.txt"))
        );
        assert_eq!(
            sanitize_entry_path("a\\b.txt"),
            Some(PathBuf::from("a").join("b.txt"))
        );
        assert_eq!(
            sanitize_entry_path("./a//b/"),
            Some(PathBuf::from("a").join("b"))
        );
        assert_eq!(sanitize_entry_path("a/../b"), Some(PathBuf::from("b")));
    }

    #[test]
    fn sanitize_rejects_escapes() {
        for bad in [
            "../evil.txt",
            "a/../../evil.txt",
            "/etc/passwd",
            "\\Windows\\evil.dll",
            "C:/evil.txt",
            "C:evil.txt",
            "file.txt:stream",
            "",
            ".",
            "a/..",
        ] {
            assert_eq!(sanitize_entry_path(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn folder_name_strips_volume_suffix() {
        assert_eq!(folder_name(Path::new("x/photos.zip")), "photos");
        assert_eq!(folder_name(Path::new("x/movie.part1.rar")), "movie");
        assert_eq!(folder_name(Path::new("x/movie.part12.rar")), "movie");
        assert_eq!(folder_name(Path::new("x/my.partner.rar")), "my.partner");
        assert_eq!(folder_name(Path::new("x/a.b.zip")), "a.b");
    }
}
