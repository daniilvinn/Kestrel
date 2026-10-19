//! Archive extraction (ZIP and RAR).

#[path = "ops_common.rs"]
mod common;

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use common::*;
use ks_fs::ops::{ConflictChoice, ConflictPolicy, JobEvent, JobSpec, Outcome, Resolution};
use zip::write::SimpleFileOptions;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join(name)
}

/// Write a ZIP with `(name, content)` entries; a name ending in `/` is a directory.
fn make_zip(path: &Path, entries: &[(&str, &str)]) {
    let mut zip = zip::ZipWriter::new(fs::File::create(path).unwrap());
    let opts = SimpleFileOptions::default()
        .last_modified_time(zip::DateTime::from_date_and_time(2001, 2, 3, 4, 5, 6).unwrap());
    for (name, content) in entries {
        if name.ends_with('/') {
            zip.add_directory(*name, opts).unwrap();
        } else {
            zip.start_file(*name, opts).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
    }
    zip.finish().unwrap();
}

fn extract(archives: &[PathBuf], dest: &Path) -> JobSpec {
    JobSpec::Extract {
        archives: archives.to_vec(),
        dest_dir: dest.to_path_buf(),
    }
}

fn created(report: &ks_fs::ops::JobReport) -> Vec<PathBuf> {
    report
        .outcomes
        .iter()
        .filter_map(|o| match o {
            Outcome::Created { path, .. } => Some(path.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn zip_with_one_top_level_folder_extracts_in_place() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let zip = td.path().join("pack.zip");
    make_zip(
        &zip,
        &[
            ("pack/", ""),
            ("pack/a.txt", "alpha"),
            ("pack/sub/b.txt", "bravo"),
        ],
    );
    let dest = td.path().join("out");
    fs::create_dir(&dest).unwrap();

    let run = h.run(extract(std::slice::from_ref(&zip), &dest));
    let rep = run.assert_done();
    assert_eq!(
        listing(&dest),
        ["pack/", "pack/a.txt", "pack/sub/", "pack/sub/b.txt"]
    );
    assert_eq!(read(&dest.join("pack").join("sub").join("b.txt")), "bravo");
    assert_eq!(created(rep), [dest.join("pack")]);
    assert_eq!(rep.items_done, 1);
    assert_eq!(rep.bytes_done, 10);

    // The entry's timestamp is kept, not the extraction time.
    let modified = fs::metadata(dest.join("pack").join("a.txt"))
        .unwrap()
        .modified()
        .unwrap();
    let age = SystemTime::now().duration_since(modified).unwrap();
    assert!(age > Duration::from_secs(20 * 365 * 24 * 3600), "{age:?}");
}

#[test]
fn several_top_level_items_get_a_folder_named_after_the_archive() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let zip = td.path().join("loose.zip");
    make_zip(&zip, &[("a.txt", "a"), ("b.txt", "b")]);

    let rep = h.run(extract(std::slice::from_ref(&zip), td.path()));
    assert_eq!(created(rep.assert_done()), [td.path().join("loose")]);
    assert_eq!(listing(&td.path().join("loose")), ["a.txt", "b.txt"]);

    // A second extraction never merges into the first.
    let rep = h.run(extract(std::slice::from_ref(&zip), td.path()));
    assert_eq!(created(rep.assert_done()), [td.path().join("loose (1)")]);
    assert_eq!(listing(&td.path().join("loose (1)")), ["a.txt", "b.txt"]);
}

#[test]
fn entries_that_escape_the_destination_are_refused() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let zip = td.path().join("evil.zip");
    make_zip(
        &zip,
        &[
            ("../evil.txt", "x"),
            ("ok/../../evil2.txt", "x"),
            ("/abs.txt", "x"),
            ("C:/drive.txt", "x"),
            ("ok.txt", "fine"),
        ],
    );
    let dest = td.path().join("out");
    fs::create_dir(&dest).unwrap();

    let run = h.run(extract(&[zip], &dest));
    let JobEvent::Failed(rep) = run.last() else {
        panic!("expected Failed, got {:?}", run.last());
    };
    assert_eq!(rep.errors.len(), 4, "{:?}", rep.errors);
    assert!(rep
        .errors
        .iter()
        .all(|e| e.message == "unsafe path skipped"));
    assert_eq!(listing(&dest), ["ok.txt"]);
    assert!(!td.path().join("evil.txt").exists());
    assert!(!td.path().join("evil2.txt").exists());
}

#[test]
fn existing_files_go_through_the_conflict_policy() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let zip = td.path().join("one.zip");
    make_zip(&zip, &[("note.txt", "from zip")]);
    let dest = td.path().join("out");
    write(&dest.join("note.txt"), "mine");

    let rep = h.run_policy(
        extract(std::slice::from_ref(&zip), &dest),
        ConflictPolicy::Skip,
    );
    assert_eq!(rep.assert_done().items_skipped, 1);
    assert_eq!(read(&dest.join("note.txt")), "mine");

    let run = h.run_with(
        extract(std::slice::from_ref(&zip), &dest),
        Default::default(),
        |p| {
            assert_eq!(p.dst, dest.join("note.txt"));
            assert_eq!(p.src, zip.join("note.txt"));
            assert_eq!(p.src_meta.len, 8);
            Resolution::once(ConflictChoice::KeepBoth)
        },
    );
    run.assert_done();
    assert_eq!(read(&dest.join("note (1).txt")), "from zip");

    h.run_policy(extract(&[zip], &dest), ConflictPolicy::Overwrite)
        .assert_done();
    assert_eq!(read(&dest.join("note.txt")), "from zip");
    // No temporary files are left behind.
    assert_eq!(listing(&dest), ["note (1).txt", "note.txt"]);
}

#[test]
fn rar_with_one_top_level_folder() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let run = h.run(extract(&[fixture("nested.rar")], td.path()));
    let rep = run.assert_done();
    assert_eq!(
        listing(td.path()),
        [
            "project/",
            "project/readme.txt",
            "project/src/",
            "project/src/main.rs"
        ]
    );
    assert_eq!(
        read(&td.path().join("project").join("readme.txt")),
        "hello from rar\n"
    );
    assert_eq!(created(rep), [td.path().join("project")]);
    assert_eq!(rep.bytes_done, 15 + 13);
}

#[test]
fn rar_with_loose_files_gets_its_own_folder() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let rep = h.run(extract(&[fixture("loose.rar")], td.path()));
    rep.assert_done();
    assert_eq!(listing(td.path()), ["loose/", "loose/a.txt", "loose/b.txt"]);
    assert_eq!(read(&td.path().join("loose").join("b.txt")), "beta\n");
}

#[test]
fn password_protected_rar_is_reported() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let run = h.run(extract(&[fixture("locked.rar")], td.path()));
    let JobEvent::Failed(rep) = run.last() else {
        panic!("expected Failed, got {:?}", run.last());
    };
    assert!(
        rep.errors[0].message.contains("password-protected"),
        "{:?}",
        rep.errors
    );
    assert!(listing(td.path()).is_empty(), "{:?}", listing(td.path()));
}

#[test]
fn non_archives_fail_cleanly() {
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    let fake = td.path().join("fake.zip");
    write(&fake, "not really a zip");
    let run = h.run(extract(&[fake], td.path()));
    let JobEvent::Failed(rep) = run.last() else {
        panic!("expected Failed, got {:?}", run.last());
    };
    assert_eq!(rep.errors[0].message, "not a ZIP or RAR archive");
    assert_eq!(listing(td.path()), ["fake.zip"]);
}

#[test]
fn undo_trashes_what_the_extraction_created() {
    if !trash_supported() {
        return;
    }
    let h = Harness::new();
    let td = tempfile::tempdir().unwrap();
    h.run(extract(&[fixture("loose.rar")], td.path()))
        .assert_done();
    assert!(td.path().join("loose").is_dir());
    h.undo().assert_done();
    assert!(!td.path().join("loose").exists());
    purge_trash_under(td.path());
}
