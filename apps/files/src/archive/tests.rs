use super::*;
use std::os::unix::fs::symlink;

/// A scratch folder under the system temp dir, removed (read-only
/// folders included) on drop.
struct Scratch(PathBuf, PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "helix-files-archive-{}-{tag}-{n}",
            std::process::id()
        ));
        remove_all(&p);
        fs::create_dir_all(&p).unwrap();
        let work = p.join(".work");
        Scratch(p, work)
    }

    /// The work folder for creating and extracting.
    fn work(&self) -> Option<&Path> {
        Some(self.1.as_path())
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.0.join(rel);
        fs::create_dir_all(&p).unwrap();
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        assert!(self.0.starts_with(std::env::temp_dir()));
        remove_all(&self.0);
    }
}

/// Never cancels; remembers the last progress.
#[derive(Default)]
struct Quiet {
    last: (u64, u64),
}

impl Control for Quiet {
    fn cancelled(&self) -> bool {
        false
    }
    fn progress(&mut self, done: u64, total: u64, _: &str) {
        self.last = (done, total);
    }
}

/// Cancels after being asked `after` times, which is midway through
/// the archives below (it is asked per item and per buffer).
struct CancelAfter {
    after: u32,
    asked: std::cell::Cell<u32>,
}

impl CancelAfter {
    fn new(after: u32) -> Self {
        CancelAfter {
            after,
            asked: Default::default(),
        }
    }
}

impl Control for CancelAfter {
    fn cancelled(&self) -> bool {
        self.asked.set(self.asked.get() + 1);
        self.asked.get() > self.after
    }
    fn progress(&mut self, _: u64, _: u64, _: &str) {}
}

fn names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

fn write(p: &Path, content: &[u8], mode: u32, mtime: i64) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, content).unwrap();
    fs::set_permissions(p, fs::Permissions::from_mode(mode)).unwrap();
    set_mtime(p, mtime, false).unwrap();
}

/// A folder `name` in `dir` with nested and empty folders, a symbolic
/// link, and unusual permissions and times.
fn sample_tree(dir: &Path, name: &str) -> PathBuf {
    let top = dir.join(name);
    write(
        &top.join("docs/notes.txt"),
        b"hello archive\n",
        0o640,
        1_600_000_000,
    );
    write(
        &top.join("docs/deep/er/data.bin"),
        &(0..50_000u32).map(|i| (i % 251) as u8).collect::<Vec<_>>(),
        0o600,
        1_500_000_000,
    );
    write(
        &top.join("run.sh"),
        b"#!/bin/sh\necho hi\n",
        0o755,
        1_400_000_000,
    );
    fs::create_dir_all(top.join("empty")).unwrap();
    symlink("docs/notes.txt", top.join("link-to-notes")).unwrap();
    set_mtime(&top.join("link-to-notes"), 1_300_000_000, true).unwrap();
    fs::set_permissions(top.join("docs/deep"), fs::Permissions::from_mode(0o750)).unwrap();
    set_mtime(&top.join("empty"), 1_200_000_000, false).unwrap();
    set_mtime(&top.join("docs/deep/er"), 1_250_000_000, false).unwrap();
    set_mtime(&top.join("docs/deep"), 1_260_000_000, false).unwrap();
    top
}

/// Checks that `b` matches `a`: types, contents, link targets, modes and
/// modification times.
fn assert_same_tree(a: &Path, b: &Path) {
    let ma = fs::symlink_metadata(a).unwrap();
    let mb = fs::symlink_metadata(b).unwrap_or_else(|e| panic!("{} missing: {e}", b.display()));
    let what = b.display();
    assert_eq!(ma.file_type(), mb.file_type(), "{what}: type");
    assert_eq!(ma.mtime(), mb.mtime(), "{what}: mtime");
    if ma.file_type().is_symlink() {
        assert_eq!(
            fs::read_link(a).unwrap(),
            fs::read_link(b).unwrap(),
            "{what}"
        );
        return;
    }
    assert_eq!(ma.mode() & 0o7777, mb.mode() & 0o7777, "{what}: mode");
    if ma.is_dir() {
        assert_eq!(names(a), names(b), "{what}: contents");
        for n in names(a) {
            assert_same_tree(&a.join(&n), &b.join(&n));
        }
    } else {
        assert_eq!(fs::read(a).unwrap(), fs::read(b).unwrap(), "{what}: data");
    }
}

#[test]
fn formats_and_nicks() {
    for f in Format::ALL {
        assert_eq!(Format::from_nick(f.nick()), Some(f));
        assert_eq!(format!(".{}", f.nick()), f.extension());
    }
    assert_eq!(Format::from_nick("zip"), Some(Format::Zip));
    assert_eq!(Format::from_nick("tar.xz"), Some(Format::TarXz));
    assert_eq!(Format::from_nick("7z"), None);
}

#[test]
fn content_types() {
    assert!(is_archive_type("application/zip"));
    assert!(is_archive_type("application/x-compressed-tar"));
    assert!(is_archive_type("application/x-zstd-compressed-tar"));
    // Zip-based documents are not archives to extract.
    assert!(!is_archive_type("application/vnd.oasis.opendocument.text"));
    assert!(!is_archive_type("application/epub+zip"));
    assert!(!is_archive_type("text/plain"));
}

#[test]
fn sniffs_by_content() {
    assert_eq!(sniff(b"PK\x03\x04rest"), Some(Kind::Zip));
    assert_eq!(
        sniff(&[0x1f, 0x8b, 8, 0]),
        Some(Kind::Tar(Compression::Gzip))
    );
    assert_eq!(
        sniff(&[0xfd, b'7', b'z', b'X', b'Z', 0, 0]),
        Some(Kind::Tar(Compression::Xz))
    );
    assert_eq!(sniff(b"BZh91AY"), Some(Kind::Tar(Compression::Bzip2)));
    assert_eq!(
        sniff(&[0x28, 0xb5, 0x2f, 0xfd, 0]),
        Some(Kind::Tar(Compression::Zstd))
    );
    let mut tar = vec![0u8; 512];
    tar[257..263].copy_from_slice(b"ustar\0");
    assert_eq!(sniff(&tar), Some(Kind::Tar(Compression::None)));
    assert_eq!(sniff(b"just some text"), None);
    assert_eq!(sniff(b""), None);
}

#[test]
fn entry_paths() {
    assert_eq!(safe_rel(b"a/b.txt"), Ok(Some(PathBuf::from("a/b.txt"))));
    assert_eq!(safe_rel(b"./a//b/./c"), Ok(Some(PathBuf::from("a/b/c"))));
    assert_eq!(safe_rel(b"./"), Ok(None));
    assert_eq!(safe_rel(b"/etc/passwd"), Err(Unsafe::Absolute));
    assert_eq!(safe_rel(b"../x"), Err(Unsafe::ParentDir));
    assert_eq!(safe_rel(b"a/../../x"), Err(Unsafe::ParentDir));
    assert_eq!(safe_rel(b"a/.."), Err(Unsafe::ParentDir));
    assert_eq!(safe_rel(b"..."), Ok(Some(PathBuf::from("..."))));
}

#[test]
fn link_targets() {
    let at = |p: &str| PathBuf::from(p);
    assert!(link_stays_inside(&at("l"), b"file"));
    assert!(link_stays_inside(&at("l"), b"./sub/file"));
    assert!(link_stays_inside(&at("a/l"), b"../b"));
    assert!(link_stays_inside(&at("a/b/l"), b"../../c"));
    assert!(link_stays_inside(&at("l"), b"."));
    assert!(!link_stays_inside(&at("l"), b"../x"));
    assert!(!link_stays_inside(&at("a/l"), b"../../x"));
    assert!(!link_stays_inside(&at("l"), b"/etc"));
    assert!(!link_stays_inside(&at("l"), b""));
    // "x/.." goes up from wherever the link x leads.
    assert!(!link_stays_inside(&at("a/l"), b"x/.."));
    assert!(!link_stays_inside(&at("a/l"), b"x/../../y"));
}

/// Every format round-trips a tree: nested and empty folders, links,
/// permissions and modification times.
#[test]
fn round_trip_every_format() {
    for format in Format::ALL {
        let s = Scratch::new("round");
        let src = s.dir("src");
        let top = sample_tree(&src, "Project");
        let target = src.join(format!("Project{}", format.extension()));
        let made = create(
            std::slice::from_ref(&top),
            &target,
            format,
            s.work(),
            &mut Quiet::default(),
        )
        .unwrap_or_else(|e| panic!("{format:?}: {}", e.message()));
        assert_eq!(made, target);
        let head = fs::read(&made).unwrap();
        let expected = match format {
            Format::Zip => Kind::Zip,
            Format::TarGz => Kind::Tar(Compression::Gzip),
            Format::TarXz => Kind::Tar(Compression::Xz),
            Format::TarZst => Kind::Tar(Compression::Zstd),
        };
        assert_eq!(sniff(&head), Some(expected), "{format:?}");
        // Only the archive was added, no temporary files are left.
        assert_eq!(
            names(&src),
            vec!["Project", &format!("Project{}", format.extension())]
        );

        let out = s.dir("out");
        let mut ctl = Quiet::default();
        let got = extract(&made, &out, s.work(), &mut ctl)
            .unwrap_or_else(|e| panic!("{format:?}: {}", e.message()));
        assert_eq!(got, out.join("Project"), "{format:?}");
        assert_eq!(names(&out), vec!["Project"], "{format:?}");
        assert_same_tree(&top, &got);
        assert!(ctl.last.1 > 0, "{format:?}: progress reported");
    }
}

/// Several items go into a folder named after the archive.
#[test]
fn several_top_level_items_get_a_folder() {
    for format in [Format::Zip, Format::TarGz] {
        let s = Scratch::new("several");
        let src = s.dir("src");
        write(&src.join("a.txt"), b"a", 0o644, 1_000_000_000);
        write(&src.join("b.txt"), b"b", 0o644, 1_000_000_000);
        let target = src.join(format!("Archive{}", format.extension()));
        let made = create(
            &[src.join("a.txt"), src.join("b.txt")],
            &target,
            format,
            s.work(),
            &mut Quiet::default(),
        )
        .unwrap();
        let out = s.dir("out");
        let got = extract(&made, &out, s.work(), &mut Quiet::default()).unwrap();
        assert_eq!(got, out.join("Archive"));
        assert_eq!(names(&got), vec!["a.txt", "b.txt"]);
        assert_eq!(fs::read(got.join("b.txt")).unwrap(), b"b");
        // A single file comes out on its own.
        let one = src.join(format!("one{}", format.extension()));
        create(
            &[src.join("a.txt")],
            &one,
            format,
            s.work(),
            &mut Quiet::default(),
        )
        .unwrap();
        let got = extract(&one, &out, s.work(), &mut Quiet::default()).unwrap();
        assert_eq!(got, out.join("a.txt"));
        assert_eq!(names(&out), vec!["Archive", "a.txt"]);
    }
}

/// Taken names get " (2)", " (3)", …; nothing is replaced.
#[test]
fn name_collisions() {
    let s = Scratch::new("collide");
    let src = s.dir("src");
    let top = sample_tree(&src, "Photos");
    let target = src.join("Photos.zip");
    let first = create(
        std::slice::from_ref(&top),
        &target,
        Format::Zip,
        s.work(),
        &mut Quiet::default(),
    )
    .unwrap();
    let second = create(
        std::slice::from_ref(&top),
        &target,
        Format::Zip,
        s.work(),
        &mut Quiet::default(),
    )
    .unwrap();
    assert_eq!(first, target);
    assert_eq!(second, src.join("Photos (2).zip"));

    let out = s.dir("out");
    fs::create_dir(out.join("Photos")).unwrap();
    fs::write(out.join("Photos/mine.txt"), "keep").unwrap();
    let got = extract(&first, &out, s.work(), &mut Quiet::default()).unwrap();
    assert_eq!(got, out.join("Photos (2)"));
    let got = extract(&first, &out, s.work(), &mut Quiet::default()).unwrap();
    assert_eq!(got, out.join("Photos (3)"));
    assert_eq!(names(&out.join("Photos")), vec!["mine.txt"]);
    assert_eq!(fs::read(out.join("Photos/mine.txt")).unwrap(), b"keep");
    // The archive's folder name is used for several items, numbered too.
    write(&src.join("x.txt"), b"x", 0o644, 0);
    write(&src.join("y.txt"), b"y", 0o644, 0);
    let two = create(
        &[src.join("x.txt"), src.join("y.txt")],
        &src.join("pair.tar.gz"),
        Format::TarGz,
        s.work(),
        &mut Quiet::default(),
    )
    .unwrap();
    fs::write(out.join("pair"), "a file named pair").unwrap();
    let got = extract(&two, &out, s.work(), &mut Quiet::default()).unwrap();
    assert_eq!(got, out.join("pair (2)"));
}

// ----- archives made by hand -----

enum E<'a> {
    File(&'a str, &'a [u8], u32),
    Dir(&'a str),
    Link(&'a str, &'a str),
    HardLink(&'a str, &'a str),
    Char(&'a str),
    Fifo(&'a str),
}

/// A tar archive with exactly these entries (names unchecked, as a
/// hostile archive would have them).
fn raw_tar(entries: &[E]) -> Vec<u8> {
    let mut b = tar::Builder::new(Vec::new());
    for e in entries {
        let mut h = tar::Header::new_gnu();
        let (name, kind, data, link): (&str, tar::EntryType, &[u8], &str) = match e {
            E::File(n, d, mode) => {
                h.set_mode(*mode);
                (n, tar::EntryType::Regular, d, "")
            }
            E::Dir(n) => (n, tar::EntryType::Directory, b"", ""),
            E::Link(n, t) => (n, tar::EntryType::Symlink, b"", t),
            E::HardLink(n, t) => (n, tar::EntryType::Link, b"", t),
            E::Char(n) => (n, tar::EntryType::Char, b"", ""),
            E::Fifo(n) => (n, tar::EntryType::Fifo, b"", ""),
        };
        if !matches!(e, E::File(..)) {
            h.set_mode(0o755);
        }
        h.set_entry_type(kind);
        h.set_size(data.len() as u64);
        h.set_mtime(1_000_000_000);
        let gnu = h.as_gnu_mut().unwrap();
        gnu.name[..name.len()].copy_from_slice(name.as_bytes());
        gnu.linkname[..link.len()].copy_from_slice(link.as_bytes());
        h.set_cksum();
        b.append(&h, data).unwrap();
    }
    b.into_inner().unwrap()
}

fn gzip(data: &[u8]) -> Vec<u8> {
    let mut e = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    e.write_all(data).unwrap();
    e.finish().unwrap()
}

/// Extracts `bytes` (saved as `name`) into a fresh `out` folder of `s`
/// and returns the result and the `out` folder.
fn extract_bytes(s: &Scratch, name: &str, bytes: &[u8]) -> (Result<PathBuf, Error>, PathBuf) {
    let src = s.dir("archives");
    let path = src.join(name);
    fs::write(&path, bytes).unwrap();
    let out = s.dir("out");
    (extract(&path, &out, s.work(), &mut Quiet::default()), out)
}

fn assert_unsafe(r: Result<PathBuf, Error>, want: Unsafe, out: &Path, what: &str) {
    match r {
        Err(Error::Unsafe { why, .. }) => assert_eq!(why, want, "{what}"),
        Err(e) => panic!("{what}: wrong error {e:?}"),
        Ok(p) => panic!("{what}: extracted to {}", p.display()),
    }
    // Nothing is left behind, not even the staging folder.
    assert!(names(out).is_empty(), "{what}: left {:?}", names(out));
}

/// Every kind of hostile entry is refused, nothing is written outside
/// the destination, and no partial output stays.
#[test]
fn hostile_tar_archives() {
    let s = Scratch::new("hostile");
    let outside = s.dir("outside");
    let abs = format!("{}/abs.txt", outside.display());
    let link_abs = outside.display().to_string();
    let cases: Vec<(&str, Vec<E>, Unsafe)> = vec![
        (
            "dot-dot",
            vec![
                E::File("ok.txt", b"ok", 0o644),
                E::File("../evil.txt", b"x", 0o644),
            ],
            Unsafe::ParentDir,
        ),
        (
            "nested dot-dot",
            vec![E::Dir("a/"), E::File("a/../../evil.txt", b"x", 0o644)],
            Unsafe::ParentDir,
        ),
        (
            "absolute",
            vec![E::File(&abs, b"x", 0o644)],
            Unsafe::Absolute,
        ),
        (
            "link out by dot-dot",
            vec![E::Link("esc", "../outside")],
            Unsafe::LinkOutside,
        ),
        (
            "absolute link",
            vec![E::Link("esc", &link_abs)],
            Unsafe::LinkOutside,
        ),
        (
            "link through a link",
            vec![
                E::Dir("d/"),
                E::Link("d/here", "."),
                E::Link("d/up", "here/../../outside"),
            ],
            Unsafe::LinkOutside,
        ),
        (
            "hard link out",
            vec![E::HardLink("h", "../outside/secret.txt")],
            Unsafe::BadHardLink,
        ),
        (
            "write through an inside link",
            vec![
                E::Dir("real/"),
                E::Link("alias", "real"),
                E::File("alias/file.txt", b"x", 0o644),
            ],
            Unsafe::ThroughLink,
        ),
    ];
    fs::write(outside.join("secret.txt"), "secret").unwrap();
    for (what, entries, want) in cases {
        let tgz = gzip(&raw_tar(&entries));
        let (r, out) = extract_bytes(&s, "evil.tar.gz", &tgz);
        assert_unsafe(r, want, &out, what);
        assert_eq!(names(&outside), vec!["secret.txt"], "{what}");
        assert_eq!(fs::read(outside.join("secret.txt")).unwrap(), b"secret");
        assert!(!s.0.join("evil.txt").exists(), "{what}");
        assert!(names(&s.1).is_empty(), "{what}: work left");
        fs::remove_dir(&out).unwrap();
    }
}

/// The classic escape: a link to a folder outside, then a file written
/// through it. The link is refused before anything goes through it, and
/// an inside link replaced by a file is unlinked, never written through.
#[test]
fn symlink_escape_then_write() {
    let s = Scratch::new("escape");
    let outside = s.dir("outside");
    let entries = [
        E::Link("dir", "../../outside"),
        E::File("dir/pwned.txt", b"pwned", 0o644),
    ];
    let (r, out) = extract_bytes(&s, "e.tar", &raw_tar(&entries));
    assert_unsafe(r, Unsafe::LinkOutside, &out, "escape");
    assert!(names(&outside).is_empty());
    fs::remove_dir(&out).unwrap();

    // A file entry replacing an earlier (inside) link of the same name.
    let entries = [
        E::File("target.txt", b"original", 0o644),
        E::Link("l", "target.txt"),
        E::File("l", b"replacement", 0o644),
    ];
    let (r, out) = extract_bytes(&s, "r.tar", &raw_tar(&entries));
    let got = r.unwrap();
    assert_eq!(got, out.join("r"));
    assert_eq!(fs::read(got.join("target.txt")).unwrap(), b"original");
    assert_eq!(fs::read(got.join("l")).unwrap(), b"replacement");
    assert!(!fs::symlink_metadata(got.join("l"))
        .unwrap()
        .file_type()
        .is_symlink());
}

/// Device nodes and FIFOs are skipped; set-user-ID, set-group-ID and
/// sticky bits are dropped; hard links inside work.
#[test]
fn special_entries_and_bits() {
    let s = Scratch::new("special");
    let entries = [
        E::Dir("top/"),
        E::File("top/suid", b"x", 0o4755),
        E::File("top/sgid", b"x", 0o2711),
        E::Char("top/null"),
        E::Fifo("top/pipe"),
        E::HardLink("top/again", "top/suid"),
    ];
    let (r, out) = extract_bytes(&s, "s.tar", &raw_tar(&entries));
    let got = r.unwrap();
    assert_eq!(got, out.join("top"));
    assert_eq!(names(&got), vec!["again", "sgid", "suid"]);
    let mode = |n: &str| fs::metadata(got.join(n)).unwrap().mode() & 0o7777;
    assert_eq!(mode("suid"), 0o755);
    assert_eq!(mode("sgid"), 0o711);
    assert_eq!(fs::read(got.join("again")).unwrap(), b"x");
}

/// A zip with "../", an absolute path or an escaping link is refused.
#[test]
fn hostile_zip_archives() {
    let s = Scratch::new("hostile-zip");
    let outside = s.dir("outside");
    let abs = format!("{}/abs.txt", outside.display());
    type Build = Box<dyn Fn(&mut zip::ZipWriter<io::Cursor<Vec<u8>>>)>;
    let file = |name: String| -> Build {
        Box::new(move |z| {
            z.start_file(name.as_str(), zip::write::SimpleFileOptions::default())
                .unwrap();
            z.write_all(b"x").unwrap();
        })
    };
    let cases: Vec<(&str, Build, Unsafe)> = vec![
        ("dot-dot", file("../evil.txt".into()), Unsafe::ParentDir),
        ("absolute", file(abs.clone()), Unsafe::Absolute),
        (
            "link",
            Box::new(|z| {
                z.add_symlink(
                    "esc",
                    "../outside",
                    zip::write::SimpleFileOptions::default(),
                )
                .unwrap();
            }),
            Unsafe::LinkOutside,
        ),
    ];
    for (what, build, want) in cases {
        let mut z = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        build(&mut z);
        let bytes = z.finish().unwrap().into_inner();
        let (r, out) = extract_bytes(&s, "evil.zip", &bytes);
        assert_unsafe(r, want, &out, what);
        assert!(names(&outside).is_empty(), "{what}");
        fs::remove_dir(&out).unwrap();
    }
}

/// An encrypted zip is reported as such, not extracted as garbage.
#[test]
fn encrypted_zip() {
    let s = Scratch::new("encrypted");
    let mut z = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
    z.start_file("secret.txt", zip::write::SimpleFileOptions::default())
        .unwrap();
    z.write_all(b"secret").unwrap();
    let mut bytes = z.finish().unwrap().into_inner();
    // Set the "encrypted" flag in the local and central headers.
    for (sig, flags_at) in [(b"PK\x03\x04", 6), (b"PK\x01\x02", 8)] {
        let at = bytes.windows(4).position(|w| w == sig).unwrap();
        bytes[at + flags_at] |= 1;
    }
    let (r, out) = extract_bytes(&s, "secret.zip", &bytes);
    assert!(matches!(r, Err(Error::Encrypted)), "{r:?}");
    assert!(names(&out).is_empty());
}

/// Tar compressed with bzip2, plain tar, and a misleading name: the
/// content decides.
#[test]
fn other_tar_compressions_by_content() {
    let s = Scratch::new("bz");
    let tar = raw_tar(&[E::Dir("d/"), E::File("d/f.txt", b"bzip", 0o644)]);
    let mut e = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
    e.write_all(&tar).unwrap();
    let bz = e.finish().unwrap();
    for (name, bytes) in [
        ("x.tar.bz2", bz.clone()),
        ("x.tar", tar.clone()),
        ("misnamed.zip", gzip(&tar)),
        ("noext", bz),
    ] {
        let (r, out) = extract_bytes(&s, name, &bytes);
        let got = r.unwrap_or_else(|e| panic!("{name}: {}", e.message()));
        assert_eq!(fs::read(got.join("f.txt")).unwrap(), b"bzip", "{name}");
        remove_all(&out);
    }
    let (r, out) = extract_bytes(&s, "text.tar.gz", b"not an archive at all");
    assert!(matches!(r, Err(Error::NotArchive)), "{r:?}");
    assert!(names(&out).is_empty());
    remove_all(&out);
    // Gzip that isn't a tar inside.
    let (r, out) = extract_bytes(&s, "plain.gz", &gzip(&[7u8; 2000]));
    assert!(matches!(r, Err(Error::Damaged(_))), "{r:?}");
    assert!(names(&out).is_empty());
}

/// Output far beyond the archive's size stops as a bomb, and output
/// beyond the free space stops before the disk fills; nothing stays.
#[test]
fn bombs_and_space() {
    let s = Scratch::new("bomb");
    let zeros = vec![0u8; 4 << 20];
    let tgz = gzip(&raw_tar(&[E::File("zeros", &zeros, 0o644)]));
    assert!(tgz.len() < 64 << 10);
    let src = s.dir("a");
    let path = src.join("bomb.tar.gz");
    fs::write(&path, &tgz).unwrap();
    let out = s.dir("out");
    let tight = Limits {
        reserve: 0,
        max_ratio: 10,
        ratio_floor: 1 << 20,
    };
    let r = extract_with(&path, &out, s.work(), &mut Quiet::default(), &tight);
    assert!(matches!(r, Err(Error::Bomb)), "{r:?}");
    assert!(names(&out).is_empty());
    let full = Limits {
        reserve: u64::MAX,
        ..Limits::default()
    };
    let r = extract_with(&path, &out, s.work(), &mut Quiet::default(), &full);
    assert!(matches!(r, Err(Error::NoSpace { .. })), "{r:?}");
    assert!(names(&out).is_empty());
    // Zip sizes are known up front.
    let mut z = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
    z.start_file("zeros", zip::write::SimpleFileOptions::default())
        .unwrap();
    z.write_all(&zeros).unwrap();
    let zpath = src.join("bomb.zip");
    fs::write(&zpath, z.finish().unwrap().into_inner()).unwrap();
    let r = extract_with(&zpath, &out, s.work(), &mut Quiet::default(), &full);
    assert!(matches!(r, Err(Error::NoSpace { .. })), "{r:?}");
    assert!(names(&out).is_empty());
    let r = extract_with(&zpath, &out, s.work(), &mut Quiet::default(), &tight);
    assert!(matches!(r, Err(Error::Bomb)), "{r:?}");
    assert!(names(&out).is_empty());
}

/// Cancelling midway leaves no partial output, when extracting and when
/// creating.
#[test]
fn cancelling_leaves_nothing() {
    let s = Scratch::new("cancel");
    let src = s.dir("src");
    let top = src.join("big");
    for i in 0..8 {
        let data: Vec<u8> = (0..(1u32 << 20))
            .map(|j| (j.wrapping_mul(i + 7) >> 3) as u8)
            .collect();
        write(&top.join(format!("f{i}.bin")), &data, 0o644, 1_000_000_000);
    }
    for format in Format::ALL {
        let target = src.join(format!("big{}", format.extension()));
        let mut ctl = CancelAfter::new(40);
        let r = create(
            std::slice::from_ref(&top),
            &target,
            format,
            s.work(),
            &mut ctl,
        );
        assert!(matches!(r, Err(Error::Cancelled)), "{format:?}: {r:?}");
        assert_eq!(names(&src), vec!["big"], "{format:?}");

        let made = create(
            std::slice::from_ref(&top),
            &target,
            format,
            s.work(),
            &mut Quiet::default(),
        )
        .unwrap();
        let out = s.dir("out");
        let mut ctl = CancelAfter::new(40);
        let r = extract(&made, &out, s.work(), &mut ctl);
        assert!(matches!(r, Err(Error::Cancelled)), "{format:?}: {r:?}");
        assert!(names(&out).is_empty(), "{format:?}: {:?}", names(&out));
        assert!(names(&s.1).is_empty(), "{format:?}: work left");
        fs::remove_file(&made).unwrap();
    }
}

/// Read-only folders from an archive keep their mode, also at the top.
#[test]
fn read_only_folders() {
    let s = Scratch::new("ro");
    let src = s.dir("src");
    let top = src.join("locked");
    write(&top.join("inner/file.txt"), b"x", 0o444, 1_000_000_000);
    fs::set_permissions(top.join("inner"), fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(&top, fs::Permissions::from_mode(0o555)).unwrap();
    for format in [Format::Zip, Format::TarXz] {
        let made = create(
            std::slice::from_ref(&top),
            &src.join(format!("locked{}", format.extension())),
            format,
            s.work(),
            &mut Quiet::default(),
        )
        .unwrap();
        let out = s.dir("out");
        let got = extract(&made, &out, s.work(), &mut Quiet::default()).unwrap();
        assert_same_tree(&top, &got);
        remove_all(&out);
        fs::remove_file(&made).unwrap();
    }
}

/// Work happens in the work folder when it shares the destination's
/// filesystem, else in the destination; either way nothing is left.
#[test]
fn work_folder() {
    let s = Scratch::new("work");
    let dest = s.dir("dest");
    assert_eq!(work_dir(s.work(), &dest), s.1);
    assert_eq!(work_dir(None, &dest), dest);
    // The work folder is made private.
    assert_eq!(fs::metadata(&s.1).unwrap().mode() & 0o777, 0o700);

    let src = s.dir("src");
    let top = sample_tree(&src, "T");
    let made = create(
        std::slice::from_ref(&top),
        &dest.join("T.tar.gz"),
        Format::TarGz,
        None,
        &mut Quiet::default(),
    )
    .unwrap();
    assert_eq!(names(&dest), vec!["T.tar.gz"]);
    let got = extract(&made, &dest, None, &mut Quiet::default()).unwrap();
    assert_same_tree(&top, &got);
    assert_eq!(names(&dest), vec!["T", "T.tar.gz"]);
    // A refused archive leaves nothing in the destination either.
    let evil = src.join("evil.tar");
    fs::write(
        &evil,
        raw_tar(&[E::File("ok", b"x", 0o644), E::File("../x", b"x", 0o644)]),
    )
    .unwrap();
    assert!(matches!(
        extract(&evil, &dest, None, &mut Quiet::default()),
        Err(Error::Unsafe { .. })
    ));
    assert_eq!(names(&dest), vec!["T", "T.tar.gz"]);
}
