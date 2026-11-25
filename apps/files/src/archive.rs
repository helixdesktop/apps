//! Creating and extracting archives: zip, tar, and tar compressed with
//! gzip, xz, bzip2 or zstd. Plain blocking `std::fs` code with no GTK:
//! `ops.rs` runs it on its worker thread.
//!
//! Archives and extracted items are made in a hidden work folder and
//! moved into place in one step at the end, never replacing anything, so
//! a failed or cancelled operation leaves nothing behind. The work folder
//! is the caller's (Files uses one in the user's cache) when it is on the
//! destination's filesystem, else the destination itself. Outside the
//! destination is better: GTK's `GtkDirectoryList` stops following a
//! folder for good once an entry it was told about vanishes before it
//! could read it (a staging folder that is made and removed quickly),
//! and views of the destination would then miss every later change.
//!
//! Archives are untrusted input. Entries with absolute paths or `..`
//! components, symbolic links that point outside the destination, and
//! entries that would be written through a symbolic link stop the
//! extraction; device nodes and FIFOs are skipped, and set-user-ID,
//! set-group-ID and sticky bits dropped. Extraction stops before it
//! fills the disk, and when an archive expands far more than real data
//! does (a decompression bomb).

use std::ffi::{CString, OsStr, OsString};
use std::fs;
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::naming;
use helix_app_common::i18n::tr;
use helix_app_common::trf;

/// Formats an archive can be created in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Zip,
    TarGz,
    TarXz,
    TarZst,
}

impl Format {
    pub const ALL: [Format; 4] = [Format::Zip, Format::TarGz, Format::TarXz, Format::TarZst];

    pub fn extension(self) -> &'static str {
        match self {
            Format::Zip => ".zip",
            Format::TarGz => ".tar.gz",
            Format::TarXz => ".tar.xz",
            Format::TarZst => ".tar.zst",
        }
    }

    /// Name stored in settings; `zip` and `tar.xz` are also GNOME Files'
    /// `default-compression-format` nicks.
    pub fn nick(self) -> &'static str {
        &self.extension()[1..]
    }

    pub fn from_nick(nick: &str) -> Option<Format> {
        Format::ALL.into_iter().find(|f| f.nick() == nick)
    }
}

/// Compression around a tar archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Compression {
    None,
    Gzip,
    Xz,
    Bzip2,
    Zstd,
}

/// What an archive is, judged by its content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Zip,
    Tar(Compression),
}

/// Content types (shared-mime-info) of the archives Files extracts.
/// Exact types only: documents that are zip files inside (OpenDocument,
/// EPUB, JAR …) are not offered for extraction.
pub const CONTENT_TYPES: &[&str] = &[
    "application/zip",
    "application/x-zip",
    "application/x-zip-compressed",
    "application/x-tar",
    "application/x-gtar",
    "application/x-compressed-tar",
    "application/x-xz-compressed-tar",
    "application/x-bzip-compressed-tar",
    "application/x-bzip2-compressed-tar",
    "application/x-zstd-compressed-tar",
];

pub fn is_archive_type(content_type: &str) -> bool {
    CONTENT_TYPES.contains(&content_type)
}

/// The kind of archive whose first bytes are `head` (512 are enough).
/// Compressed streams are taken to hold a tar archive; extraction fails
/// cleanly when they don't.
pub fn sniff(head: &[u8]) -> Option<Kind> {
    if head.starts_with(b"PK\x03\x04") || head.starts_with(b"PK\x05\x06") {
        Some(Kind::Zip)
    } else if head.starts_with(&[0x1f, 0x8b]) {
        Some(Kind::Tar(Compression::Gzip))
    } else if head.starts_with(&[0xfd, b'7', b'z', b'X', b'Z', 0]) {
        Some(Kind::Tar(Compression::Xz))
    } else if head.starts_with(b"BZh") {
        Some(Kind::Tar(Compression::Bzip2))
    } else if head.starts_with(&[0x28, 0xb5, 0x2f, 0xfd]) {
        Some(Kind::Tar(Compression::Zstd))
    } else if head.get(257..262) == Some(b"ustar") {
        Some(Kind::Tar(Compression::None))
    } else {
        None
    }
}

/// The kind of the archive at `path`, from its content.
pub fn detect(path: &Path) -> Result<Kind, Error> {
    let mut f = fs::File::open(path).map_err(|e| Error::Io(path.into(), e))?;
    let mut head = Vec::with_capacity(512);
    (&mut f)
        .take(512)
        .read_to_end(&mut head)
        .map_err(|e| Error::Io(path.into(), e))?;
    sniff(&head).ok_or(Error::NotArchive)
}

/// How an operation is steered: cancellation and progress.
pub trait Control {
    fn cancelled(&self) -> bool;
    /// `done` of `total` (bytes); `current` is the entry being handled.
    fn progress(&mut self, done: u64, total: u64, current: &str);
}

/// Why an entry was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unsafe {
    /// An absolute path.
    Absolute,
    /// A `..` component.
    ParentDir,
    /// A symbolic link to somewhere outside the destination.
    LinkOutside,
    /// A hard link to something that isn't a file extracted before.
    BadHardLink,
    /// Writing it would go through a symbolic link from the archive.
    ThroughLink,
}

#[derive(Debug)]
pub enum Error {
    Cancelled,
    /// A file or folder on disk (not the archive's content) failed.
    Io(PathBuf, io::Error),
    /// The archive could not be read: damaged, truncated, or not the
    /// format it looks like.
    Damaged(String),
    NotArchive,
    Encrypted,
    Unsafe {
        entry: String,
        why: Unsafe,
    },
    /// An entry is both a folder and something else.
    Conflict(String),
    NoSpace {
        needed: u64,
        available: u64,
    },
    Bomb,
}

impl Error {
    pub fn message(&self) -> String {
        let size = crate::fmt::size;
        match self {
            Error::Cancelled => tr("Cancelled").into(),
            Error::Io(path, e) => trf!(
                "“{file}”: {error}",
                file = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                error = helix_app_common::errors::io_detail(e)
            ),
            Error::Damaged(detail) => trf!(
                "The archive is damaged or in an unsupported format ({detail}).",
                detail = detail
            ),
            Error::NotArchive => tr("This is not an archive Files can extract.").into(),
            Error::Encrypted => tr("Encrypted archives are not supported.").into(),
            Error::Unsafe { entry, why } => match why {
                Unsafe::Absolute => trf!(
                    "“{entry}” has an absolute path, so the archive is unsafe to extract.",
                    entry = entry
                ),
                Unsafe::ParentDir => trf!(
                    "“{entry}” points outside the destination folder, so the archive is unsafe to extract.",
                    entry = entry
                ),
                Unsafe::LinkOutside => trf!(
                    "The link “{entry}” points outside the destination folder, so the archive is unsafe to extract.",
                    entry = entry
                ),
                Unsafe::BadHardLink => trf!(
                    "The hard link “{entry}” does not point to a file in the archive, so the archive is unsafe to extract.",
                    entry = entry
                ),
                Unsafe::ThroughLink => trf!(
                    "“{entry}” would be written through a link, so the archive is unsafe to extract.",
                    entry = entry
                ),
            },
            Error::Conflict(entry) => trf!(
                "“{entry}” clashes with another item in the archive.",
                entry = entry
            ),
            Error::NoSpace { needed, available } => trf!(
                "There is not enough free space: {needed} needed, {available} available.",
                needed = size(*needed),
                available = size(*available)
            ),
            Error::Bomb => tr(
                "The archive expands to far more data than it holds, as a decompression bomb would, so it was not extracted.",
            )
            .into(),
        }
    }

    fn unsafe_entry(name: &[u8], why: Unsafe) -> Error {
        Error::Unsafe {
            entry: String::from_utf8_lossy(name).into_owned(),
            why,
        }
    }
}

fn damaged(e: impl std::fmt::Display) -> Error {
    Error::Damaged(e.to_string())
}

/// Limits that keep extraction from filling the disk or running away.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Space left free on the destination's filesystem.
    pub reserve: u64,
    /// Output allowed per byte of archive …
    pub max_ratio: u64,
    /// … once the output is larger than this.
    pub ratio_floor: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            reserve: 64 << 20,
            max_ratio: 1000,
            ratio_floor: 1 << 30,
        }
    }
}

// ----- paths -----

/// The path an entry named `name` (raw bytes, `/`-separated) gets under
/// the destination, or `None` for the destination itself ("./").
fn safe_rel(name: &[u8]) -> Result<Option<PathBuf>, Unsafe> {
    if name.first() == Some(&b'/') {
        return Err(Unsafe::Absolute);
    }
    let mut out = PathBuf::new();
    for comp in name.split(|&b| b == b'/') {
        match comp {
            b"" | b"." => {}
            b".." => return Err(Unsafe::ParentDir),
            c => out.push(OsStr::from_bytes(c)),
        }
    }
    Ok((!out.as_os_str().is_empty()).then_some(out))
}

/// Whether a symbolic link at `rel` (under the destination) pointing to
/// `target` stays inside the destination. The target must be relative and
/// its `..` components must all come first and climb no higher than the
/// destination: after a link, `..` goes up from wherever the link leads,
/// so `a/..` is only `.` when `a` is a real folder.
fn link_stays_inside(rel: &Path, target: &[u8]) -> bool {
    if target.is_empty() || target[0] == b'/' {
        return false;
    }
    let depth = rel.components().count().saturating_sub(1);
    let mut ups = 0;
    let mut descended = false;
    for comp in target.split(|&b| b == b'/') {
        match comp {
            b"" | b"." => {}
            b".." => {
                if descended {
                    return false;
                }
                ups += 1;
                if ups > depth {
                    return false;
                }
            }
            _ => descended = true,
        }
    }
    true
}

fn cstring(p: &Path) -> io::Result<CString> {
    CString::new(p.as_os_str().as_bytes()).map_err(io::Error::other)
}

/// Renames `from` to `to` unless `to` exists (then `AlreadyExists`).
fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    let (f, t) = (cstring(from)?, cstring(to)?);
    // SAFETY: both are valid NUL-terminated paths.
    let r = unsafe {
        libc::renameat2(
            libc::AT_FDCWD,
            f.as_ptr(),
            libc::AT_FDCWD,
            t.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    if r == 0 {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    if matches!(e.raw_os_error(), Some(libc::EINVAL) | Some(libc::ENOSYS)) {
        // A filesystem without RENAME_NOREPLACE: check, then rename.
        if fs::symlink_metadata(to).is_ok() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        return fs::rename(from, to);
    }
    Err(e)
}

/// Moves `from` into `dir` as `name`, or as "name (2)", … when that is
/// taken. Never replaces anything.
fn rename_free(from: &Path, dir: &Path, name: &OsStr) -> Result<PathBuf, Error> {
    let lossy = name.to_string_lossy().into_owned();
    for attempt in 0..1000 {
        let candidate: OsString = if attempt == 0 {
            name.to_owned()
        } else {
            naming::numbered_name(&lossy, |n| fs::symlink_metadata(dir.join(n)).is_ok()).into()
        };
        let to = dir.join(&candidate);
        match rename_noreplace(from, &to) {
            Ok(()) => return Ok(to),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::Io(to, e)),
        }
    }
    Err(Error::Io(
        dir.join(name),
        io::ErrorKind::AlreadyExists.into(),
    ))
}

/// A new hidden entry in `dir` made by `make` (which fails with
/// `AlreadyExists` when the name is taken).
fn hidden_new(
    dir: &Path,
    what: &str,
    make: impl Fn(&Path) -> io::Result<()>,
) -> Result<PathBuf, Error> {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    loop {
        let n = N.fetch_add(1, Ordering::Relaxed);
        let p = dir.join(format!(".helix-{what}-{}-{n}", std::process::id()));
        match make(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::Io(dir.into(), e)),
        }
    }
}

/// The folder for work on things going into `dest`: `work` (made if
/// needed) when it is on the same filesystem, else `dest`.
fn work_dir(work: Option<&Path>, dest: &Path) -> PathBuf {
    if let Some(w) = work {
        let made = fs::DirBuilder::new().recursive(true).mode(0o700).create(w);
        if let (Ok(()), Ok(a), Ok(b)) = (made, fs::metadata(w), fs::metadata(dest)) {
            if a.dev() == b.dev() && a.is_dir() {
                return w.to_path_buf();
            }
        }
    }
    dest.to_path_buf()
}

/// Removes `p` and everything in it, first making folders writable (an
/// archive may have made some read-only).
fn remove_all(p: &Path) {
    let Ok(meta) = fs::symlink_metadata(p) else {
        return;
    };
    if meta.is_dir() {
        let _ = fs::set_permissions(p, fs::Permissions::from_mode(0o700));
        if let Ok(rd) = fs::read_dir(p) {
            for e in rd.flatten() {
                remove_all(&e.path());
            }
        }
        let _ = fs::remove_dir(p);
    } else {
        let _ = fs::remove_file(p);
    }
}

/// Bytes available to the user on the filesystem holding `dir`.
fn free_space(dir: &Path) -> io::Result<u64> {
    let c = cstring(dir)?;
    // SAFETY: `st` is plain data filled in by statvfs.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((st.f_bavail as u64).saturating_mul(st.f_frsize as u64))
}

fn set_mtime(p: &Path, secs: i64, is_link: bool) -> io::Result<()> {
    let offset = std::time::Duration::from_secs(secs.unsigned_abs());
    let t = if secs >= 0 {
        std::time::UNIX_EPOCH + offset
    } else {
        std::time::UNIX_EPOCH - offset
    };
    let times = fs::FileTimes::new().set_modified(t);
    if is_link {
        fs::set_times_nofollow(p, times)
    } else {
        fs::set_times(p, times)
    }
}

// ----- extraction -----

/// Where extracted entries go: a staging folder, written only through
/// real folders made here.
struct Sink<'a> {
    root: PathBuf,
    /// Folders to give their mode and time when all is written.
    dirs: Vec<(PathBuf, u32, Option<i64>)>,
    written: u64,
    space: u64,
    available: u64,
    bomb: u64,
    /// Progress: bytes of the archive read so far (tar), else `written`.
    read: Option<std::rc::Rc<std::cell::Cell<u64>>>,
    total: u64,
    ctl: &'a mut dyn Control,
    last_report: std::time::Instant,
}

impl Sink<'_> {
    fn check(&self) -> Result<(), Error> {
        if self.ctl.cancelled() {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }

    fn report(&mut self, entry: &Path, force: bool) {
        if force || self.last_report.elapsed() > std::time::Duration::from_millis(50) {
            self.last_report = std::time::Instant::now();
            let done = self.read.as_ref().map_or(self.written, |r| r.get());
            self.ctl
                .progress(done, self.total, &entry.to_string_lossy());
        }
    }

    fn account(&mut self, n: u64) -> Result<(), Error> {
        self.written += n;
        if self.written > self.space {
            return Err(Error::NoSpace {
                needed: self.written,
                available: self.available,
            });
        }
        if self.written > self.bomb {
            return Err(Error::Bomb);
        }
        Ok(())
    }

    /// Makes the folders above `rel` (refusing to go through anything
    /// but real folders) and clears the way for `rel` itself, which is
    /// returned as a full path.
    fn prepare(&mut self, rel: &Path, name: &[u8], keep_dir: bool) -> Result<PathBuf, Error> {
        let mut cur = self.root.clone();
        let comps: Vec<_> = rel.components().collect();
        let Some((last, parents)) = comps.split_last() else {
            return Ok(cur);
        };
        for c in parents {
            cur.push(c);
            match fs::symlink_metadata(&cur) {
                Ok(m) if m.is_dir() => {}
                Ok(m) if m.file_type().is_symlink() => {
                    return Err(Error::unsafe_entry(name, Unsafe::ThroughLink))
                }
                Ok(_) => return Err(Error::Conflict(String::from_utf8_lossy(name).into())),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    fs::DirBuilder::new()
                        .mode(0o700)
                        .create(&cur)
                        .map_err(|e| Error::Io(cur.clone(), e))?;
                    self.dirs.push((cur.clone(), 0o755, None));
                }
                Err(e) => return Err(Error::Io(cur, e)),
            }
        }
        cur.push(last);
        match fs::symlink_metadata(&cur) {
            Ok(m) if m.is_dir() => {
                if !keep_dir {
                    return Err(Error::Conflict(String::from_utf8_lossy(name).into()));
                }
            }
            Ok(m) if keep_dir && m.file_type().is_symlink() => {
                return Err(Error::unsafe_entry(name, Unsafe::ThroughLink))
            }
            Ok(_) if keep_dir => return Err(Error::Conflict(String::from_utf8_lossy(name).into())),
            // A later entry of the same name replaces a file or link (as
            // tar does); unlinking never follows a link.
            Ok(_) => fs::remove_file(&cur).map_err(|e| Error::Io(cur.clone(), e))?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(Error::Io(cur, e)),
        }
        Ok(cur)
    }

    fn file(
        &mut self,
        rel: &Path,
        name: &[u8],
        mode: u32,
        mtime: Option<i64>,
        data: &mut dyn Read,
    ) -> Result<u64, Error> {
        self.check()?;
        let path = self.prepare(rel, name, false)?;
        let io_err = |e| Error::Io(path.clone(), e);
        // create_new: O_EXCL never follows (or writes through) a link.
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(io_err)?;
        let mut buf = vec![0u8; 64 * 1024];
        let mut size = 0u64;
        loop {
            self.check()?;
            let n = match data.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(damaged(e)),
            };
            self.account(n as u64)?;
            size += n as u64;
            f.write_all(&buf[..n]).map_err(io_err)?;
            self.report(rel, false);
        }
        f.set_permissions(fs::Permissions::from_mode(mode & 0o777))
            .map_err(io_err)?;
        drop(f);
        if let Some(t) = mtime {
            set_mtime(&path, t, false).map_err(io_err)?;
        }
        self.report(rel, false);
        Ok(size)
    }

    fn dir(&mut self, rel: &Path, name: &[u8], mode: u32, mtime: Option<i64>) -> Result<(), Error> {
        self.check()?;
        let path = self.prepare(rel, name, true)?;
        if !path.exists() {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(&path)
                .map_err(|e| Error::Io(path.clone(), e))?;
        }
        self.dirs.push((path, mode, mtime));
        Ok(())
    }

    fn symlink(
        &mut self,
        rel: &Path,
        name: &[u8],
        target: &[u8],
        mtime: Option<i64>,
    ) -> Result<(), Error> {
        self.check()?;
        if !link_stays_inside(rel, target) {
            return Err(Error::unsafe_entry(name, Unsafe::LinkOutside));
        }
        let path = self.prepare(rel, name, false)?;
        std::os::unix::fs::symlink(OsStr::from_bytes(target), &path)
            .map_err(|e| Error::Io(path.clone(), e))?;
        if let Some(t) = mtime {
            set_mtime(&path, t, true).map_err(|e| Error::Io(path.clone(), e))?;
        }
        Ok(())
    }

    fn hard_link(&mut self, rel: &Path, name: &[u8], target: &[u8]) -> Result<(), Error> {
        self.check()?;
        let bad = || Error::unsafe_entry(name, Unsafe::BadHardLink);
        let target_rel = safe_rel(target).map_err(|_| bad())?.ok_or_else(bad)?;
        // The target must be a file reached through real folders.
        let mut cur = self.root.clone();
        for c in target_rel.components() {
            cur.push(c);
            let m = fs::symlink_metadata(&cur).map_err(|_| bad())?;
            if m.file_type().is_symlink() {
                return Err(bad());
            }
        }
        if !cur.is_file() {
            return Err(bad());
        }
        if self.root.join(rel) == cur {
            return Ok(());
        }
        let path = self.prepare(rel, name, false)?;
        fs::hard_link(&cur, &path).map_err(|e| Error::Io(path.clone(), e))
    }

    /// Gives the folders their modes and times, deepest first, except the
    /// top-level ones (returned for after they are moved into place: a
    /// read-only folder can't be moved).
    fn finish(&mut self) -> Result<Vec<(PathBuf, u32, Option<i64>)>, Error> {
        let mut dirs = std::mem::take(&mut self.dirs);
        dirs.sort_by_key(|d| std::cmp::Reverse(d.0.components().count()));
        let mut top = Vec::new();
        for (path, mode, mtime) in dirs {
            if path.parent() == Some(self.root.as_path()) {
                top.push((path, mode, mtime));
                continue;
            }
            apply_dir_meta(&path, mode, mtime)?;
        }
        Ok(top)
    }
}

fn apply_dir_meta(path: &Path, mode: u32, mtime: Option<i64>) -> Result<(), Error> {
    if let Some(t) = mtime {
        set_mtime(path, t, false).map_err(|e| Error::Io(path.into(), e))?;
    }
    fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777))
        .map_err(|e| Error::Io(path.into(), e))
}

/// Counts the bytes read from the archive file, for progress.
struct Counting<R> {
    inner: R,
    count: std::rc::Rc<std::cell::Cell<u64>>,
}

impl<R: Read> Read for Counting<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.count.set(self.count.get() + n as u64);
        Ok(n)
    }
}

fn decompressor(c: Compression, r: Box<dyn Read>) -> Result<Box<dyn Read>, Error> {
    Ok(match c {
        Compression::None => r,
        Compression::Gzip => Box::new(flate2::read::MultiGzDecoder::new(r)),
        Compression::Xz => Box::new(lzma_rust2::XzReader::new(r, true)),
        Compression::Bzip2 => Box::new(bzip2::read::MultiBzDecoder::new(r)),
        Compression::Zstd => Box::new(zstd::Decoder::new(r).map_err(damaged)?),
    })
}

const S_IFMT: u32 = 0o170000;
const S_IFLNK: u32 = 0o120000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;

fn extract_tar(c: Compression, file: fs::File, sink: &mut Sink) -> Result<(), Error> {
    let count = std::rc::Rc::new(std::cell::Cell::new(0));
    sink.read = Some(count.clone());
    let counting = Counting {
        inner: BufReader::new(file),
        count,
    };
    let mut ar = tar::Archive::new(decompressor(c, Box::new(counting))?);
    for entry in ar.entries().map_err(damaged)? {
        sink.check()?;
        let mut e = entry.map_err(damaged)?;
        let name = e.path_bytes().into_owned();
        let rel = safe_rel(&name).map_err(|why| Error::unsafe_entry(&name, why))?;
        let h = e.header();
        let mode = h.mode().unwrap_or(0o644);
        let mtime = h.mtime().ok().map(|t| t as i64);
        let kind = h.entry_type();
        use tar::EntryType as T;
        let Some(rel) = rel else {
            continue;
        };
        match kind {
            T::Regular | T::Continuous | T::GNUSparse => {
                sink.file(&rel, &name, mode, mtime, &mut e)?;
            }
            T::Directory => sink.dir(&rel, &name, mode, mtime)?,
            T::Symlink => {
                let target = e
                    .link_name_bytes()
                    .map(|t| t.into_owned())
                    .unwrap_or_default();
                sink.symlink(&rel, &name, &target, mtime)?;
            }
            T::Link => {
                let target = e
                    .link_name_bytes()
                    .map(|t| t.into_owned())
                    .unwrap_or_default();
                sink.hard_link(&rel, &name, &target)?;
            }
            // Device nodes, FIFOs and anything unknown are skipped.
            _ => {}
        }
        sink.report(&rel, false);
    }
    Ok(())
}

/// Modification time of a zip entry: the Unix extended timestamp when
/// there is one, else the MS-DOS local time.
fn zip_mtime<R: Read>(f: &zip::read::ZipFile<'_, R>) -> Option<i64> {
    for field in f.extra_data_fields() {
        if let zip::ExtraField::ExtendedTimestamp(ts) = field {
            if let Some(t) = ts.mod_time() {
                return Some(t as i64);
            }
        }
    }
    f.last_modified().and_then(from_dos_time)
}

fn from_dos_time(d: zip::DateTime) -> Option<i64> {
    // SAFETY: plain data, filled in below and read by mktime.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    tm.tm_year = d.year() as i32 - 1900;
    tm.tm_mon = d.month() as i32 - 1;
    tm.tm_mday = d.day() as i32;
    tm.tm_hour = d.hour() as i32;
    tm.tm_min = d.minute() as i32;
    tm.tm_sec = d.second() as i32;
    tm.tm_isdst = -1;
    let t = unsafe { libc::mktime(&mut tm) };
    (t != -1).then_some(t as i64)
}

fn to_dos_time(secs: i64) -> zip::DateTime {
    let t = secs as libc::time_t;
    // SAFETY: plain data, filled in by localtime_r.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return zip::DateTime::default();
    }
    zip::DateTime::from_date_and_time(
        (tm.tm_year + 1900).clamp(1980, 2107) as u16,
        (tm.tm_mon + 1) as u8,
        tm.tm_mday as u8,
        tm.tm_hour as u8,
        tm.tm_min as u8,
        tm.tm_sec.min(59) as u8,
    )
    .unwrap_or_default()
}

fn extract_zip(file: fs::File, sink: &mut Sink) -> Result<(), Error> {
    let mut zip = zip::ZipArchive::new(BufReader::new(file)).map_err(damaged)?;
    let mut total = 0u64;
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i).map_err(damaged)?;
        if f.encrypted() {
            return Err(Error::Encrypted);
        }
        total = total.saturating_add(f.size());
    }
    if total > sink.space {
        return Err(Error::NoSpace {
            needed: total,
            available: sink.available,
        });
    }
    sink.total = total;
    for i in 0..zip.len() {
        sink.check()?;
        let mut f = zip.by_index(i).map_err(damaged)?;
        let name = match f.name() {
            Ok(n) => n.as_bytes().to_vec(),
            Err(_) => f.name_raw().to_vec(),
        };
        let rel = safe_rel(&name).map_err(|why| Error::unsafe_entry(&name, why))?;
        let Some(rel) = rel else {
            continue;
        };
        let mode = f.unix_mode();
        let mtime = zip_mtime(&f);
        let kind = mode.map(|m| m & S_IFMT).filter(|&k| k != 0);
        if f.is_symlink() || kind == Some(S_IFLNK) {
            let mut target = Vec::new();
            (&mut f)
                .take(4096)
                .read_to_end(&mut target)
                .map_err(damaged)?;
            sink.symlink(&rel, &name, &target, mtime)?;
        } else if f.is_dir() || kind == Some(S_IFDIR) {
            sink.dir(&rel, &name, mode.unwrap_or(0o755), mtime)?;
        } else if kind.is_none() || kind == Some(S_IFREG) {
            let declared = f.size();
            let written = sink.file(
                &rel,
                &name,
                mode.unwrap_or(0o644),
                mtime,
                &mut (&mut f).take(declared.saturating_add(1)),
            )?;
            if written > declared {
                return Err(Error::Damaged(trf!(
                    "“{entry}” is larger than it says",
                    entry = String::from_utf8_lossy(&name)
                )));
            }
        }
        // Device nodes, FIFOs and sockets are skipped.
        sink.report(&rel, false);
    }
    Ok(())
}

/// Extracts `archive` into the folder `dest` and returns what it made:
/// the archive's one top-level item when it has only one, else a new
/// folder named after the archive holding its items. Existing files are
/// never replaced: taken names become "name (2)", ….
/// Work goes in `work` (see the module documentation).
pub fn extract(
    archive: &Path,
    dest: &Path,
    work: Option<&Path>,
    ctl: &mut dyn Control,
) -> Result<PathBuf, Error> {
    extract_with(archive, dest, work, ctl, &Limits::default())
}

pub fn extract_with(
    archive: &Path,
    dest: &Path,
    work: Option<&Path>,
    ctl: &mut dyn Control,
    limits: &Limits,
) -> Result<PathBuf, Error> {
    let kind = detect(archive)?;
    let file = fs::File::open(archive).map_err(|e| Error::Io(archive.into(), e))?;
    let archive_size = file
        .metadata()
        .map_err(|e| Error::Io(archive.into(), e))?
        .len();
    let available = free_space(dest).map_err(|e| Error::Io(dest.into(), e))?;
    let file_name = archive
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = naming::split_extension(&file_name).0.to_string();
    let staging = hidden_new(&work_dir(work, dest), "extract", |p| {
        fs::DirBuilder::new().mode(0o700).create(p)
    })?;
    let mut sink = Sink {
        root: staging.clone(),
        dirs: Vec::new(),
        written: 0,
        space: available.saturating_sub(limits.reserve),
        available,
        bomb: archive_size
            .saturating_mul(limits.max_ratio)
            .max(limits.ratio_floor),
        read: None,
        total: archive_size,
        ctl,
        last_report: std::time::Instant::now(),
    };
    let result = match kind {
        Kind::Zip => extract_zip(file, &mut sink),
        Kind::Tar(c) => extract_tar(c, file, &mut sink),
    }
    .and_then(|()| sink.check())
    .and_then(|()| {
        let total = sink.total;
        sink.ctl.progress(total, total, "");
        sink.finish()
    })
    .and_then(|top| place(&staging, dest, &stem, top));
    if result.is_err() {
        remove_all(&staging);
    }
    result
}

/// Moves the extracted items from `staging` into `dest`.
fn place(
    staging: &Path,
    dest: &Path,
    stem: &str,
    top: Vec<(PathBuf, u32, Option<i64>)>,
) -> Result<PathBuf, Error> {
    let names: Vec<OsString> = fs::read_dir(staging)
        .map_err(|e| Error::Io(staging.into(), e))?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<Result<_, _>>()
        .map_err(|e| Error::Io(staging.into(), e))?;
    // Where the top-level folders went (`None`: the result itself).
    let (result, holder) = if let [name] = names.as_slice() {
        let to = rename_free(&staging.join(name), dest, name)?;
        let _ = fs::remove_dir(staging);
        (to, None)
    } else {
        let stem = if stem.is_empty() { "Archive" } else { stem };
        let to = rename_free(staging, dest, OsStr::new(stem))?;
        fs::set_permissions(&to, fs::Permissions::from_mode(0o755))
            .map_err(|e| Error::Io(to.clone(), e))?;
        (to.clone(), Some(to))
    };
    for (path, mode, mtime) in top {
        let moved = match &holder {
            Some(h) => h.join(path.file_name().unwrap_or_default()),
            None => result.clone(),
        };
        apply_dir_meta(&moved, mode, mtime)?;
    }
    Ok(result)
}

// ----- creation -----

/// One item to archive.
struct Item {
    path: PathBuf,
    /// Its name in the archive, `/`-separated.
    name: String,
    meta: fs::Metadata,
}

/// Lists `sources` and everything in them (not following links), each
/// folder before its contents.
fn collect(sources: &[PathBuf], skip: &Path, ctl: &dyn Control) -> Result<Vec<Item>, Error> {
    let mut items = Vec::new();
    let mut stack: Vec<(PathBuf, String)> = sources
        .iter()
        .rev()
        .map(|s| {
            let name = s
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (s.clone(), name)
        })
        .collect();
    while let Some((path, name)) = stack.pop() {
        if ctl.cancelled() {
            return Err(Error::Cancelled);
        }
        if path == skip {
            continue;
        }
        let meta = fs::symlink_metadata(&path).map_err(|e| Error::Io(path.clone(), e))?;
        if meta.is_dir() {
            let mut children: Vec<OsString> = fs::read_dir(&path)
                .map_err(|e| Error::Io(path.clone(), e))?
                .filter_map(|e| e.ok().map(|e| e.file_name()))
                .collect();
            children.sort();
            for c in children.iter().rev() {
                stack.push((path.join(c), format!("{name}/{}", c.to_string_lossy())));
            }
        } else if !meta.is_file() && !meta.file_type().is_symlink() {
            // Sockets, FIFOs and device nodes are left out.
            continue;
        }
        items.push(Item { path, name, meta });
    }
    Ok(items)
}

/// Reads a file being archived, for progress and cancellation.
struct Feed<'a, 'c> {
    inner: io::Take<fs::File>,
    done: &'a mut u64,
    total: u64,
    name: &'a str,
    ctl: &'a mut (dyn Control + 'c),
    last_report: &'a mut std::time::Instant,
}

impl Read for Feed<'_, '_> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.ctl.cancelled() {
            // Not `Interrupted`: io::copy retries those.
            return Err(io::Error::other("cancelled"));
        }
        let n = self.inner.read(buf)?;
        *self.done += n as u64;
        if self.last_report.elapsed() > std::time::Duration::from_millis(50) {
            *self.last_report = std::time::Instant::now();
            self.ctl.progress(*self.done, self.total, self.name);
        }
        Ok(n)
    }
}

/// A compressed tar stream being written.
enum Encoder<W: Write> {
    Gz(flate2::write::GzEncoder<W>),
    Xz(lzma_rust2::XzWriter<W>),
    Zst(zstd::Encoder<'static, W>),
}

impl<W: Write> Write for Encoder<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Encoder::Gz(e) => e.write(buf),
            Encoder::Xz(e) => e.write(buf),
            Encoder::Zst(e) => e.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Encoder::Gz(e) => e.flush(),
            Encoder::Xz(e) => e.flush(),
            Encoder::Zst(e) => e.flush(),
        }
    }
}

impl<W: Write> Encoder<W> {
    fn new(format: Format, w: W) -> io::Result<Self> {
        Ok(match format {
            Format::TarXz => Encoder::Xz(lzma_rust2::XzWriter::new(
                w,
                lzma_rust2::XzOptions::with_preset(6),
            )?),
            Format::TarZst => Encoder::Zst(zstd::Encoder::new(w, 3)?),
            _ => Encoder::Gz(flate2::write::GzEncoder::new(
                w,
                flate2::Compression::default(),
            )),
        })
    }
    fn finish(self) -> io::Result<W> {
        match self {
            Encoder::Gz(e) => e.finish(),
            Encoder::Xz(e) => e.finish(),
            Encoder::Zst(e) => e.finish(),
        }
    }
}

/// Maps an error from writing the archive: cancellation, else trouble
/// with the archive file being written.
fn write_err(ctl: &dyn Control, path: &Path, e: io::Error) -> Error {
    if ctl.cancelled() {
        Error::Cancelled
    } else {
        Error::Io(path.into(), e)
    }
}

fn mtime_of(meta: &fs::Metadata) -> i64 {
    meta.mtime()
}

fn write_tar<W: Write>(
    w: W,
    items: &[Item],
    total: u64,
    tmp: &Path,
    ctl: &mut dyn Control,
) -> Result<W, Error> {
    let mut b = tar::Builder::new(w);
    b.mode(tar::HeaderMode::Complete);
    b.follow_symlinks(false);
    let mut done = 0u64;
    let mut last_report = std::time::Instant::now();
    for it in items {
        if ctl.cancelled() {
            return Err(Error::Cancelled);
        }
        let mut h = tar::Header::new_gnu();
        h.set_metadata_in_mode(&it.meta, tar::HeaderMode::Complete);
        let r = if it.meta.is_dir() {
            h.set_size(0);
            b.append_data(&mut h, &it.name, io::empty())
        } else if it.meta.file_type().is_symlink() {
            let target = fs::read_link(&it.path).map_err(|e| Error::Io(it.path.clone(), e))?;
            h.set_size(0);
            b.append_link(&mut h, &it.name, &target)
        } else {
            let f = fs::File::open(&it.path).map_err(|e| Error::Io(it.path.clone(), e))?;
            let size = it.meta.len();
            let before = done;
            let feed = Feed {
                inner: f.take(size),
                done: &mut done,
                total,
                name: &it.name,
                ctl: &mut *ctl,
                last_report: &mut last_report,
            };
            let r = b.append_data(&mut h, &it.name, feed);
            if r.is_ok() && done - before != size {
                return Err(Error::Io(
                    it.path.clone(),
                    io::Error::other(tr("The file changed while it was being archived.")),
                ));
            }
            r
        };
        r.map_err(|e| write_err(ctl, tmp, e))?;
        ctl.progress(done, total, &it.name);
    }
    b.into_inner().map_err(|e| write_err(ctl, tmp, e))
}

fn write_zip(
    w: BufWriter<fs::File>,
    items: &[Item],
    total: u64,
    tmp: &Path,
    ctl: &mut dyn Control,
) -> Result<BufWriter<fs::File>, Error> {
    use zip::write::SimpleFileOptions;
    let zip_err = |ctl: &dyn Control, e: zip::result::ZipError| match e {
        zip::result::ZipError::Io(e) => write_err(ctl, tmp, e),
        e => Error::Io(tmp.into(), io::Error::other(e)),
    };
    let mut z = zip::ZipWriter::new(w);
    let mut done = 0u64;
    let mut last_report = std::time::Instant::now();
    for it in items {
        if ctl.cancelled() {
            return Err(Error::Cancelled);
        }
        let mtime = mtime_of(&it.meta);
        let simple = SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(it.meta.mode() & 0o7777)
            .last_modified_time(to_dos_time(mtime))
            .large_file(it.meta.len() >= 0xFFFF_FFFF);
        let options = match u32::try_from(mtime) {
            Ok(t) => simple
                .with_extended_timestamp(Some(t), None, None)
                .unwrap_or_else(|_| simple.into_full_options()),
            Err(_) => simple.into_full_options(),
        };
        if it.meta.is_dir() {
            z.add_directory(format!("{}/", it.name), options)
                .map_err(|e| zip_err(ctl, e))?;
        } else if it.meta.file_type().is_symlink() {
            let target = fs::read_link(&it.path).map_err(|e| Error::Io(it.path.clone(), e))?;
            z.add_symlink(&it.name, target.to_string_lossy(), options)
                .map_err(|e| zip_err(ctl, e))?;
        } else {
            let f = fs::File::open(&it.path).map_err(|e| Error::Io(it.path.clone(), e))?;
            z.start_file(&it.name, options)
                .map_err(|e| zip_err(ctl, e))?;
            let mut feed = Feed {
                inner: f.take(it.meta.len()),
                done: &mut done,
                total,
                name: &it.name,
                ctl: &mut *ctl,
                last_report: &mut last_report,
            };
            io::copy(&mut feed, &mut z).map_err(|e| write_err(ctl, tmp, e))?;
        }
        ctl.progress(done, total, &it.name);
    }
    z.finish().map_err(|e| zip_err(ctl, e))
}

/// Creates an archive of `sources` at `target` (a free name next to it
/// when `target` exists by the end) and returns its path. Work goes in
/// `work` (see the module documentation).
pub fn create(
    sources: &[PathBuf],
    target: &Path,
    format: Format,
    work: Option<&Path>,
    ctl: &mut dyn Control,
) -> Result<PathBuf, Error> {
    let dir = target.parent().unwrap_or(Path::new("/"));
    let name = target.file_name().unwrap_or_default();
    let tmp = hidden_new(&work_dir(work, dir), "archive", |p| {
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(p)
            .map(drop)
    })?;
    let result = (|| {
        let items = collect(sources, &tmp, ctl)?;
        let total = items
            .iter()
            .filter(|i| i.meta.is_file())
            .map(|i| i.meta.len())
            .sum();
        ctl.progress(0, total, "");
        let file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| Error::Io(tmp.clone(), e))?;
        let w = BufWriter::new(file);
        let w = match format {
            Format::Zip => write_zip(w, &items, total, &tmp, ctl)?,
            _ => {
                let enc = Encoder::new(format, w).map_err(|e| Error::Io(tmp.clone(), e))?;
                let enc = write_tar(enc, &items, total, &tmp, ctl)?;
                enc.finish().map_err(|e| write_err(ctl, &tmp, e))?
            }
        };
        w.into_inner()
            .map_err(|e| Error::Io(tmp.clone(), e.into_error()))?
            .sync_all()
            .map_err(|e| Error::Io(tmp.clone(), e))?;
        if ctl.cancelled() {
            return Err(Error::Cancelled);
        }
        rename_free(&tmp, dir, name)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests;
