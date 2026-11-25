//! File operations executed on a worker thread.
//!
//! Everything here uses blocking GIO calls and must not run on the GTK main
//! thread. The UI supplies a [`Handler`] that receives progress and answers
//! conflict questions (blocking the worker until the user decides).

use gtk::gio;
use gtk::gio::prelude::*;
use gtk::glib;

use crate::archive;
use crate::naming;
use helix_app_common::i18n::tr;
use helix_app_common::trf;
use helix_app_common::trnf;

#[derive(Debug, Clone)]
pub enum Job {
    Copy {
        sources: Vec<gio::File>,
        dest: gio::File,
    },
    Move {
        sources: Vec<gio::File>,
        dest: gio::File,
    },
    Link {
        sources: Vec<gio::File>,
        dest: gio::File,
    },
    Trash(Vec<gio::File>),
    Delete(Vec<gio::File>),
    /// Restore items living in `trash:///`.
    Restore(Vec<gio::File>),
    /// Undo of a trash operation: restore the most recently trashed items
    /// whose original location is one of these.
    Untrash(Vec<gio::File>),
    EmptyTrash,
    /// Archive `sources` into the archive file `dest`.
    Compress {
        sources: Vec<gio::File>,
        dest: gio::File,
        format: archive::Format,
    },
    /// Extract archives into `dest`, or next to each archive when `None`.
    Extract {
        archives: Vec<gio::File>,
        dest: Option<gio::File>,
    },
}

impl Job {
    /// Short description for the progress popover.
    pub fn title(&self) -> String {
        match self {
            Job::Copy { sources, dest } => trnf!(
                "Copying {n} item to “{folder}”",
                "Copying {n} items to “{folder}”",
                sources.len(),
                folder = display_name(dest)
            ),
            Job::Move { sources, dest } => trnf!(
                "Moving {n} item to “{folder}”",
                "Moving {n} items to “{folder}”",
                sources.len(),
                folder = display_name(dest)
            ),
            Job::Link { sources, .. } => trnf!(
                "Creating links to {n} item",
                "Creating links to {n} items",
                sources.len()
            ),
            Job::Trash(v) => trnf!(
                "Moving {n} item to Trash",
                "Moving {n} items to Trash",
                v.len()
            ),
            Job::Delete(v) => trnf!("Deleting {n} item", "Deleting {n} items", v.len()),
            Job::Restore(v) | Job::Untrash(v) => {
                trnf!("Restoring {n} item", "Restoring {n} items", v.len())
            }
            Job::EmptyTrash => tr("Emptying Trash").into(),
            Job::Compress { dest, .. } => {
                trf!("Creating “{archive}”", archive = display_name(dest))
            }
            Job::Extract { archives, .. } => match archives.as_slice() {
                [one] => trf!("Extracting “{archive}”", archive = display_name(one)),
                v => trnf!("Extracting {n} archive", "Extracting {n} archives", v.len()),
            },
        }
    }
}

impl Job {
    /// What a toast says when the job finished without errors, given how
    /// many top-level items it produced (`None`: the caller reports it
    /// itself, as for Move to Trash).
    pub fn finished(&self, created: usize) -> Option<String> {
        let count = |v: &Vec<gio::File>| if created > 0 { created } else { v.len() };
        Some(match self {
            Job::Copy { sources, dest } => trnf!(
                "Copied {n} item to “{folder}”",
                "Copied {n} items to “{folder}”",
                count(sources),
                folder = display_name(dest)
            ),
            Job::Move { sources, dest } => trnf!(
                "Moved {n} item to “{folder}”",
                "Moved {n} items to “{folder}”",
                count(sources),
                folder = display_name(dest)
            ),
            Job::Link { sources, .. } => {
                trnf!("Created {n} link", "Created {n} links", count(sources))
            }
            Job::Restore(v) | Job::Untrash(v) => {
                trnf!("Restored {n} item", "Restored {n} items", count(v))
            }
            Job::Delete(v) => trnf!("Deleted {n} item", "Deleted {n} items", count(v)),
            Job::EmptyTrash => tr("Trash emptied").into(),
            Job::Trash(_) => return None,
            Job::Compress { dest, .. } => {
                trf!("Created “{archive}”", archive = display_name(dest))
            }
            Job::Extract { archives, .. } => trnf!(
                "Extracted {n} archive",
                "Extracted {n} archives",
                count(archives)
            ),
        })
    }
}

/// Best-effort display name of a file without blocking I/O.
pub fn display_name(f: &gio::File) -> String {
    match f.path() {
        Some(p) if p == std::path::Path::new("/") => "/".into(),
        Some(p) => p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        None => {
            let uri = f.uri().to_string();
            let label = crate::bookmarks::default_label(&uri);
            if uri == "trash:///" {
                tr("Trash").into()
            } else {
                label
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Skip,
    KeepBoth,
    /// Replace the destination; when both are folders, merge them.
    Replace,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution {
    pub choice: Choice,
    pub apply_to_all: bool,
}

#[derive(Debug, Clone)]
pub struct Conflict {
    pub source: gio::File,
    pub dest: gio::File,
    pub source_is_dir: bool,
    pub dest_is_dir: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Progress {
    pub done_items: u64,
    pub total_items: u64,
    pub done_bytes: u64,
    pub total_bytes: u64,
    pub current: String,
}

impl Progress {
    pub fn fraction(&self) -> f64 {
        let f = if self.total_bytes > 0 {
            self.done_bytes as f64 / self.total_bytes as f64
        } else if self.total_items > 0 {
            self.done_items as f64 / self.total_items as f64
        } else {
            0.0
        };
        f.clamp(0.0, 1.0)
    }
}

pub trait Handler {
    fn progress(&mut self, p: &Progress);
    fn conflict(&mut self, c: &Conflict) -> Resolution;
}

#[derive(Debug, Default, Clone)]
pub struct Outcome {
    pub errors: Vec<String>,
    pub cancelled: bool,
    /// Top-level results (new copies, moved items, links, restored items).
    pub created: Vec<gio::File>,
    /// Original locations of successfully trashed items (for undo).
    pub trashed: Vec<gio::File>,
    /// Items that could not be trashed because their filesystem has no
    /// usable trash (offered for permanent deletion instead).
    pub untrashable: Vec<gio::File>,
    /// Moved items: (from, to).
    pub moved: Vec<(gio::File, gio::File)>,
}

const NOFOLLOW: gio::FileQueryInfoFlags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;

struct Runner<'a> {
    cancel: &'a gio::Cancellable,
    handler: &'a mut dyn Handler,
    progress: Progress,
    remembered: Option<Choice>,
    out: Outcome,
    last_report: std::time::Instant,
}

fn is_cancelled(e: &glib::Error) -> bool {
    e.matches(gio::IOErrorEnum::Cancelled)
}

/// Type of `f` without following symlinks (`Unknown` if it doesn't exist).
fn file_type(f: &gio::File, cancel: &gio::Cancellable) -> gio::FileType {
    f.query_file_type(NOFOLLOW, Some(cancel))
}

fn exists(f: &gio::File, cancel: &gio::Cancellable) -> bool {
    file_type(f, cancel) != gio::FileType::Unknown
}

/// Counts items and bytes under `f` (not following symlinks). `tick` is
/// called periodically with running totals and may return false to stop.
pub fn measure(
    f: &gio::File,
    cancel: &gio::Cancellable,
    tick: &mut dyn FnMut(u64, u64) -> bool,
) -> (u64, u64) {
    let mut items = 0u64;
    let mut bytes = 0u64;
    let mut stack = vec![f.clone()];
    while let Some(cur) = stack.pop() {
        if cancel.is_cancelled() {
            break;
        }
        let Ok(info) = cur.query_info("standard::type,standard::size", NOFOLLOW, Some(cancel))
        else {
            continue;
        };
        items += 1;
        if info.file_type() != gio::FileType::Directory {
            bytes += info.size().max(0) as u64;
        }
        if info.file_type() == gio::FileType::Directory {
            if let Ok(en) = cur.enumerate_children("standard::name", NOFOLLOW, Some(cancel)) {
                while let Ok(Some(child)) = en.next_file(Some(cancel)) {
                    stack.push(cur.child(child.name()));
                }
            }
        }
        if items.is_multiple_of(200) && !tick(items, bytes) {
            break;
        }
    }
    (items, bytes)
}

impl Runner<'_> {
    fn report(&mut self, force: bool) {
        if force || self.last_report.elapsed() > std::time::Duration::from_millis(80) {
            self.last_report = std::time::Instant::now();
            self.handler.progress(&self.progress);
        }
    }

    /// Records that `verb` (one of the keys matched below) failed on `f`.
    fn error(&mut self, verb: &str, f: &gio::File, e: &glib::Error) {
        if is_cancelled(e) {
            self.out.cancelled = true;
        } else {
            let file = display_name(f);
            let error = helix_app_common::errors::glib_detail(e);
            self.out.errors.push(match verb {
                "replace" => trf!(
                    "Could not replace “{file}”: {error}",
                    file = file,
                    error = error
                ),
                "move" => trf!(
                    "Could not move “{file}”: {error}",
                    file = file,
                    error = error
                ),
                "read" => trf!(
                    "Could not read “{file}”: {error}",
                    file = file,
                    error = error
                ),
                "remove" => trf!(
                    "Could not remove “{file}”: {error}",
                    file = file,
                    error = error
                ),
                "link to" => trf!(
                    "Could not link to “{file}”: {error}",
                    file = file,
                    error = error
                ),
                "move to the Trash" => trf!(
                    "Could not move “{file}” to the Trash: {error}",
                    file = file,
                    error = error
                ),
                "delete" => trf!(
                    "Could not delete “{file}”: {error}",
                    file = file,
                    error = error
                ),
                "restore" => trf!(
                    "Could not restore “{file}”: {error}",
                    file = file,
                    error = error
                ),
                "empty" => trf!(
                    "Could not empty “{file}”: {error}",
                    file = file,
                    error = error
                ),
                _ => trf!(
                    "Could not process “{file}”: {error}",
                    file = file,
                    error = error
                ),
            });
        }
    }

    fn stopped(&mut self) -> bool {
        if self.cancel.is_cancelled() {
            self.out.cancelled = true;
        }
        self.out.cancelled
    }

    fn scan(&mut self, sources: &[gio::File]) {
        let cancel = self.cancel;
        for s in sources {
            let (i, b) = measure(s, cancel, &mut |_, _| true);
            self.progress.total_items += i;
            self.progress.total_bytes += b;
        }
    }

    fn resolve(&mut self, c: &Conflict) -> Choice {
        if let Some(choice) = self.remembered {
            return choice;
        }
        let r = self.handler.conflict(c);
        if r.apply_to_all {
            self.remembered = Some(r.choice);
        }
        if r.choice == Choice::Cancel {
            self.cancel.cancel();
            self.out.cancelled = true;
        }
        r.choice
    }

    fn free_copy_name(&self, dir: &gio::File, name: &str) -> gio::File {
        let cancel = self.cancel;
        dir.child(naming::copy_name(name, |n| exists(&dir.child(n), cancel)))
    }

    fn delete_recursive(&mut self, f: &gio::File) -> Result<(), glib::Error> {
        if file_type(f, self.cancel) == gio::FileType::Directory {
            let en = f.enumerate_children("standard::name", NOFOLLOW, Some(self.cancel))?;
            let mut children = Vec::new();
            while let Some(info) = en.next_file(Some(self.cancel))? {
                children.push(f.child(info.name()));
            }
            drop(en);
            for c in children {
                if self.stopped() {
                    return Err(glib::Error::new(
                        gio::IOErrorEnum::Cancelled,
                        tr("Cancelled"),
                    ));
                }
                self.delete_recursive(&c)?;
            }
        }
        f.delete(Some(self.cancel))?;
        self.progress.done_items += 1;
        self.report(false);
        Ok(())
    }

    /// Copies (or moves) `src` to `target`, handling conflicts. Returns the
    /// final destination when something was transferred.
    fn transfer(&mut self, src: &gio::File, target: gio::File, is_move: bool) -> Option<gio::File> {
        if self.stopped() {
            return None;
        }
        let src_type = file_type(src, self.cancel);
        if src_type == gio::FileType::Unknown {
            self.out
                .errors
                .push(trf!("“{}” no longer exists", display_name(src)));
            return None;
        }
        let src_is_dir = src_type == gio::FileType::Directory;
        if src_is_dir && (target.equal(src) || target.has_prefix(src)) {
            self.out.errors.push(if is_move {
                trf!("Cannot move “{}” into itself", display_name(src))
            } else {
                trf!("Cannot copy “{}” into itself", display_name(src))
            });
            return None;
        }
        let mut target = target;
        let dest_type = file_type(&target, self.cancel);
        let mut merge = false;
        if dest_type != gio::FileType::Unknown {
            if target.equal(src) {
                // Moving onto itself: nothing to do.
                return None;
            }
            let dest_is_dir = dest_type == gio::FileType::Directory;
            let conflict = Conflict {
                source: src.clone(),
                dest: target.clone(),
                source_is_dir: src_is_dir,
                dest_is_dir,
            };
            match self.resolve(&conflict) {
                Choice::Skip => {
                    let (i, b) = measure(src, self.cancel, &mut |_, _| true);
                    self.progress.done_items += i;
                    self.progress.done_bytes += b;
                    return None;
                }
                Choice::Cancel => return None,
                Choice::KeepBoth => {
                    let parent = target.parent()?;
                    let name = target.basename()?.to_string_lossy().into_owned();
                    target = self.free_copy_name(&parent, &name);
                }
                Choice::Replace => {
                    if src_is_dir && dest_is_dir {
                        merge = true;
                    } else if let Err(e) = self.delete_recursive(&target) {
                        self.error("replace", &target, &e);
                        return None;
                    }
                }
            }
        }

        if is_move && !merge {
            match src.move_(
                &target,
                gio::FileCopyFlags::NOFOLLOW_SYMLINKS,
                Some(self.cancel),
                None,
            ) {
                Ok(()) => {
                    // Account for the whole subtree at once.
                    let (i, b) = measure(&target, self.cancel, &mut |_, _| true);
                    self.progress.done_items += i;
                    self.progress.done_bytes += b;
                    self.progress.current = display_name(src);
                    self.report(false);
                    return Some(target);
                }
                Err(e)
                    if e.matches(gio::IOErrorEnum::WouldRecurse)
                        || e.matches(gio::IOErrorEnum::NotSupported) => {}
                Err(e) => {
                    self.error("move", src, &e);
                    return None;
                }
            }
        }

        self.progress.current = display_name(src);
        if src_is_dir {
            if !merge {
                if let Err(e) = target.make_directory(Some(self.cancel)) {
                    self.error(if is_move { "move" } else { "copy" }, src, &e);
                    return None;
                }
                // Keep permissions/timestamps where supported.
                if let Ok(info) =
                    src.query_info("unix::mode,time::modified", NOFOLLOW, Some(self.cancel))
                {
                    let _ = target.set_attributes_from_info(&info, NOFOLLOW, Some(self.cancel));
                }
            }
            self.progress.done_items += 1;
            let children =
                match src.enumerate_children("standard::name", NOFOLLOW, Some(self.cancel)) {
                    Ok(en) => {
                        let mut v = Vec::new();
                        while let Ok(Some(info)) = en.next_file(Some(self.cancel)) {
                            v.push(info.name());
                        }
                        v
                    }
                    Err(e) => {
                        self.error("read", src, &e);
                        return Some(target);
                    }
                };
            let mut all_ok = true;
            for name in children {
                if self.stopped() {
                    return Some(target);
                }
                let child_src = src.child(&name);
                let child_dst = target.child(&name);
                let errors_before = self.out.errors.len();
                let moved = self.transfer(&child_src, child_dst, is_move);
                if moved.is_none() || self.out.errors.len() != errors_before {
                    all_ok = false;
                }
            }
            if is_move && all_ok && !self.stopped() {
                if let Err(e) = src.delete(Some(self.cancel)) {
                    self.error("remove", src, &e);
                }
            }
            Some(target)
        } else {
            let base_done = self.progress.done_bytes;
            let result = {
                let progress = &mut self.progress;
                let handler = &mut *self.handler;
                let last = &mut self.last_report;
                let mut cb = |cur: i64, _total: i64| {
                    progress.done_bytes = base_done + cur.max(0) as u64;
                    if last.elapsed() > std::time::Duration::from_millis(80) {
                        *last = std::time::Instant::now();
                        handler.progress(progress);
                    }
                };
                let flags =
                    gio::FileCopyFlags::NOFOLLOW_SYMLINKS | gio::FileCopyFlags::ALL_METADATA;
                if is_move {
                    src.move_(&target, flags, Some(self.cancel), Some(&mut cb))
                } else {
                    src.copy(&target, flags, Some(self.cancel), Some(&mut cb))
                }
            };
            match result {
                Ok(()) => {
                    let size = target
                        .query_info("standard::size", NOFOLLOW, Some(self.cancel))
                        .map(|i| i.size().max(0) as u64)
                        .unwrap_or(0);
                    self.progress.done_bytes = base_done + size;
                    self.progress.done_items += 1;
                    self.report(false);
                    Some(target)
                }
                Err(e) => {
                    self.progress.done_bytes = base_done;
                    self.error(if is_move { "move" } else { "copy" }, src, &e);
                    None
                }
            }
        }
    }

    fn copy_or_move(&mut self, sources: &[gio::File], dest: &gio::File, is_move: bool) {
        self.scan(sources);
        self.report(true);
        for src in sources {
            if self.stopped() {
                break;
            }
            let Some(name) = src.basename() else { continue };
            let same_dir = src.parent().is_some_and(|p| p.equal(dest));
            let target = if same_dir {
                if is_move {
                    continue; // Moving into the folder it is already in.
                }
                self.free_copy_name(dest, &name.to_string_lossy())
            } else {
                dest.child(&name)
            };
            if let Some(done) = self.transfer(src, target, is_move) {
                if is_move {
                    self.out.moved.push((src.clone(), done.clone()));
                }
                self.out.created.push(done);
            }
        }
    }

    fn link(&mut self, sources: &[gio::File], dest: &gio::File) {
        for src in sources {
            if self.stopped() {
                break;
            }
            let Some(path) = src.path() else {
                self.out.errors.push(trf!(
                    "Cannot create a link to “{}”: not a local file",
                    display_name(src)
                ));
                continue;
            };
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let cancel = self.cancel;
            let link = dest.child(naming::link_name(&name, |n| exists(&dest.child(n), cancel)));
            match link.make_symbolic_link(&path, Some(self.cancel)) {
                Ok(()) => self.out.created.push(link),
                Err(e) => self.error("link to", src, &e),
            }
        }
    }

    fn trash(&mut self, files: &[gio::File]) {
        self.progress.total_items = files.len() as u64;
        for f in files {
            if self.stopped() {
                break;
            }
            self.progress.current = display_name(f);
            self.report(false);
            match f.trash(Some(self.cancel)) {
                Ok(()) => self.out.trashed.push(f.clone()),
                // No trash on that filesystem (none, or one GIO will not
                // use, e.g. not owned by the user on NTFS or FAT).
                Err(e) if e.matches(gio::IOErrorEnum::NotSupported) => {
                    self.out.untrashable.push(f.clone())
                }
                Err(e) => self.error("move to the Trash", f, &e),
            }
            self.progress.done_items += 1;
        }
    }

    fn delete(&mut self, files: &[gio::File]) {
        for f in files {
            // Items in trash:/// can only be deleted at top level, which the
            // backend does recursively.
            if f.uri_scheme().as_deref() != Some("trash") {
                self.scan(std::slice::from_ref(f));
            } else {
                self.progress.total_items += 1;
            }
        }
        for f in files {
            if self.stopped() {
                break;
            }
            self.progress.current = display_name(f);
            let r = if f.uri_scheme().as_deref() == Some("trash") {
                f.delete(Some(self.cancel))
                    .inspect(|_| self.progress.done_items += 1)
            } else {
                self.delete_recursive(f)
            };
            if let Err(e) = r {
                self.error("delete", f, &e);
            }
        }
    }

    fn restore_one(&mut self, item: &gio::File) {
        let info = match item.query_info(
            "trash::orig-path,standard::name",
            gio::FileQueryInfoFlags::NONE,
            Some(self.cancel),
        ) {
            Ok(i) => i,
            Err(e) => return self.error("restore", item, &e),
        };
        let Some(orig) = info.attribute_byte_string("trash::orig-path") else {
            self.out
                .errors
                .push(trf!("“{}” has no original location", display_name(item)));
            return;
        };
        let orig = gio::File::for_path(orig.as_str());
        if let Some(parent) = orig.parent() {
            if !exists(&parent, self.cancel) {
                if let Err(e) = parent.make_directory_with_parents(Some(self.cancel)) {
                    return self.error("restore", item, &e);
                }
            }
        }
        if let Some(done) = self.transfer(item, orig, true) {
            self.out.created.push(done);
        }
    }

    fn restore(&mut self, items: &[gio::File]) {
        self.progress.total_items = items.len() as u64;
        for item in items {
            if self.stopped() {
                break;
            }
            self.restore_one(item);
        }
    }

    fn untrash(&mut self, originals: &[gio::File]) {
        let trash = gio::File::for_uri("trash:///");
        let en = match trash.enumerate_children(
            "standard::name,trash::orig-path,trash::deletion-date",
            gio::FileQueryInfoFlags::NONE,
            Some(self.cancel),
        ) {
            Ok(en) => en,
            Err(e) => return self.error("read", &trash, &e),
        };
        let mut candidates: Vec<(String, String, gio::File)> = Vec::new();
        while let Ok(Some(info)) = en.next_file(Some(self.cancel)) {
            let Some(orig) = info.attribute_byte_string("trash::orig-path") else {
                continue;
            };
            let date = info
                .attribute_string("trash::deletion-date")
                .map(|s| s.to_string())
                .unwrap_or_default();
            candidates.push((orig.to_string(), date, trash.child(info.name())));
        }
        let items: Vec<gio::File> = originals
            .iter()
            .filter_map(|o| {
                let path = o.path()?.to_string_lossy().into_owned();
                candidates
                    .iter()
                    .filter(|(p, _, _)| *p == path)
                    .max_by(|a, b| a.1.cmp(&b.1))
                    .map(|c| c.2.clone())
            })
            .collect();
        if items.len() < originals.len() {
            self.out.errors.push(trnf!(
                "{n} item could not be found in the Trash",
                "{n} items could not be found in the Trash",
                originals.len() - items.len()
            ));
        }
        self.restore(&items);
    }

    fn empty_trash(&mut self) {
        let trash = gio::File::for_uri("trash:///");
        let en = match trash.enumerate_children(
            "standard::name",
            gio::FileQueryInfoFlags::NONE,
            Some(self.cancel),
        ) {
            Ok(en) => en,
            Err(e) => return self.error("empty", &trash, &e),
        };
        let mut items = Vec::new();
        while let Ok(Some(info)) = en.next_file(Some(self.cancel)) {
            items.push(trash.child(info.name()));
        }
        self.progress.total_items = items.len() as u64;
        for item in items {
            if self.stopped() {
                break;
            }
            if let Err(e) = item.delete(Some(self.cancel)) {
                self.error("delete", &item, &e);
            }
            self.progress.done_items += 1;
            self.report(false);
        }
    }
}

impl Runner<'_> {
    /// Local paths of `files`, or `None` after reporting the first one
    /// that has none.
    fn local_paths(&mut self, files: &[gio::File]) -> Option<Vec<std::path::PathBuf>> {
        let mut paths = Vec::new();
        for f in files {
            match f.path() {
                Some(p) => paths.push(p),
                None => {
                    self.out.errors.push(trf!(
                        "“{file}” is not on this computer, so it can’t be archived or extracted",
                        file = display_name(f)
                    ));
                    return None;
                }
            }
        }
        Some(paths)
    }

    fn compress(&mut self, sources: &[gio::File], dest: &gio::File, format: archive::Format) {
        let Some(paths) = self.local_paths(sources) else {
            return;
        };
        let Some(target) = self.local_paths(std::slice::from_ref(dest)) else {
            return;
        };
        self.progress.total_items = 1;
        let mut ctl = ArchiveControl {
            runner: self,
            base: 0,
            size: None,
        };
        let work = archive_work_dir();
        match archive::create(&paths, &target[0], format, Some(&work), &mut ctl) {
            Ok(made) => {
                self.out.created.push(gio::File::for_path(made));
                self.progress.done_items = 1;
            }
            Err(archive::Error::Cancelled) => self.out.cancelled = true,
            Err(e) => self.out.errors.push(trf!(
                "Could not create “{file}”: {error}",
                file = display_name(dest),
                error = e.message()
            )),
        }
    }

    fn extract(&mut self, archives: &[gio::File], dest: Option<&gio::File>) {
        let Some(paths) = self.local_paths(archives) else {
            return;
        };
        let dest = match dest {
            Some(d) => match self.local_paths(std::slice::from_ref(d)) {
                Some(mut p) => p.pop(),
                None => return,
            },
            None => None,
        };
        let sizes: Vec<u64> = paths
            .iter()
            .map(|p| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0))
            .collect();
        self.progress.total_items = paths.len() as u64;
        self.progress.total_bytes = sizes.iter().sum();
        let work = archive_work_dir();
        let mut base = 0;
        for ((path, file), size) in paths.iter().zip(archives).zip(sizes) {
            if self.stopped() {
                break;
            }
            let folder = match &dest {
                Some(d) => d.clone(),
                None => path.parent().map(|p| p.to_path_buf()).unwrap_or_default(),
            };
            let mut ctl = ArchiveControl {
                runner: self,
                base,
                size: Some(size),
            };
            match archive::extract(path, &folder, Some(&work), &mut ctl) {
                Ok(made) => self.out.created.push(gio::File::for_path(made)),
                Err(archive::Error::Cancelled) => self.out.cancelled = true,
                Err(e) => self.out.errors.push(trf!(
                    "Could not extract “{file}”: {error}",
                    file = display_name(file),
                    error = e.message()
                )),
            }
            base += size;
            self.progress.done_items += 1;
            self.progress.done_bytes = base;
            self.report(false);
        }
    }
}

/// Where archives are made and extracted before they are moved into
/// place (see `archive`): a folder in the user's cache.
pub(crate) fn archive_work_dir() -> std::path::PathBuf {
    #[cfg(not(test))]
    let base = glib::user_cache_dir();
    // Tests never write to the real cache.
    #[cfg(test)]
    let base = std::env::temp_dir().join(format!("helix-files-test-cache-{}", std::process::id()));
    base.join("helix-files").join("work")
}

/// Lets an archive operation see the job's cancellation and report its
/// progress. Extraction reports its own scale; it is mapped onto the
/// archive's `size` bytes after `base` of the whole job.
struct ArchiveControl<'r, 'a> {
    runner: &'r mut Runner<'a>,
    base: u64,
    size: Option<u64>,
}

impl archive::Control for ArchiveControl<'_, '_> {
    fn cancelled(&self) -> bool {
        self.runner.cancel.is_cancelled()
    }

    fn progress(&mut self, done: u64, total: u64, current: &str) {
        let p = &mut self.runner.progress;
        match self.size {
            Some(size) => {
                let part = if total > 0 {
                    (done.min(total) as u128 * size as u128 / total as u128) as u64
                } else {
                    0
                };
                p.done_bytes = self.base + part;
            }
            None => {
                p.total_bytes = total;
                p.done_bytes = done;
            }
        }
        p.current = current.to_string();
        self.runner.report(false);
    }
}

/// Runs `job` to completion (blocking).
pub fn run(job: &Job, cancel: &gio::Cancellable, handler: &mut dyn Handler) -> Outcome {
    let mut r = Runner {
        cancel,
        handler,
        progress: Progress::default(),
        remembered: None,
        out: Outcome::default(),
        last_report: std::time::Instant::now(),
    };
    match job {
        Job::Copy { sources, dest } => r.copy_or_move(sources, dest, false),
        Job::Move { sources, dest } => r.copy_or_move(sources, dest, true),
        Job::Link { sources, dest } => r.link(sources, dest),
        Job::Trash(files) => r.trash(files),
        Job::Delete(files) => r.delete(files),
        Job::Restore(items) => r.restore(items),
        Job::Untrash(orig) => r.untrash(orig),
        Job::EmptyTrash => r.empty_trash(),
        Job::Compress {
            sources,
            dest,
            format,
        } => r.compress(sources, dest, *format),
        Job::Extract { archives, dest } => r.extract(archives, dest.as_ref()),
    }
    r.progress.done_items = r.progress.total_items.max(r.progress.done_items);
    r.report(true);
    if cancel.is_cancelled() {
        r.out.cancelled = true;
    }
    r.out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    #[test]
    fn finished_messages() {
        let f = |p: &str| gio::File::for_path(p);
        let copy = Job::Copy {
            sources: vec![f("/a"), f("/b"), f("/c")],
            dest: f("/home/me/Documents"),
        };
        assert_eq!(
            copy.finished(0).as_deref(),
            Some("Copied 3 items to “Documents”")
        );
        // Skipped conflicts: count what was made.
        assert_eq!(
            copy.finished(2).as_deref(),
            Some("Copied 2 items to “Documents”")
        );
        let mv = Job::Move {
            sources: vec![f("/a")],
            dest: f("/tmp"),
        };
        assert_eq!(mv.finished(1).as_deref(), Some("Moved 1 item to “tmp”"));
        let link = Job::Link {
            sources: vec![f("/a")],
            dest: f("/"),
        };
        assert_eq!(link.finished(1).as_deref(), Some("Created 1 link"));
        assert_eq!(
            Job::Restore(vec![f("/a"), f("/b")]).finished(2).as_deref(),
            Some("Restored 2 items")
        );
        assert_eq!(Job::Trash(vec![f("/a")]).finished(0), None);
    }

    /// A unique scratch directory under the system temp dir, removed on drop.
    pub struct TempDir(pub PathBuf);
    impl TempDir {
        pub fn new(tag: &str) -> Self {
            static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let p = std::env::temp_dir()
                .join(format!("helix-files-test-{}-{tag}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
        pub fn path(&self, rel: &str) -> PathBuf {
            self.0.join(rel)
        }
        pub fn file(&self, rel: &str) -> gio::File {
            gio::File::for_path(self.path(rel))
        }
        pub fn write(&self, rel: &str, content: &str) {
            let p = self.path(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, content).unwrap();
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            assert!(self.0.starts_with(std::env::temp_dir()));
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    struct Canned {
        answer: Resolution,
        asked: usize,
        last_progress: Progress,
    }
    impl Canned {
        fn new(choice: Choice, all: bool) -> Self {
            Canned {
                answer: Resolution {
                    choice,
                    apply_to_all: all,
                },
                asked: 0,
                last_progress: Progress::default(),
            }
        }
    }
    impl Handler for Canned {
        fn progress(&mut self, p: &Progress) {
            self.last_progress = p.clone();
        }
        fn conflict(&mut self, _: &Conflict) -> Resolution {
            self.asked += 1;
            self.answer
        }
    }

    fn read(p: &Path) -> String {
        fs::read_to_string(p).unwrap()
    }

    fn go(job: Job, h: &mut Canned) -> Outcome {
        run(&job, &gio::Cancellable::new(), h)
    }

    #[test]
    fn copy_tree() {
        let t = TempDir::new("copy");
        t.write("src/a.txt", "A");
        t.write("src/sub/b.txt", "BB");
        fs::create_dir_all(t.path("dst")).unwrap();
        let mut h = Canned::new(Choice::Skip, false);
        let out = go(
            Job::Copy {
                sources: vec![t.file("src")],
                dest: t.file("dst"),
            },
            &mut h,
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(read(&t.path("dst/src/a.txt")), "A");
        assert_eq!(read(&t.path("dst/src/sub/b.txt")), "BB");
        assert!(t.path("src/a.txt").exists());
        assert_eq!(out.created.len(), 1);
        assert_eq!(h.last_progress.total_bytes, h.last_progress.done_bytes);
        assert_eq!(h.last_progress.fraction(), 1.0);
    }

    #[test]
    fn copy_into_same_folder_makes_copy_names() {
        let t = TempDir::new("dup");
        t.write("d/file.txt", "x");
        let mut h = Canned::new(Choice::Cancel, false);
        go(
            Job::Copy {
                sources: vec![t.file("d/file.txt")],
                dest: t.file("d"),
            },
            &mut h,
        );
        go(
            Job::Copy {
                sources: vec![t.file("d/file.txt")],
                dest: t.file("d"),
            },
            &mut h,
        );
        assert!(t.path("d/file (copy).txt").exists());
        assert!(t.path("d/file (copy 2).txt").exists());
        assert_eq!(h.asked, 0);
    }

    #[test]
    fn conflicts() {
        let t = TempDir::new("conflict");
        t.write("a/x.txt", "new");
        t.write("a/y.txt", "new");
        t.write("b/x.txt", "old");
        t.write("b/y.txt", "old");
        let srcs = vec![t.file("a/x.txt"), t.file("a/y.txt")];

        let mut skip = Canned::new(Choice::Skip, true);
        go(
            Job::Copy {
                sources: srcs.clone(),
                dest: t.file("b"),
            },
            &mut skip,
        );
        assert_eq!(skip.asked, 1, "apply-to-all should ask once");
        assert_eq!(read(&t.path("b/x.txt")), "old");

        let mut both = Canned::new(Choice::KeepBoth, false);
        go(
            Job::Copy {
                sources: srcs.clone(),
                dest: t.file("b"),
            },
            &mut both,
        );
        assert_eq!(both.asked, 2);
        assert_eq!(read(&t.path("b/x (copy).txt")), "new");
        assert_eq!(read(&t.path("b/x.txt")), "old");

        let mut replace = Canned::new(Choice::Replace, true);
        go(
            Job::Copy {
                sources: srcs,
                dest: t.file("b"),
            },
            &mut replace,
        );
        assert_eq!(read(&t.path("b/x.txt")), "new");
        assert_eq!(read(&t.path("b/y.txt")), "new");
    }

    #[test]
    fn cancel_stops() {
        let t = TempDir::new("cancel");
        t.write("a/x.txt", "new");
        t.write("a/y.txt", "new");
        t.write("b/x.txt", "old");
        let mut h = Canned::new(Choice::Cancel, false);
        let out = go(
            Job::Copy {
                sources: vec![t.file("a/x.txt"), t.file("a/y.txt")],
                dest: t.file("b"),
            },
            &mut h,
        );
        assert!(out.cancelled);
        assert!(!t.path("b/y.txt").exists());
    }

    #[test]
    fn merge_folders_on_move() {
        let t = TempDir::new("merge");
        t.write("a/dir/one.txt", "1");
        t.write("a/dir/sub/two.txt", "2");
        t.write("b/dir/zero.txt", "0");
        let mut h = Canned::new(Choice::Replace, true);
        let out = go(
            Job::Move {
                sources: vec![t.file("a/dir")],
                dest: t.file("b"),
            },
            &mut h,
        );
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(read(&t.path("b/dir/one.txt")), "1");
        assert_eq!(read(&t.path("b/dir/sub/two.txt")), "2");
        assert_eq!(read(&t.path("b/dir/zero.txt")), "0");
        assert!(!t.path("a/dir").exists());
    }

    #[test]
    fn simple_move_and_noop_move() {
        let t = TempDir::new("move");
        t.write("a/f.txt", "f");
        fs::create_dir_all(t.path("b")).unwrap();
        let mut h = Canned::new(Choice::Cancel, false);
        go(
            Job::Move {
                sources: vec![t.file("a/f.txt")],
                dest: t.file("a"),
            },
            &mut h,
        );
        assert!(t.path("a/f.txt").exists());
        let out = go(
            Job::Move {
                sources: vec![t.file("a/f.txt")],
                dest: t.file("b"),
            },
            &mut h,
        );
        assert!(out.errors.is_empty());
        assert!(!t.path("a/f.txt").exists());
        assert_eq!(read(&t.path("b/f.txt")), "f");
        assert_eq!(h.asked, 0);
    }

    #[test]
    fn refuses_copy_into_itself() {
        let t = TempDir::new("self");
        t.write("a/sub/f", "x");
        let mut h = Canned::new(Choice::Cancel, false);
        let out = go(
            Job::Copy {
                sources: vec![t.file("a")],
                dest: t.file("a/sub"),
            },
            &mut h,
        );
        assert_eq!(out.errors.len(), 1);
        assert!(!t.path("a/sub/a").exists());
    }

    #[test]
    fn untrashable_items_are_offered_not_reported() {
        // GIO has no trash on system internal mounts such as /dev/shm: it
        // answers "not supported", as for an NTFS trash the user does not own.
        let path = PathBuf::from(format!(
            "/dev/shm/helix-files-untrashable-{}",
            std::process::id()
        ));
        if fs::write(&path, "x").is_err() {
            return;
        }
        let mut h = Canned::new(Choice::Cancel, false);
        let out = go(Job::Trash(vec![gio::File::for_path(&path)]), &mut h);
        let kept = path.exists();
        let _ = fs::remove_file(&path);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(out.trashed.is_empty());
        assert_eq!(out.untrashable.len(), 1);
        assert!(kept, "left in place for the user to decide");
    }

    #[test]
    fn delete_tree() {
        let t = TempDir::new("delete");
        t.write("victim/a/b/c.txt", "c");
        t.write("victim/d.txt", "d");
        t.write("keep.txt", "k");
        let mut h = Canned::new(Choice::Cancel, false);
        let out = go(Job::Delete(vec![t.file("victim")]), &mut h);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert!(!t.path("victim").exists());
        assert!(t.path("keep.txt").exists());
    }

    #[test]
    fn links() {
        let t = TempDir::new("link");
        t.write("f.txt", "x");
        let mut h = Canned::new(Choice::Cancel, false);
        go(
            Job::Link {
                sources: vec![t.file("f.txt")],
                dest: t.file(""),
            },
            &mut h,
        );
        go(
            Job::Link {
                sources: vec![t.file("f.txt")],
                dest: t.file(""),
            },
            &mut h,
        );
        assert_eq!(
            fs::read_link(t.path("Link to f.txt")).unwrap(),
            t.path("f.txt")
        );
        assert!(t.path("Link to f (2).txt").exists());
    }

    #[test]
    fn measures() {
        let t = TempDir::new("measure");
        t.write("d/a", "12345");
        t.write("d/s/b", "123");
        let (items, bytes) = measure(&t.file("d"), &gio::Cancellable::new(), &mut |_, _| true);
        assert_eq!(items, 4); // d, a, s, b
        assert!(bytes >= 8);
    }

    /// Compress and Extract run as jobs: the archive is made in the
    /// folder, archives extract next to themselves or into a folder, and
    /// a cancelled job leaves nothing.
    #[test]
    fn archive_jobs() {
        let t = TempDir::new("archive");
        t.write("docs/a.txt", "alpha");
        t.write("docs/sub/b.txt", "beta");
        t.write("c.txt", "gamma");
        let mut h = Canned::new(Choice::Skip, false);
        let job = Job::Compress {
            sources: vec![t.file("docs"), t.file("c.txt")],
            dest: t.file("Archive.tar.gz"),
            format: archive::Format::TarGz,
        };
        assert_eq!(job.title(), "Creating “Archive.tar.gz”");
        let out = go(job.clone(), &mut h);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(out.created.len(), 1);
        assert!(out.created[0].equal(&t.file("Archive.tar.gz")));
        assert_eq!(job.finished(1).as_deref(), Some("Created “Archive.tar.gz”"));
        assert_eq!(h.last_progress.total_bytes, 14);
        // A single item: Extract To puts it straight into the folder.
        go(
            Job::Compress {
                sources: vec![t.file("c.txt")],
                dest: t.file("one.zip"),
                format: archive::Format::Zip,
            },
            &mut h,
        );

        fs::create_dir(t.path("out")).unwrap();
        let job = Job::Extract {
            archives: vec![t.file("Archive.tar.gz"), t.file("one.zip")],
            dest: Some(t.file("out")),
        };
        assert_eq!(job.title(), "Extracting 2 archives");
        let out = go(job.clone(), &mut h);
        assert!(out.errors.is_empty(), "{:?}", out.errors);
        assert_eq!(
            job.finished(out.created.len()).as_deref(),
            Some("Extracted 2 archives")
        );
        assert_eq!(read(&t.path("out/Archive/docs/sub/b.txt")), "beta");
        assert_eq!(read(&t.path("out/c.txt")), "gamma");
        assert_eq!(h.last_progress.fraction(), 1.0);

        // Extract Here: next to the archive, never over what is there.
        let out = go(
            Job::Extract {
                archives: vec![t.file("one.zip")],
                dest: None,
            },
            &mut h,
        );
        assert!(out.created[0].equal(&t.file("c (2).txt")));
        assert_eq!(read(&t.path("c.txt")), "gamma");

        // Errors name the archive.
        t.write("bad.zip", "not really a zip");
        let out = go(
            Job::Extract {
                archives: vec![t.file("bad.zip")],
                dest: None,
            },
            &mut h,
        );
        assert_eq!(
            out.errors,
            vec!["Could not extract “bad.zip”: This is not an archive Files can extract."]
        );

        // Cancelled before it starts: nothing is made.
        let cancel = gio::Cancellable::new();
        cancel.cancel();
        let before = fs::read_dir(&t.0).unwrap().count();
        let out = run(
            &Job::Compress {
                sources: vec![t.file("docs")],
                dest: t.file("never.zip"),
                format: archive::Format::Zip,
            },
            &cancel,
            &mut h,
        );
        assert!(out.cancelled && out.errors.is_empty());
        let out = run(
            &Job::Extract {
                archives: vec![t.file("Archive.tar.gz")],
                dest: None,
            },
            &cancel,
            &mut h,
        );
        assert!(out.cancelled && out.errors.is_empty());
        assert_eq!(fs::read_dir(&t.0).unwrap().count(), before);
        assert!(fs::read_dir(archive_work_dir()).unwrap().next().is_none());
        remove_test_cache();
    }

    /// Removes this process's stand-in for the user's cache.
    pub fn remove_test_cache() {
        let work = archive_work_dir();
        let cache = work.parent().and_then(|p| p.parent()).unwrap();
        assert!(cache.starts_with(std::env::temp_dir()));
        let _ = fs::remove_dir_all(cache);
    }
}
