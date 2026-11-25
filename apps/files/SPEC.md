# Files

Drop-in replacement for GNOME Files (Nautilus).

- **App ID**: `org.helixdesktop.Files`
- **Binary**: `helix-files`
- **Crate**: `apps/files`

## Purpose
File manager for local and remote locations.

## Main windows
- Browser window with sidebar (places, bookmarks, devices), path bar, view (icon/list/grid), and status bar.
- Multiple windows and tabs.
- Preferences dialog.

## Contracts used
- XDG portals: FileChooser (when acting as chooser), OpenURI, Trash, Background.
- GSettings: `org.gnome.nautilus.preferences`, `org.gnome.nautilus.window-state`, `org.gnome.nautilus.list-view`.
- GVFS for remote and virtual locations.
- Standard freedesktop trash and recent-files specs.
- GNOME Files' (Nautilus's) star store, `$XDG_DATA_HOME/nautilus/tags`:
  helixdesktop/spec `contracts/files/starred.md` (`src/stars.rs`, the
  same file as helix-desktop's `crates/helix-settings/src/stars.rs`).

## Non-goals
- Exact visual clone of current Nautilus.
- Extensions API compatibility.

---

## Behaviour (derived spec)

All file access goes through GIO (`GFile`), so every location GVFS can
handle (`sftp://`, `smb://`, `dav://`, `mtp://`, `trash:///`, `recent:///`,
`network:///` …) works like a local folder. Folder listings use
`GtkDirectoryList` (asynchronous, monitored for changes from the start: turning monitoring on after a folder loaded makes GTK drop and reload every item, so a big folder flashed blank).

### Window
- `AdwApplicationWindow` containing an `AdwOverlaySplitView`:
  - **Sidebar** (collapses into an overlay below 600sp; a toggle button
    appears in the content header; `F9` toggles): a list of places:
    1. *Home*, *Recent* (`recent:///`), *Starred* (`starred:///`, see
       "Starred"), *Trash* (`trash:///`).
    2. XDG user directories that exist and are not `$HOME`: Desktop,
       Documents, Downloads, Music, Pictures, Videos.
    3. GTK bookmarks from `$XDG_CONFIG_HOME/gtk-3.0/bookmarks`
       (format: one `URI[ space label]` per line). The file is monitored
       and the section refreshes when it changes.
    4. Devices: mounts and volumes from `GVolumeMonitor`. Mounted items
       show an eject (if `can-eject`) or unmount button; unmounted
       volumes mount on click (with a `GtkMountOperation` for passwords).
    5. *Network* (`network:///`) and *Other Locations*.
  - Right-click on a bookmark row: *Rename…*, *Remove*. On any row:
    *Open in New Tab*, *Open in New Window*.
  - Rows are drop targets: dropping files on a place moves/copies into it.
  - **Content**: header bar with back / forward buttons, the path bar
    (title), search toggle, view toggle (grid ⇄ list), view options menu
    (sort, zoom, show hidden) and the main menu; an operations button
    (visible while file operations run) opens the progress popover.
    Below it an `AdwTabBar` (auto-hidden with one tab), a search bar, then
    the `AdwTabView`. A status line at the bottom shows
    "N items" / "N items selected (size)".
- **Other Locations** is an internal page (`other-locations:` pseudo
  location) listing *Computer* (`file:///`), all volumes/mounts, *Networks*
  (`network:///`) and a *Connect to Server* entry that accepts any URI and
  mounts it (`g_file_mount_enclosing_volume`).
- Trash location shows a banner with *Empty Trash…*; items have *Restore*
  and *Delete Permanently* in the context menu.
- When a location is not mounted (`G_IO_ERROR_NOT_MOUNTED`) it is mounted
  automatically with a `GtkMountOperation` and reloaded. Other errors show
  an `AdwStatusPage` with the message.

### Path bar
- Breadcrumb buttons for each ancestor of the current location. A path
  under `$HOME` starts with a *Home* crumb; other local paths start with
  the root `/` crumb; other URI schemes start with the URI root (shown as
  host name, or a friendly name for `trash`, `recent`, `network`).
  Clicking a crumb navigates there.
- `Ctrl+L` switches to an editable entry showing the path (local) or URI
  (remote). Enter navigates (`g_file_parse_name`, so `~/x`, `/abs`, and
  any URI work). Escape returns to breadcrumbs. Pref
  `always-use-location-entry` keeps the entry permanently.

### Navigation & history
- Per-tab back/forward history (`Alt+Left`, `Alt+Right`, mouse buttons
  `mouse-back-button`/`mouse-forward-button` when
  `mouse-use-extra-buttons`), parent (`Alt+Up`), home (`Alt+Home`),
  reload (`F5`, `Ctrl+R`).
- Going up/back to a folder re-selects the child that was navigated from.
- Tabs: `Ctrl+T` new tab (same location), `Ctrl+W` closes the tab (the
  window when it is the last), `Ctrl+PageUp/PageDown` switch tabs (AdwTabView
  defaults); `Ctrl+N` new window. *Open in New Tab* / *Open in New Window*
  in the folder context menu.

### Views
- **Grid** (`GtkGridView`): icon + name (wrapped, up to 3 lines).
  Zoom levels (`org.gnome.nautilus.icon-view default-zoom-level`):
  small 48px, small-plus 64, medium 96, large 128, extra-large 256.
- **List** (`GtkColumnView`): Name (with icon), Size, Type, Modified;
  click headers to sort. Visible columns follow
  `org.gnome.nautilus.list-view default-visible-columns` (`size`, `type`,
  `date_modified`; name always shown). Zoom (`default-zoom-level`):
  small 16px, medium 32, large 48.
- `Ctrl+1` grid, `Ctrl+2` list (`default-folder-viewer`
  `icon-view`/`list-view` is the initial view and is updated on toggle).
  `Ctrl+plus` / `Ctrl+minus` / `Ctrl+0` zoom in/out/reset.
- Thumbnails: thumbnails from the freedesktop thumbnail cache are shown
  in the grid when allowed by `show-image-thumbnails` (`never`,
  `local-only`, `always`). Otherwise the themed icon for the content type.
  They are looked up for the tiles shown, on worker threads
  (`$XDG_CACHE_HOME/thumbnails/{large,x-large,xx-large,normal}/<md5(uri)>.png`
  whose `Thumb::MTime` equals the file's and whose `Thumb::URI`, if set,
  is the file's), not with GIO's `thumbnail::path` while a folder lists:
  that looks for every file and was most of a big folder's loading time
  (50 000 files: 916 ms with it, 345 ms without; `tests/load_bench.rs`).
  A video's thumbnail (MIME type `video/*`) is shown with a film strip
  over it (`ui/filmstrip.rs`): a dark band down each side, about a sixth
  of the picture's shorter side wide, with rounded sprocket holes cleared
  to transparent so the view's background shows through. It is drawn
  when shown, never saved into the shared thumbnail cache.
  A regular file (a remote one's GVfs FUSE path is found on the worker
  thread) with no valid cached thumbnail gets one made
  (`ui/thumbnailer.rs`), at most 2 at a time: the installed thumbnailer
  for its MIME type (`thumbnailers/*.thumbnailer` in the data folders,
  `%i %u %o %s` expanded, killed after 30 s), or for pictures without one
  Files' own gdk-pixbuf helper (`helix-files --thumbnail SIZE IN OUT`) run
  the same way. No file is ever decoded in the Files process itself (as in
  GNOME, whose thumbnail factory only runs thumbnailers): a picture without
  a thumbnailer gets none when the sandbox is unavailable. Scaled to fit
  256 px and saved to
  `$XDG_CACHE_HOME/thumbnails/large/<md5(uri)>.png` (0600, written to a
  temporary file and renamed) with `Thumb::URI` and `Thumb::MTime`, so GIO
  and other apps take it as valid. Failures are not written to the
  cache's `fail/` folder: a thumbnail that could not be made is not tried
  again while Files runs (unless the file's mtime changes) and is tried
  again on the next run.
- Thumbnailers run only in a bubblewrap sandbox (`ui/sandbox.rs`, after
  gnome-desktop's): read-only `/usr` (and the merged `/bin`, `/lib`…
  links), `/etc/ld.so.cache`, `/etc/alternatives` and the fontconfig
  cache; fresh `/proc` and `/dev`; a private temporary folder as `/tmp`
  holding the output (`/tmp/thumbnail.png`) and the input bound read-only
  as `/tmp/file-to-thumbnail.<ext>` (`%i`/`%u` point there); all
  namespaces unshared (no network), a new session, a cleared environment
  (`PATH`, `HOME=/tmp`, `LANG=C.UTF-8`, `GIO_USE_VFS=local`,
  `GST_REGISTRY_1_0`), killed with Files; GStreamer's registry kept in
  `$XDG_CACHE_HOME/helix-files/thumbnailer/gstreamer-1.0`. A seccomp
  filter (classic BPF built in Files, passed with `--seccomp`) refuses
  what gnome-desktop's refuses: other architectures' and x32 system
  calls, `syslog`, `uselib`, `acct`, `quotactl`, keyring calls, NUMA
  calls, `unshare`, `setns`, `mount`, `umount2`, `pivot_root`, `chroot`,
  `clone` with `CLONE_NEWUSER`, `clone3` and the new mount API (`ENOSYS`),
  `perf_event_open`, `ptrace`, `modify_ldt`, `ioctl` `TIOCSTI`/`TIOCLINUX`,
  personalities other than `PER_LINUX`, and sockets other than
  `AF_UNSPEC`/`AF_UNIX`/`AF_INET`/`AF_INET6`/`AF_NETLINK`. The output is
  read only as a regular file (not a symbolic link) of at most 32 MiB, and
  the temporary folder is removed afterwards. Whether bubblewrap can make
  a sandbox is checked once; where it cannot (not installed, user
  namespaces off), no thumbnailer runs. The package depends on
  `bubblewrap`. Thumbnails are read and decoded on
  worker threads, at most 4 at a time (`ui/thumbs.rs`): the tiles bound
  most recently first, and a request whose tile shows another file by
  its turn is dropped; the last 192 decoded thumbnails are kept. (Loading
  every bound tile's thumbnail at once kept about a thousand files open
  in a big media folder and ran the app out of file descriptors.)
- Hidden files (dot-files and backup files `*~`) are shown when
  `show-hidden-files` is true (`Ctrl+H` toggles and saves).
- Sorting: by name (natural, case-insensitive: `file2` < `file10`), size,
  type (description), modification time; ascending or reversed
  (`default-sort-order`, `default-sort-in-reverse-order`; `atime`,
  `btime`, `starred` fall back to name). Folders first when
  `org.gtk.gtk4.Settings.FileChooser sort-directories-first` is true.
  Ties are broken by name.
- Dates: `simple` shows "HH:MM" for today, "Yesterday", weekday within a
  week, "D Mon" within the year, else "D Mon YYYY"; `detailed` shows the
  full date and time.
- Activation: double-click (or single click when `click-policy` is
  `single`) / Enter opens. Folders and mountables navigate; other files
  open in the default application for their content type and are added
  to recent files, as in GNOME Files: `g_app_info_launch`, which gives
  the application the file's path (for a share such as `smb://`, its
  GVfs FUSE path) or its URI. No default application: the Open With
  dialog. (Not `GtkFileLauncher`: it goes through the OpenURI portal,
  which asks for an application for every type until one has been chosen
  a few times, and sends remote files to a handler of their URI scheme.) Items with `standard::target-uri` (recent, network) open
  their target.
- Typing while the window has focus starts a search (search bar key
  capture).

### Selection & context menus
- Multi-selection (`GtkMultiSelection`); `Ctrl+A` select all,
  `Shift+Ctrl+I` invert selection.
- Item context menu: Open, Open With…, Open in New Tab / Window (folders),
  Cut, Copy, Rename…, Move to Trash, Delete Permanently (if
  `show-delete-permanently`), Create Link (if `show-create-link`),
  Restore / Delete Permanently (in Trash), *Star* or *Unstar* (see
  "Starred"), Copy Location, Properties.
- Background menu: New Folder…, Paste, Select All, Add to Bookmarks,
  Properties (of the current folder).

### Operations
- **New Folder** (`Shift+Ctrl+N`): dialog with name validation (no `/`,
  not `.`/`..`, not existing); creates and selects it.
- **Rename** (`F2`): dialog with the name pre-selected up to the
  extension; uses `g_file_set_display_name`. Undoable (`Ctrl+Z`).
- **Cut / Copy / Paste** (`Ctrl+X/C/V`): the clipboard holds a
  `GdkFileList` (exported as `text/uri-list`), plus
  `x-special/gnome-copied-files` (`copy|cut\nURI…`) for interop and
  `text/plain` paths. Paste reads our own state first, else
  `x-special/gnome-copied-files`, else a file list. A cut is cleared
  after a successful paste.
- **Move to Trash** (`Delete`): `g_file_trash`; a toast "N items moved to
  Trash" offers *Undo* (also `Ctrl+Z`), which restores the most recently
  trashed entries whose `trash::orig-path` matches. If the filesystem has
  no usable trash (GIO answers "not supported": none, or a `.Trash-$UID`
  the user does not own, as on NTFS or FAT mounted for root), those items
  are not reported as errors: a dialog "“NAME” Can’t Be Put in the Trash"
  ("N Items …") offers *Delete* to delete them permanently (as Nautilus
  does) or *Cancel*.
- **Delete Permanently** (`Shift+Delete`): confirmation dialog, then
  recursive delete.
- **Restore** (in Trash): moves back to `trash::orig-path`, creating
  missing parent folders.
- **Empty Trash**: confirmation, then deletes all top-level items of
  `trash:///`.
- **Drag and drop**: items are drag sources (`GdkFileList`). Drop targets:
  the view background (current folder), folder items, sidebar places.
  Default action: move when source and destination are on the same
  filesystem, copy otherwise; holding Ctrl forces copy, Shift forces
  move. Dropping onto the folder the items already live in does nothing.
- **Create Link**: symbolic link "Link to NAME" in the same folder.
- Copy/move/trash/delete/restore/empty run on a worker thread with a
  `GCancellable`. A pre-scan computes item count and total bytes; the
  progress popover shows one row per job (title, progress bar named by the
  title, details, cancel button). The popover only opens when the user
  opens it — never by itself, so keyboard focus stays in the view. While
  jobs run, the header shows the operations button, whose icon is a pie
  of the combined progress (the mean of the jobs' fractions) and whose
  tooltip and accessible description say "File Operations — N % done".
- When a job finishes without errors a toast says so (libadwaita speaks
  toasts): "Copied 3 items to “Docs”", "Moved 1 item to “tmp”", "Created
  2 links", "Restored 4 items", "Deleted 2 items", "Trash emptied" (counts
  are the items actually produced when that is known); Move to Trash has
  its own toast with *Undo* (above). Errors open an alert listing them;
  a cancelled job shows "Operation cancelled".
- **Naming**: pasting into the folder the items came from creates
  "name (copy).ext", then "name (copy 2).ext", … (multi-dot archive
  extensions like `.tar.gz` stay intact; dot-files keep their name as
  the stem; copying "a (copy).txt" yields "a (copy 2).txt").
- **Conflicts** (destination exists, different source): dialog with
  *Skip*, *Keep Both* (copy naming), *Replace* (or *Merge* when both are
  folders) and *Cancel*, plus an "Apply to all" check box.
  Enter never overwrites: for files the default response is *Keep Both*
  (Replace is styled destructive and only chosen deliberately); for two
  folders the default stays *Merge* (styled suggested), which still asks
  again for every conflicting file inside. Escape always cancels.
- Moving or copying a folder into itself or a descendant is refused.

### Archives
- The item menu has *Create Archive…* (not in the Trash; disabled in
  search results, Starred, Recent and folders that can't be written to)
  and, when every selected item is an archive Files extracts (by content
  type: zip, tar, and tar compressed with gzip, xz, bzip2 or zstd; not
  zip-based documents such as OpenDocument or EPUB), *Extract Here* and
  *Extract To…*. Extract Here is disabled in a read-only folder; in
  search results and Starred it extracts next to each archive.
- *Create Archive…* opens "Create Archive": an *Archive Name* entry
  (default: the item's name, without its extension for a file, or
  "Archive" for several items) and an *Archive Format* drop-down
  (`.zip`, `.tar.gz`, `.tar.xz`, `.tar.zst`). A name that is taken in
  the folder disables *Create* and says so. The chosen format is
  remembered in `$XDG_CONFIG_HOME/helix-files/archive-format`, and in
  GNOME Files' `default-compression-format` when that key has it (zip,
  tar.xz); that key is read when Helix has no record yet.
- *Extract To…* asks for a folder with the GTK folder chooser (the
  FileChooser portal). Extracting puts an archive's one top-level item
  straight into the folder, and several items into a new folder named
  after the archive without its extension (`naming::split_extension`).
- Both run as file operations (progress, cancel, errors, toasts:
  "Created “x.zip”", "Extracted 2 archives"). They are made in a hidden
  work folder (`$XDG_CACHE_HOME/helix-files/work` when it is on the same
  filesystem, else the destination) and moved into place in one step, so
  cancelling or failing leaves nothing; a taken name becomes "name (2)",
  never replacing anything. The views follow through the folder monitor.
  (GTK's `GtkDirectoryList` stops following a folder once an entry it
  was told about vanishes before it could read it, which a short-lived
  staging folder in the destination would trigger.)
- Permission bits and modification times are kept (zip also stores the
  Unix timestamp); symbolic links are stored as links.
- Archives are untrusted (`src/archive.rs`): an entry with an absolute
  path or a `..` component, a symbolic link pointing outside the
  destination (absolute, or climbing higher than its folder; `..` may
  only come first, since after a link it climbs from wherever the link
  leads), a hard link to anything but a file extracted before, or an
  entry that would be written through a link stops the extraction with
  an error naming the entry. Files are created with `O_EXCL`, so a link
  is never followed. Device nodes and FIFOs are skipped; set-user-ID,
  set-group-ID and sticky bits are dropped. Extraction stops before the
  filesystem has less than 64 MiB left (a zip's sizes are checked up
  front) and when the output passes 1000 times the archive's size above
  1 GiB (a decompression bomb). Encrypted zips are refused ("Encrypted
  archives are not supported.").
- Crates: `zip`, `tar`, `flate2`, `lzma-rust2`, `bzip2` (libbz2-rs-sys),
  all pure Rust, and `zstd` (its C library built in, statically); no
  external archiver and no new runtime library.

### Search
- `Ctrl+F` shows the search bar; typing searches recursively under the
  current folder on a worker thread, case-insensitive substring match on
  names, results streamed incrementally into the view (with a
  "Searching…" indicator). `recursive-search`: `always`, `local-only`
  (recursive only for native files; remote folders search direct
  children only), `never`. Hidden files and directories are skipped
  unless hidden files are shown. Closing the search returns to the folder
  listing.

### Starred
Stars are GNOME Files' own, read and written in Nautilus's store
(helixdesktop/spec `contracts/files/starred.md`), so both apps share
them. The store is read on a worker thread, and read again when its
files change (looked at every 2 s), so stars set in GNOME Files show.
- *Starred* lists the starred files and folders inside the home folder
  that still exist, as a folder would (grid or list, sorted the same).
  It shows "No Starred Files" when there are none, and "Starred Files
  Unavailable" when the store can't be used. It has no folder to paste
  or drop into, and no search. Its item menu adds Open Item Location.
- *Star* is in the item menu when some selected files are not starred,
  *Unstar* when all are. Neither appears for files outside the home
  folder (as in Nautilus), in the Trash, or when the stars are read-only
  (a schema version or collation locale Helix doesn't write). Each change
  is announced ("Starred", "Unstarred").
- Starred files have a star after their name in the list and in their
  icon's corner in the grid, named "Starred".
- Renaming, moving and undoing a rename move the stars of the file and
  of the files inside it.
- Not yet: undo for stars, a clickable star in the list, and search in
  Starred (helixdesktop/desktop#155).

### Properties
- `Alt+Return` / `Ctrl+I`. For one item: icon, name, type, size (for
  folders computed recursively in the background, updating live:
  "N items, totalling X"), parent folder, link target, modified /
  accessed / created times, free space of the filesystem, and
  permissions (owner/group/others read, write, execute switches for
  files supporting `unix::mode`; changes applied immediately), and under
  the name a star toggle (named "Starred", pressed while starred) for
  files that can be starred. Each
  check box is named with its scope ("Owner read", "Group write",
  "Others execute"). For
  several items: count and combined size.

### Open With
- Dialog listing applications for the content type under two headings:
  "Recommended Applications" (`g_app_info_get_recommended_for_type`)
  and "Other Applications" (the rest of `g_app_info_get_all_for_type`).
  Only when no application handles the type is every installed
  application offered ("All Applications"). A search entry (focused on
  open; typing anywhere in the dialog goes to it) filters by name, shows
  "No Results Found" when nothing matches and announces the match count;
  Down moves into the list. *Open* is the default button; Enter in the
  search or on a row opens the selected (first matching) application.
  "Always use for this file type" calls
  `g_app_info_set_as_default_for_type`. App icons are presentational.

### Preferences dialog (`Ctrl+,`)
Default view, sort folders before files, click policy, show hidden
files, show "Delete Permanently", show "Create Link", recursive search,
show thumbnails, date format, always use location entry.

### GSettings
`org.gnome.nautilus.preferences`: `default-folder-viewer` (enum
`icon-view|list-view`), `show-hidden-files` (b), `click-policy` (enum
`single|double`), `show-delete-permanently` (b), `show-create-link` (b),
`recursive-search` (enum `always|local-only|never`),
`show-image-thumbnails` (enum `always|local-only|never`),
`default-sort-order` (enum `name|size|type|mtime|atime|btime|starred`),
`default-sort-in-reverse-order` (b), `date-time-format` (enum
`simple|detailed`), `always-use-location-entry` (b),
`mouse-use-extra-buttons` (b), `mouse-back-button` (u),
`mouse-forward-button` (u).
`org.gnome.nautilus.window-state`: `initial-size` (`(ii)`),
`maximized` (b).
`org.gnome.nautilus.list-view`: `default-zoom-level` (enum
`small|medium|large`), `default-visible-columns` (as).
`org.gnome.nautilus.icon-view`: `default-zoom-level` (enum
`small|small-plus|medium|large|extra-large`).
`org.gtk.gtk4.Settings.FileChooser`: `sort-directories-first` (b).
`org.gnome.nautilus.compression`: `default-compression-format` (enum
`zip|tar.xz|7z|encrypted_zip`; Helix writes only `zip` and `tar.xz`).
Missing schemas fall back to in-memory defaults (`helix-app-common::Prefs`).
`data/org.gnome.nautilus.gschema.xml` holds only these keys for systems
without Nautilus installed.

### D-Bus
- The application is unique (`org.helixdesktop.Files`); launching again opens a
  window in the running instance.
- Implements `org.freedesktop.FileManager1` at
  `/org/freedesktop/FileManager1` on the session bus, through the
  GApplication's own connection (so "show this folder / open in Files"
  from other apps and portals reaches the running instance):
  `ShowFolders(as uris, s startup_id)` opens each folder in a new window,
  `ShowItems(as, s)` opens each item's parent with the item selected
  (items sharing a parent share a window, further parents open as tabs),
  `ShowItemProperties(as, s)` does the same and opens the properties
  dialog. `startup_id` is passed to the first new window as its startup
  notification / XDG activation token. An empty URI list does nothing.
- Name ownership: `RequestName(org.freedesktop.FileManager1,
  ALLOW_REPLACEMENT)` — never `REPLACE_EXISTING`. If another file manager
  (e.g. Nautilus) already owns the name, Helix Files does not take it; it
  stays queued and gets the name only if the owner goes away. A file
  manager that requests with REPLACE may take it from Helix Files.
  Which file manager is the default is a **packaging decision**: the
  distribution installs exactly one `FileManager1` D-Bus service file
  (or the session starts the preferred one first); Helix Files does not
  fight for the name at runtime.
- D-Bus activation: `data/org.helixdesktop.Files.FileManager1.service`
  (`Name=org.freedesktop.FileManager1`,
  `Exec=/usr/bin/helix-files --gapplication-service`) installs to
  `$datadir/dbus-1/services/` (normally `/usr/share/dbus-1/services/`).
  It is named after the app, not the interface, so it does not collide on
  disk with Nautilus' `org.freedesktop.FileManager1.service`; if both are
  installed dbus-daemon picks one, so packaging must ship only the chosen
  default's file (e.g. via alternatives / package conflicts). In
  `--gapplication-service` mode no window is opened at start-up; windows
  come from the method calls, and the process exits 10 s after the last
  window closes.
- Tested only on a private bus (`dbus-run-session`): activation,
  all three methods, and not replacing an existing owner.

### Command line
`helix-files [URI|PATH…]` opens each location in a window (a regular file
opens its folder with the file selected). `-w/--new-window` forces a new
window even without arguments, `-s/--select` selects the given items in
their parents, `-q/--quit` quits the running instance.

### Keyboard shortcuts (summary)
`Ctrl+L` location, `Ctrl+F` search, `Ctrl+T`/`Ctrl+W` tab, `Ctrl+N`
window, `Ctrl+1`/`Ctrl+2` view, `Ctrl+H` hidden files, `Ctrl+D`
bookmark, `F2` rename, `Delete` trash, `Shift+Delete` delete,
`Ctrl+X/C/V` clipboard, `Ctrl+A` select all, `Ctrl+Z` undo,
`Alt+Return`/`Ctrl+I` properties, `Shift+Ctrl+N` new folder, `F5` reload,
`F9` sidebar, `Ctrl+,` preferences, `Ctrl+Q` quit, `Ctrl+?` Keyboard
Shortcuts window (also in the main menu, before About). The window is
built from a table that a unit test checks against every registered
accelerator and view shortcut, so it stays complete. "Show Hidden
Files" lives in the view menu (and the folder background menu), not in
the main menu.

## Deferred
- Tree (expandable) list view, batch rename, scripts, "Open in Terminal", folder item counts in the
  size column (`show-directory-item-counts`).

## Decisions
- FileChooser portal: Helix's own portal (helix-desktop) stays the
  FileChooser backend; Helix Files does not implement
  `org.freedesktop.impl.portal.FileChooser`.
- `org.freedesktop.FileManager1`: implemented; the name is requested
  without REPLACE, and packaging decides the default file manager.

### Accessibility
- Status changes that happen without focus moving are announced to
  screen readers (`helix_app_common::a11y`): a recursive search finishing ("Search finished: 12 items found" / "No results") and a folder that cannot be opened ("Unable to open folder: <reason>", once per failure); its "Unable to Open Folder" page has a *Try Again* button that reloads the folder. A finished file operation is reported by its toast; failures open an alert, cancellation shows a toast.
- The folder-loading spinner is named "Loading folder". Items that are
  cut (waiting for Paste) are dimmed and have the accessible description
  "Cut".
- Every control has a specific accessible name: icon-only buttons are
  named like their tooltip (without shortcut text); the root `/` crumb is
  named "Computer"; the slashes between crumbs are not exposed; the
  location and search entries are named "Location" and "Search Current
  Folder"; per-item buttons name their item ("Eject REPOS", "Cancel
  Copying 3 items to “Docs”"), and each job's progress bar is labelled by
  the job's title.
