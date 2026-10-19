//! Integration tests for the preview engine (tempfile fixtures).
#![allow(clippy::print_stderr)]

use std::fs;
use std::io::Write;
use std::path::Path;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use ks_preview::*;
use tokio_util::sync::CancellationToken;

fn build(path: &Path, opts: &PreviewOptions) -> Preview {
    build_preview(path, opts, &CancellationToken::new()).expect("not cancelled")
}

fn rust_source(bytes: usize) -> String {
    let unit = "/// Doc comment\nfn add_numbers(a: u32, b: u32) -> u32 {\n    let s = \"text\"; // trailing\n    a + b * 42\n}\n\n";
    unit.repeat(bytes / unit.len() + 1)
}

fn recv_gen(rx: &Receiver<PreviewEvent>, generation: u64) -> PreviewEvent {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let ev = rx.recv_timeout(left).expect("event within 10s");
        if ev.generation == generation {
            return ev;
        }
    }
}

#[test]
fn rust_file_is_highlighted_with_line_count() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("main.rs");
    fs::write(&p, "fn main() {\n\tprintln!(\"hi\");\n}\n").unwrap();
    let opts = PreviewOptions::default();
    let pv = build(&p, &opts);
    let PreviewBody::Text(t) = pv.body else {
        panic!("expected text")
    };
    assert_eq!(t.line_count, 3);
    assert_eq!(t.syntax.as_deref(), Some("Rust"));
    assert_eq!(t.encoding, Encoding::Utf8);
    assert!(!t.truncated);
    assert_eq!(t.lines[1].text(), "    println!(\"hi\");", "tab expanded");
    assert!(t.lines[0]
        .spans
        .iter()
        .any(|s| s.style.fg == Some(Color::Rgb(opts.palette.keyword))));
    assert!(pv.meta.is_some());
}

#[test]
fn a_text_limit_below_the_hex_size_still_reports_truncation() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("cut.txt");
    // 3000 bytes, cut at 1000 inside a multi-byte character (999 'a' then 'é').
    let content = format!("{}é{}", "a".repeat(999), "b".repeat(1999));
    assert_eq!(content.len(), 3000);
    fs::write(&p, &content).unwrap();
    let mut opts = PreviewOptions::default();
    opts.limits.max_text_bytes = 1000;
    let PreviewBody::Text(t) = build(&p, &opts).body else {
        panic!("expected text")
    };
    assert!(t.truncated, "the file is longer than the text limit");
    assert_eq!(t.bytes_read, 1000);
    assert_eq!(t.total_lines, None);
    assert_ne!(
        t.encoding,
        Encoding::Lossy,
        "a cut inside a character is not lossy"
    );
}

#[test]
fn text_is_truncated_by_lines_and_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.txt");
    fs::write(&p, "line\n".repeat(5000)).unwrap();
    let PreviewBody::Text(t) = build(&p, &PreviewOptions::default()).body else {
        panic!()
    };
    assert_eq!(t.line_count, 2000);
    assert!(t.truncated);

    let p2 = dir.path().join("wide.txt");
    fs::write(&p2, "x".repeat(1_000_000)).unwrap();
    let PreviewBody::Text(t) = build(&p2, &PreviewOptions::default()).body else {
        panic!()
    };
    assert_eq!(t.bytes_read, 256 * 1024);
    assert!(t.truncated);
}

#[test]
fn utf16_and_utf8_bom_and_latin1() {
    let dir = tempfile::tempdir().unwrap();
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(
        "héllo\r\nwörld"
            .encode_utf16()
            .flat_map(|u| u.to_le_bytes()),
    );
    let p = dir.path().join("u16.txt");
    fs::write(&p, bytes).unwrap();
    let PreviewBody::Text(t) = build(&p, &PreviewOptions::default()).body else {
        panic!()
    };
    assert_eq!(t.encoding, Encoding::Utf16Le);
    assert_eq!(t.lines[0].text(), "héllo");
    assert_eq!(t.lines[1].text(), "wörld");

    let p = dir.path().join("bom.txt");
    fs::write(&p, b"\xEF\xBB\xBFabc").unwrap();
    let PreviewBody::Text(t) = build(&p, &PreviewOptions::default()).body else {
        panic!()
    };
    assert_eq!(t.encoding, Encoding::Utf8Bom);
    assert_eq!(t.lines[0].text(), "abc");

    let p = dir.path().join("latin.txt");
    fs::write(&p, b"caf\xe9 au lait\n").unwrap();
    let PreviewBody::Text(t) = build(&p, &PreviewOptions::default()).body else {
        panic!()
    };
    assert_eq!(t.encoding, Encoding::Lossy);
    assert!(t.lines[0].text().contains('\u{fffd}'));
}

#[test]
fn binary_gets_hex_view() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("blob.exe");
    let mut data = b"MZ\x90\x00\x03\x00\x00\x00".to_vec();
    data.extend((0..20_000u32).map(|i| (i % 251) as u8));
    fs::write(&p, &data).unwrap();
    let PreviewBody::Binary(b) = build(&p, &PreviewOptions::default()).body else {
        panic!("expected binary")
    };
    assert_eq!(b.rows.len(), 4096 / 16);
    assert_eq!(b.rows[0].offset, 0);
    assert!(b.rows[0].hex().starts_with("4d 5a 90 00"));
    assert!(b.rows[0].ascii().starts_with("MZ"));
    assert_eq!(b.rows[1].offset, 16);
    assert_eq!(b.total_size, data.len() as u64);
    assert_eq!(b.type_hint, Some("Windows executable / DLL"));
}

#[test]
fn zip_entries_are_listed() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.zip");
    {
        let f = fs::File::create(&p).unwrap();
        let mut zw = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zw.add_directory("docs/", opts).unwrap();
        zw.start_file("docs/readme.txt", opts).unwrap();
        zw.write_all(&b"hello hello hello hello ".repeat(100))
            .unwrap();
        zw.start_file("evil\x1b[31m.txt", opts).unwrap();
        zw.write_all(b"x").unwrap();
        zw.finish().unwrap();
    }
    let PreviewBody::Archive(a) = build(&p, &PreviewOptions::default()).body else {
        panic!("expected archive")
    };
    assert_eq!(a.format, ArchiveFormat::Zip);
    assert!(a.listed);
    assert_eq!(a.total_entries, 3);
    assert!(a.entries[0].is_dir);
    assert_eq!(a.entries[1].name, "docs/readme.txt");
    assert_eq!(a.entries[1].size, 2400);
    assert!(a.entries[1].compressed_size < 2400);
    assert!(!a.entries[2].name.contains('\x1b'));

    // Other archive types: metadata only.
    let p = dir.path().join("a.7z");
    fs::write(&p, b"7z\xbc\xaf\x27\x1c\0\x04").unwrap();
    let PreviewBody::Archive(a) = build(&p, &PreviewOptions::default()).body else {
        panic!()
    };
    assert_eq!(a.format, ArchiveFormat::SevenZip);
    assert!(!a.listed);
}

#[test]
fn rar_entries_are_listed() {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("../ks-fs/tests/fixtures/nested.rar");
    let PreviewBody::Archive(a) = build(&p, &PreviewOptions::default()).body else {
        panic!()
    };
    assert_eq!(a.format, ArchiveFormat::Rar);
    assert!(a.listed);
    let mut names: Vec<_> = a.entries.iter().map(|e| e.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "project",
            "project/readme.txt",
            "project/src",
            "project/src/main.rs"
        ]
    );
    assert_eq!(a.total_size, 15 + 13);
}

#[test]
fn corrupt_zip_falls_back_to_hex() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("bad.zip");
    fs::write(&p, b"PK\x03\x04garbage").unwrap();
    let PreviewBody::Binary(b) = build(&p, &PreviewOptions::default()).body else {
        panic!()
    };
    assert!(b.note.is_some());
}

#[test]
fn markdown_is_rendered() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("README.md");
    fs::write(&p, "# Title\n\n- item\n\n```rust\nlet x = 1;\n```\n").unwrap();
    let PreviewBody::Markdown(m) = build(&p, &PreviewOptions::default()).body else {
        panic!("expected markdown")
    };
    assert_eq!(m.lines[0].text(), "# Title");
    assert!(m.lines[0].spans.iter().any(|s| s.style.bold));
    assert_eq!(m.lines[2].text(), "\u{2022} item");
    assert_eq!(m.lines[5].text(), "let x = 1;");
}

#[test]
fn directory_listing_is_sorted_dirs_first_and_capped() {
    let dir = tempfile::tempdir().unwrap();
    for n in ["file10.txt", "file2.txt", "File1.txt"] {
        fs::write(dir.path().join(n), "x").unwrap();
    }
    fs::create_dir(dir.path().join("zdir")).unwrap();
    fs::create_dir(dir.path().join("adir")).unwrap();
    let PreviewBody::Directory(d) = build(dir.path(), &PreviewOptions::default()).body else {
        panic!()
    };
    let names: Vec<_> = d.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(
        names,
        ["adir", "zdir", "File1.txt", "file2.txt", "file10.txt"]
    );
    assert_eq!(d.entries[0].kind, EntryKind::Dir);
    assert_eq!(d.entries[2].size, Some(1));
    assert!(!d.truncated);

    let many = tempfile::tempdir().unwrap();
    for i in 0..600 {
        fs::write(many.path().join(format!("f{i}")), "").unwrap();
    }
    let PreviewBody::Directory(d) = build(many.path(), &PreviewOptions::default()).body else {
        panic!()
    };
    assert_eq!(d.entries.len(), 500);
    assert_eq!(d.total, 600);
    assert!(d.truncated);
}

#[test]
fn image_is_decoded_and_downscaled() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("pic.png");
    image::RgbImage::from_pixel(300, 200, image::Rgb([200, 30, 30]))
        .save(&p)
        .unwrap();
    let PreviewBody::Image(i) = build(&p, &PreviewOptions::default()).body else {
        panic!("expected image")
    };
    assert_eq!((i.width, i.height), (300, 200));
    assert_eq!(i.format, "PNG");
    assert!(!i.downscaled);

    let mut opts = PreviewOptions::default();
    opts.limits.image_max_dim = 100;
    let PreviewBody::Image(i) = build(&p, &opts).body else {
        panic!()
    };
    assert!(i.downscaled);
    assert_eq!((i.width, i.height), (300, 200), "original size is kept");
    assert!(i.image.width() <= 100 && i.image.height() <= 100);

    // Pixel-count guard.
    let mut opts = PreviewOptions::default();
    opts.limits.max_image_pixels = 10;
    assert!(matches!(
        build(&p, &opts).body,
        PreviewBody::Unavailable {
            reason: UnavailableReason::TooLarge,
            ..
        }
    ));

    // Corrupt image: hex fallback.
    let bad = dir.path().join("bad.png");
    fs::write(&bad, b"\x89PNG\r\n\x1a\nnot really").unwrap();
    assert!(matches!(
        build(&bad, &PreviewOptions::default()).body,
        PreviewBody::Binary(_)
    ));
}

#[test]
fn large_file_is_unavailable() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("huge.log");
    fs::write(&p, vec![b'a'; 5000]).unwrap();
    let mut opts = PreviewOptions::default();
    opts.limits.max_file_size = 1000;
    let pv = build(&p, &opts);
    assert!(matches!(
        pv.body,
        PreviewBody::Unavailable {
            reason: UnavailableReason::TooLarge,
            ..
        }
    ));
    assert_eq!(pv.meta.unwrap().size, 5000);
}

#[test]
fn missing_file_is_unavailable_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let pv = build(&dir.path().join("nope.txt"), &PreviewOptions::default());
    assert!(matches!(
        pv.body,
        PreviewBody::Unavailable {
            reason: UnavailableReason::NotFound,
            ..
        }
    ));
    assert!(pv.meta.is_none());
}

#[test]
fn empty_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("e.txt");
    fs::write(&p, "").unwrap();
    assert!(matches!(
        build(&p, &PreviewOptions::default()).body,
        PreviewBody::Empty
    ));
}

#[test]
fn metadata_kind_and_lines() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("m.txt");
    fs::write(&p, "12345").unwrap();
    let pv = build(&p, &PreviewOptions::metadata());
    assert!(matches!(pv.body, PreviewBody::Metadata));
    let meta = pv.meta.unwrap();
    assert_eq!(meta.size, 5);
    assert!(meta.modified.is_some());
    let text: Vec<String> = meta.to_lines().iter().map(|l| l.text()).collect();
    assert!(text
        .iter()
        .any(|l| l.starts_with("Size") && l.contains("5 B")));
    assert!(text
        .iter()
        .any(|l| l.starts_with("Modified") && l.contains("UTC")));
}

#[cfg(windows)]
fn attrib(flags: [&str; 2], p: &Path) {
    use std::os::windows::process::CommandExt;
    let st = std::process::Command::new("attrib")
        .args(flags)
        .arg(p)
        .creation_flags(0x0800_0000)
        .status()
        .unwrap();
    assert!(st.success());
}

#[cfg(windows)]
#[test]
fn readonly_and_hidden_attributes() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("h.txt");
    fs::write(&p, "x").unwrap();
    attrib(["+H", "+R"], &p);
    let m = FileMetadata::read(&p).unwrap();
    assert!(m.attributes.hidden && m.attributes.readonly);
    // Hidden entries are filtered from directory previews unless asked for.
    let PreviewBody::Directory(d) = build(dir.path(), &PreviewOptions::default()).body else {
        panic!()
    };
    assert!(d.entries.is_empty());
    let opts = PreviewOptions {
        show_hidden: true,
        ..PreviewOptions::default()
    };
    let PreviewBody::Directory(d) = build(dir.path(), &opts).body else {
        panic!()
    };
    assert_eq!(d.entries.len(), 1);
    assert!(d.entries[0].hidden);
    attrib(["-H", "-R"], &p);
}

#[test]
fn escape_sequences_in_content_never_reach_output() {
    let dir = tempfile::tempdir().unwrap();
    let evil = "\x1b[2J\x1b]0;pwned\x07 text \x1b[31mred\u{9b}31m \u{202e}rtl\r\n";
    for name in ["evil.txt", "evil.rs", "evil.md"] {
        let p = dir.path().join(name);
        fs::write(&p, evil).unwrap();
        let body = build(&p, &PreviewOptions::default()).body;
        let (PreviewBody::Text(t) | PreviewBody::Markdown(t)) = body else {
            panic!("{name}: expected text")
        };
        for l in &t.lines {
            for s in &l.spans {
                assert!(
                    !s.text.chars().any(|c| c.is_control() || c == '\u{202e}'),
                    "{name}: {:?}",
                    s.text
                );
            }
        }
    }
}

#[test]
fn cancelled_token_yields_none() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, rust_source(50_000)).unwrap();
    let tok = CancellationToken::new();
    tok.cancel();
    assert!(build_preview(&p, &PreviewOptions::default(), &tok).is_none());
}

#[test]
fn previewer_delivers_and_caches() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "fn a() {}\n").unwrap();
    let (pv, rx) = Previewer::with_channel();
    pv.request(&p, 1, PreviewOptions::default());
    let ev = recv_gen(&rx, 1);
    assert!(!ev.from_cache);
    assert_eq!(ev.path, p);
    assert!(matches!(ev.result.body, PreviewBody::Text(_)));
    assert_eq!(pv.cache_len(), 1);

    pv.request(&p, 2, PreviewOptions::default());
    let ev2 = recv_gen(&rx, 2);
    assert!(ev2.from_cache);
    assert!(std::sync::Arc::ptr_eq(&ev.result, &ev2.result));

    // Different relevant option: miss.
    let opts = PreviewOptions {
        highlight: false,
        ..PreviewOptions::default()
    };
    pv.request(&p, 3, opts);
    assert!(!recv_gen(&rx, 3).from_cache);

    // Changed size/mtime: miss and fresh content.
    std::thread::sleep(Duration::from_millis(20));
    fs::write(&p, "fn bb() {}\nfn c() {}\n").unwrap();
    pv.request(&p, 4, PreviewOptions::default());
    let ev4 = recv_gen(&rx, 4);
    assert!(!ev4.from_cache);
    let PreviewBody::Text(t) = &ev4.result.body else {
        panic!()
    };
    assert_eq!(t.line_count, 2);

    pv.invalidate(&p);
    pv.request(&p, 5, PreviewOptions::default());
    assert!(!recv_gen(&rx, 5).from_cache);
}

#[test]
fn cache_is_bounded_to_64() {
    let dir = tempfile::tempdir().unwrap();
    let (pv, rx) = Previewer::with_channel();
    for i in 0..80u64 {
        let p = dir.path().join(format!("f{i}.txt"));
        fs::write(&p, "x").unwrap();
        pv.request(&p, i, PreviewOptions::default());
        recv_gen(&rx, i);
    }
    assert_eq!(pv.cache_len(), CACHE_CAPACITY);
}

#[test]
fn rapid_requests_deliver_the_last_generation() {
    let dir = tempfile::tempdir().unwrap();
    let (pv, rx) = Previewer::with_channel();
    let big = dir.path().join("big.rs");
    fs::write(&big, rust_source(250_000)).unwrap();
    for g in 0..30u64 {
        let p = if g % 2 == 0 {
            big.clone()
        } else {
            let q = dir.path().join(format!("s{g}.txt"));
            fs::write(&q, "small").unwrap();
            q
        };
        pv.request(p, g, PreviewOptions::default());
    }
    let last = recv_gen(&rx, 29);
    assert!(matches!(last.result.body, PreviewBody::Text(_)));
    std::thread::sleep(Duration::from_millis(100));
    while let Ok(ev) = rx.try_recv() {
        assert!(ev.generation <= 29);
    }
}

#[test]
fn highlight_timing_256kb_rust() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("big.rs");
    let src = rust_source(256 * 1024);
    fs::write(&p, &src).unwrap();

    let t = Instant::now();
    ks_preview::highlight::warm_up();
    let load = t.elapsed();

    let t = Instant::now();
    let mut opts = PreviewOptions::default();
    opts.limits.max_text_lines = usize::MAX;
    opts.limits.highlight_budget = Duration::from_secs(60);
    let PreviewBody::Text(t_full) = build(&p, &opts).body else {
        panic!()
    };
    let full = t.elapsed();

    let t = Instant::now();
    let PreviewBody::Text(t_def) = build(&p, &PreviewOptions::default()).body else {
        panic!()
    };
    let def = t.elapsed();

    eprintln!(
        "BENCH syntax-set load {load:?}; highlight 256KB rust: all {} lines {full:?}; default 2000 lines {def:?}",
        t_full.line_count
    );
    assert_eq!(t_def.line_count, 2000);
    // Debug builds of syntect are 10-20x slower and test runs share the machine, so the
    // wall-clock bound only applies to optimised builds; the budget keeps debug bounded.
    if !cfg!(debug_assertions) {
        assert!(
            full < Duration::from_secs(10),
            "highlight too slow: {full:?}"
        );
    }
    assert!(t_full.line_count > 2000, "unlimited run renders every line");
}

// ------------------------------------------------------------ review follow-ups

fn collect_for(rx: &Receiver<PreviewEvent>, window: Duration) -> Vec<PreviewEvent> {
    let end = Instant::now() + window;
    let mut out = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        if let Ok(ev) = rx.recv_timeout(left) {
            out.push(ev);
        }
    }
    out
}

fn slow_opts() -> PreviewOptions {
    let mut o = PreviewOptions::default();
    o.limits.max_text_lines = usize::MAX;
    o.limits.highlight_budget = Duration::from_secs(60);
    o
}

#[test]
fn superseded_request_emits_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.rs");
    fs::write(&big, rust_source(256 * 1024)).unwrap();
    let small = dir.path().join("small.txt");
    fs::write(&small, "small").unwrap();
    let (pv, rx) = Previewer::with_channel();
    pv.request(&big, 1, slow_opts());
    pv.request(&small, 2, PreviewOptions::default());
    let events = collect_for(&rx, Duration::from_millis(1500));
    assert!(
        events.iter().all(|e| e.generation != 1),
        "stale generation 1 was delivered"
    );
    assert!(events.iter().any(|e| e.generation == 2));
}

#[test]
fn cancel_emits_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let big = dir.path().join("big.rs");
    fs::write(&big, rust_source(256 * 1024)).unwrap();
    let (pv, rx) = Previewer::with_channel();
    pv.request(&big, 1, slow_opts());
    pv.cancel();
    assert!(collect_for(&rx, Duration::from_millis(1500)).is_empty());
    // The service still works afterwards.
    pv.request(&big, 2, PreviewOptions::default());
    assert_eq!(recv_gen(&rx, 2).generation, 2);
}

#[test]
fn rapid_requests_never_deliver_stale_generations() {
    let dir = tempfile::tempdir().unwrap();
    let (pv, rx) = Previewer::with_channel();
    let big = dir.path().join("big.rs");
    fs::write(&big, rust_source(256 * 1024)).unwrap();
    let small = dir.path().join("small.txt");
    fs::write(&small, "small").unwrap();
    for g in 0..19u64 {
        pv.request(&big, g, slow_opts());
    }
    pv.request(&small, 19, PreviewOptions::default());
    let events = collect_for(&rx, Duration::from_secs(2));
    let gens: Vec<u64> = events.iter().map(|e| e.generation).collect();
    assert_eq!(gens, vec![19]);
}

#[cfg(windows)]
#[test]
fn locked_file_is_not_cached_and_recovers() {
    use std::os::windows::fs::OpenOptionsExt;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("locked.txt");
    fs::write(&p, "content").unwrap();
    let (pv, rx) = Previewer::with_channel();
    let held = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&p)
        .unwrap();
    pv.request(&p, 1, PreviewOptions::default());
    let ev = recv_gen(&rx, 1);
    assert!(
        matches!(
            ev.result.body,
            PreviewBody::Unavailable {
                reason: UnavailableReason::Locked,
                ..
            }
        ),
        "{:?}",
        ev.result.body
    );
    assert_eq!(pv.cache_len(), 0, "transient failure must not be cached");
    drop(held);
    pv.request(&p, 2, PreviewOptions::default());
    let ev = recv_gen(&rx, 2);
    assert!(!ev.from_cache);
    assert!(matches!(ev.result.body, PreviewBody::Text(_)));
}

#[test]
fn tar_and_gzip_are_metadata_only() {
    let dir = tempfile::tempdir().unwrap();
    let gz = dir.path().join("a.tar.gz");
    fs::write(&gz, [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff, 1, 2, 3]).unwrap();
    let PreviewBody::Archive(a) = build(&gz, &PreviewOptions::default()).body else {
        panic!("gz")
    };
    assert_eq!(a.format, ArchiveFormat::Gzip);
    assert!(!a.listed);

    let tar = dir.path().join("a.tar");
    let mut data = vec![0u8; 512];
    data[..5].copy_from_slice(b"f.txt");
    data[257..262].copy_from_slice(b"ustar");
    fs::write(&tar, data).unwrap();
    let PreviewBody::Archive(a) = build(&tar, &PreviewOptions::default()).body else {
        panic!("tar")
    };
    assert_eq!(a.format, ArchiveFormat::Tar);
    assert!(!a.listed);

    // Magic sniffing without extension.
    let noext = dir.path().join("blob");
    fs::write(&noext, [0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff, 1, 2, 3]).unwrap();
    assert!(matches!(
        build(&noext, &PreviewOptions::default()).body,
        PreviewBody::Archive(_)
    ));
}

#[test]
fn palette_drives_theme_colours() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.rs");
    fs::write(&p, "fn main() { let x = \"s\"; } // note\n").unwrap();
    let mut opts = PreviewOptions::default();
    opts.palette.keyword = Rgb::new(1, 2, 3);
    opts.palette.string = Rgb::new(4, 5, 6);
    opts.palette.comment = Rgb::new(7, 8, 9);
    let PreviewBody::Text(t) = build(&p, &opts).body else {
        panic!()
    };
    let spans = &t.lines[0].spans;
    let find = |needle: &str| spans.iter().find(|s| s.text.contains(needle)).unwrap();
    assert_eq!(find("fn").style.fg, Some(Color::Rgb(Rgb::new(1, 2, 3))));
    assert_eq!(find("\"s\"").style.fg, Some(Color::Rgb(Rgb::new(4, 5, 6))));
    assert_eq!(find("note").style.fg, Some(Color::Rgb(Rgb::new(7, 8, 9))));
    assert!(find("note").style.italic);
    // Text in the default foreground folds to `None`.
    assert!(spans
        .iter()
        .any(|s| s.style.fg.is_none() && s.text.trim().is_empty()));
    // A different palette is a different cache key.
    assert_ne!(opts, PreviewOptions::default());
}

#[test]
fn metadata_request_through_previewer_has_its_own_cache_key() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("m.txt");
    fs::write(&p, "abc").unwrap();
    let (pv, rx) = Previewer::with_channel();
    pv.request(&p, 1, PreviewOptions::default());
    assert!(matches!(recv_gen(&rx, 1).result.body, PreviewBody::Text(_)));
    pv.request(&p, 2, PreviewOptions::metadata());
    let ev = recv_gen(&rx, 2);
    assert!(!ev.from_cache, "metadata must not hit the Auto entry");
    assert!(matches!(ev.result.body, PreviewBody::Metadata));
    assert_eq!(ev.result.meta.as_ref().unwrap().size, 3);
    assert_eq!(pv.cache_len(), 2);
    pv.request(&p, 3, PreviewOptions::metadata());
    assert!(recv_gen(&rx, 3).from_cache);
    assert!(pv.cache_bytes() > 0);
}

#[cfg(windows)]
#[test]
fn junction_target_is_reported_without_verbatim_prefix() {
    use std::os::windows::process::CommandExt;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("real");
    fs::create_dir(&target).unwrap();
    let link = dir.path().join("jn");
    let Ok(out) = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .creation_flags(0x0800_0000)
        .output()
    else {
        return;
    };
    if !out.status.success() {
        eprintln!("mklink /J unavailable; skipping");
        return;
    }
    let m = FileMetadata::read(&link).unwrap();
    assert_eq!(m.kind, EntryKind::Symlink { to_dir: true });
    let t = m.link_target.clone().expect("target");
    assert!(!t.to_string_lossy().starts_with(r"\\?\"), "{t:?}");
    assert!(t.ends_with("real"));
    assert!(!m.link_broken);
    let lines: Vec<String> = m.to_lines().iter().map(|l| l.text()).collect();
    assert!(lines
        .iter()
        .any(|l| l.starts_with("Target") && !l.contains(r"\\?\")));
    // Previewing the junction lists the target directory.
    assert!(matches!(
        build(&link, &PreviewOptions::default()).body,
        PreviewBody::Directory(_)
    ));
}

#[test]
fn unterminated_front_matter_is_not_dimmed() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("a.md");
    fs::write(&p, "---\n# Heading\n\n\t- tabbed\n").unwrap();
    let PreviewBody::Markdown(m) = build(&p, &PreviewOptions::default()).body else {
        panic!()
    };
    assert!(m.lines[1].spans.iter().any(|s| s.style.bold), "heading");
    assert!(
        m.lines[3].text().starts_with("    \u{2022}"),
        "{:?}",
        m.lines[3]
    );
    let p = dir.path().join("b.md");
    fs::write(&p, "---\ntitle: x\n---\n# Real\n").unwrap();
    let PreviewBody::Markdown(m) = build(&p, &PreviewOptions::default()).body else {
        panic!()
    };
    assert!(m.lines[1]
        .spans
        .iter()
        .all(|s| s.style.fg == Some(Color::Role(Role::FgDim))));
    assert!(m.lines[3].spans.iter().any(|s| s.style.bold));
}

#[test]
fn line_counts_and_old_mac_line_endings() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("mac.txt");
    fs::write(&p, "one\rtwo\rthree").unwrap();
    let PreviewBody::Text(t) = build(&p, &PreviewOptions::default()).body else {
        panic!()
    };
    assert_eq!(t.line_count, 3);
    assert_eq!(t.total_lines, Some(3));
    assert_eq!(t.lines[1].text(), "two");

    let big = dir.path().join("big.txt");
    fs::write(&big, "l\n".repeat(5000)).unwrap();
    let PreviewBody::Text(t) = build(&big, &PreviewOptions::default()).body else {
        panic!()
    };
    assert!(t.truncated);
    assert_eq!(t.total_lines, None, "unknown when truncated");
}

#[test]
fn directory_scan_cap_is_flagged() {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..30 {
        fs::write(dir.path().join(format!("f{i}")), "").unwrap();
    }
    let mut opts = PreviewOptions::default();
    opts.limits.dir_scan_limit = 10;
    let PreviewBody::Directory(d) = build(dir.path(), &opts).body else {
        panic!()
    };
    assert!(d.scan_capped && d.truncated);
    assert_eq!(d.total, 10);
    let PreviewBody::Directory(d) = build(dir.path(), &PreviewOptions::default()).body else {
        panic!()
    };
    assert!(!d.scan_capped);
}

#[test]
fn the_injected_resolver_fills_the_owner_of_metadata_requests_only() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("o.txt");
    fs::write(&p, "x").unwrap();
    let mut opts = PreviewOptions::metadata();
    opts.owner = Some(OwnerResolver(|_| Some("DOM-user".to_owned())));
    let pv = build(&p, &opts);
    assert_eq!(pv.meta.unwrap().owner.as_deref(), Some("DOM-user"));
    opts.kind = RequestKind::Auto;
    assert_eq!(build(&p, &opts).meta.unwrap().owner, None);
    assert_eq!(
        build(&p, &PreviewOptions::metadata()).meta.unwrap().owner,
        None
    );
}

#[test]
fn a_stuck_owner_lookup_does_not_hold_the_worker() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("o.txt");
    fs::write(&p, "x").unwrap();
    let mut opts = PreviewOptions::metadata();
    opts.owner = Some(OwnerResolver(|_| {
        std::thread::sleep(std::time::Duration::from_secs(8));
        Some("late".to_owned())
    }));
    let started = std::time::Instant::now();
    let pv = build(&p, &opts);
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert_eq!(pv.meta.unwrap().owner, None);
}

#[test]
fn focus_line_reads_a_window_around_a_deep_line() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("deep.log");
    let body: String = (1..=60_000).map(|i| format!("row {i}\n")).collect();
    fs::write(&p, body).unwrap();
    let mut opts = PreviewOptions::default();
    opts.limits.focus_line = Some(50_000);
    let PreviewBody::Text(t) = build(&p, &opts).body else {
        panic!("not text")
    };
    assert_eq!(t.first_line, 48_999);
    assert_eq!(t.lines.len(), 2000);
    assert!(t.truncated);
    assert_eq!(t.lines[0].text(), "row 49000");
    assert_eq!(t.lines[1000].text(), "row 50000");
    // A focus inside the head limits changes nothing.
    opts.limits.focus_line = Some(100);
    let PreviewBody::Text(t) = build(&p, &opts).body else {
        panic!("not text")
    };
    assert_eq!(t.first_line, 0);
    assert_eq!(t.lines[0].text(), "row 1");
}

#[test]
fn focus_window_counts_lone_cr_and_crlf_like_the_renderer() {
    for (name, eol) in [("mac.txt", "\r"), ("win.txt", "\r\n"), ("unix.txt", "\n")] {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(name);
        let body: String = (1..=60_000).map(|i| format!("row {i}{eol}")).collect();
        fs::write(&p, body).unwrap();
        let mut opts = PreviewOptions::default();
        opts.limits.focus_line = Some(50_000);
        let PreviewBody::Text(t) = build(&p, &opts).body else {
            panic!("not text")
        };
        assert_eq!(t.first_line, 48_999, "{name}");
        assert_eq!(t.lines[0].text(), "row 49000", "{name}");
        assert_eq!(t.lines[1000].text(), "row 50000", "{name}");
    }
}
