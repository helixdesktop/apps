# Image Viewer

Drop-in replacement for Loupe.

- **App ID**: `org.helixdesktop.ImageViewer`
- **Binary**: `helix-image-viewer`
- **Crate**: `apps/image-viewer`

## Purpose
View images.

## Main windows
- Image view with zoom, rotate, and slideshow.
- Thumbnail sidebar.

## Contracts used
- XDG portals: FileChooser, OpenURI, Wallpaper.
- GSettings: `org.gnome.Loupe` or equivalent.
- Standard image formats via gdk-pixbuf or glycin.

## Non-goals
- Full editor feature set.
- Printing (out of scope for now; no menu entry).

---

## Behaviour (derived spec)

### Window
- One `AdwApplicationWindow`. Inside: a toast overlay, an
  `AdwOverlaySplitView` whose (end-side) sidebar is the **Properties** panel,
  and an `AdwToolbarView` with a header bar on top of the image area.
- Header bar: *Open* button (start), a window title showing the file name
  with `n / total` position in the folder as subtitle, and (end) the primary
  menu and a *Properties* toggle button.
- **Window size**: outside fullscreen, and unless maximized or tiled, the
  window takes the size of the image so no bars show around it: each newly
  shown image (and each rotation) sets the image area to the image's own
  size (1 image px = 1 logical px), scaled down keeping its aspect to fit
  90 % × 85 % of the monitor less the window's own parts (header bar,
  strip when shown, properties sidebar when beside the image: its width
  computed as `AdwOverlaySplitView` does, a fraction of the whole width
  between its minimum and maximum). Showing or hiding the strip or the
  sidebar, un-maximizing and leaving fullscreen refit the window, so the
  image area keeps its size. The window is never smaller than 100 × 100 px
  nor than what its header bar needs; a small window sheds parts the menu
  and shortcuts still offer: below 300 px wide the *Open* button and the
  on-image controls (also below 220 px tall), below 200 px the title and
  the *Properties* button, and the previous / next buttons (also below
  150 px tall). The hidden properties sidebar is left out of the window's
  minimum size below 300 px until it is shown; its minimum width shrinks
  with the window (to half its usual 260 px).
- **Thumbnail strip**: a bottom bar (horizontal `gtk::ListView` in a
  scrolled window) with one 64 px square thumbnail per image of the list.
  Hidden by default; *Show Thumbnails* (`win.show-thumbnails`, a stateful
  toggle in the primary menu and the on-image controls, Ctrl+F9) shows it
  for that window (not remembered). Shown only when wanted, the list has
  more than one image, and not in fullscreen / slideshow. The current image is selected and scrolled into view; clicking
  a thumbnail shows that image. Thumbnails (longest side 128 px) are
  produced by two worker threads per window: a valid freedesktop thumbnail
  (`$XDG_CACHE_HOME/thumbnails/{normal,large}/<md5(uri)>.png` whose
  `Thumb::MTime` equals the file's mtime) is reused, otherwise the image is
  decoded at that size (pixbuf with embedded orientation; the Rust decoder
  for formats without a pixbuf loader). The thumbnail cache is only read,
  never written. Only thumbnails of bound (visible) rows are requested and
  requests for rows scrolled away are skipped; up to 2000 textures are kept
  per window.
- Several windows are possible (`app.new-window`, Ctrl+N); each has its own
  image list.

### Opening images
- **Command line**: `helix-image-viewer [FILE|DIR|URI ...]`.
  - One file: the file is shown; the image list is every supported image in
    the same folder.
  - Several files: the image list is exactly those files (in argument order,
    unsupported ones dropped).
  - A directory: its first image (natural sort) is shown, list = folder.
  - `-h/--help` prints usage. Unknown files show an error page.
- **Open dialog** (Ctrl+O, header button, empty-state button): a
  `gtk::FileDialog` (portal FileChooser when available) filtered to images,
  multi-select allowed with the same single/multiple rules as above.
- **Drag and drop**: dropping one or more files onto the window opens them
  with the same rules.
- With no image, an **empty state** (`AdwStatusPage`, “View Images”, with an
  *Open Files…* button and a hint that images can be dropped) is shown.

### Decoding
- Decoding runs on a worker thread (`gio::spawn_blocking`); the UI shows a
  spinner while loading and an error status page if decoding fails. Results
  of stale loads (user already navigated away) are discarded.
- Primary decoder: **gdk-pixbuf** (system loaders: PNG, JPEG, GIF, TIFF, BMP,
  ICO, TGA, PNM, XPM, SVG, WebP/HEIF/AVIF when the loaders are installed).
- Secondary decoder: the pure-Rust **`image` crate** restricted to GIF, WebP
  and QOI. Rationale: it gives proper per-frame delays for **animated GIF and
  animated WebP** (pixbuf's animation iterator is wall-clock driven and makes
  frame extraction awkward), and it covers WebP/QOI when pixbuf loaders for
  them are missing. No other formats are enabled, to keep the dependency
  footprint small.
- Animated images play in a loop using each frame's delay (minimum 20 ms,
  0-delay frames use 100 ms). Animations whose decoded frames would exceed
  512 MiB, or that have more than 10 000 frames, show only the first frame.
- **Decompression-bomb limits** (GIF/WebP headers can declare a huge canvas
  in a file of a few bytes; decoding is in-process, unlike glycin's
  sandboxed loaders with a memory rlimit): the declared canvas of a GIF or
  animated WebP may be at most 64 Mpx (8192², 256 MiB as RGBA, since every
  frame is composited onto a full canvas with up to three such buffers
  live), a still WebP at most 256 Mpx (16384², the largest a VP8/VP8L
  bitstream can be). The GIF decoder also gets an `image` allocation limit
  for those buffers. Larger files show the error page *The image is too
  large to open*, are not retried with gdk-pixbuf, and get no thumbnail.
- **EXIF orientation** (tag 0x0112, values 1–8) is read with `kamadak-exif`
  and applied to the decoded pixels for JPEG, TIFF, PNG and WebP. HEIF/AVIF
  are skipped because their container transforms are already applied by the
  decoder. Displayed dimensions are the oriented ones.

### Supported files / image list
- Supported extensions are those of the installed pixbuf formats plus
  `gif`, `webp`, `qoi` (case-insensitive).
- The folder list contains non-hidden regular files with supported
  extensions (the currently opened file is always included), sorted by
  **natural sort**: case-insensitive, digit runs compared numerically
  (`img2` < `img10`), ties broken by the raw name.
- The folder is **watched** (`gio::FileMonitor`); additions, removals and
  renames refresh the list while keeping the current image. If the current
  image disappears, the image that took its place in the list is shown (or
  the empty state if none remain).
- Navigation does not wrap around at the ends.

### Viewing
- **Best fit** by default: the image is scaled down to fit the view, never
  scaled up beyond 100 %.
- Zoom range: from min(best-fit, 100 %) down limit to 2000 %. Zoom steps
  multiply/divide by 1.25 (snapping to 100 % and best fit when crossing them).
- A **mouse wheel zooms** (one notch = one zoom step; no modifier
  needed). Touchpad scrolling pans instead, and zooms with Ctrl.
- Zoom in/out keeps the point under the pointer (wheel/scroll/pinch) or the
  view centre (keyboard/buttons) fixed.
- When the scaled image is larger than the view it can be **panned** by
  dragging (grab cursor), by touchpad scrolling, by tilting the wheel or
  Shift+wheel (left/right), or kept centred otherwise. Pan offsets are clamped so no blank space appears beyond the
  image edges.
- Double-click toggles between best fit and 100 % around the pointer.
- **Rotate** left/right (90° steps) changes the view; the file is only
  modified by an explicit **Save Rotation** (see below). An unsaved rotation
  is discarded when another image is shown.
- Scaling filter: trilinear when shrinking, linear up to 300 %, nearest
  neighbour above (so pixels are visible when zoomed far in).
- Overlay controls on the image (previous / next buttons at the sides; a
  bottom bar with rotate left/right, zoom out, zoom level menu-button with
  best fit / 100 % / 200 %, zoom in) are revealed on pointer motion and
  hidden after 2 s of inactivity.
- **Fullscreen** (F11, or menu): the header bar and controls auto-hide after
  2 s without pointer motion (cursor hidden too) and come back on motion.
  Esc leaves fullscreen.
- **Slideshow** (F5): enters fullscreen and advances every 5 s; stops at the
  last image or on Esc / F5.

### Properties panel (F9 / Alt+Return / header toggle)
- Shown or hidden only by the user (the toggle, its shortcuts, or
  dismissing it while it overlays the image): it stays as left across
  image changes, resizes, the collapse breakpoint (below 640 sp it
  overlays the image instead of sitting beside it), maximizing and
  minimizing. The split view's sidebar is pinned, so collapsing does not
  hide it nor expanding show it.
- Image: dimensions (`W × H` and megapixels), format, file size
  (`glib::format_size`), folder (with a button that opens it),
  created/modified dates (file system).
- EXIF section (shown only when present): date taken, camera (make + model,
  make not repeated if model starts with it), exposure (`1/250 s`),
  aperture (`ƒ/2.8`), ISO, focal length (`50 mm`), GPS location
  (`48.85840° N, 2.29450° E`, with a button opening a `geo:` URI via
  `gtk::UriLauncher`).

### Saving a rotation
- *Save Rotation* (Ctrl+S, primary menu, and a save button that appears in
  the overlay controls while the view is rotated) is enabled when the view
  rotation is not 0°, the image is not animated and its format is JPEG,
  PNG, TIFF or BMP (the gdk-pixbuf savers). Other formats (GIF, WebP, QOI,
  SVG, HEIF/AVIF, …) cannot be saved.
- Runs on a worker thread: the file is decoded again with gdk-pixbuf, its
  EXIF orientation and then the view rotation are applied to the pixels
  (what is displayed is what is written), and it is re-encoded in the same
  format.
- Metadata kept where practical: JPEG keeps the original EXIF APP1 segment
  with its Orientation tag reset to 1 (other tags, including
  PixelX/YDimension, are copied unchanged), the ICC profile, quality 95;
  PNG keeps the ICC profile and common `tEXt` chunks (Title, Author,
  Description, Copyright, Creation Time, Software, Comment, Source); TIFF
  keeps the ICC profile.
- The new data is written to a hidden temporary file in the same folder
  (`.<name>.helix-save-<pid>`), given the original permissions and renamed
  over the original. On success a toast “Image saved” is shown and the
  image is reloaded (rotation back to 0°); errors are shown as a toast.

### Accessibility
- The image canvas is focusable and exposed as an image named after the
  file, described by its position in the folder, pixel size and zoom ("3
  of 4, 3840×2160, 100 %"), updated as these change.
- Each thumbnail's alternative text is its file name; the strip is named
  "Images in Folder".
- Icon-only buttons have explicit names without shortcut text.

### Actions (primary menu and shortcuts)
| Action | Shortcut | Behaviour |
|---|---|---|
| Open… | Ctrl+O | Open dialog |
| New Window | Ctrl+N | Empty window |
| Open With… | Ctrl+Shift+O | `gtk::FileLauncher` with *always ask* (OpenURI portal) |
| Copy | Ctrl+C | Current (oriented, unrotated) frame to the clipboard as a texture |
| Set as Background… | — | XDG Wallpaper portal `SetWallpaperURI(parent, uri, {show-preview: true, set-on: "background"})` over zbus on a worker thread; waits for the `Request::Response` signal and toasts the result |
| Move to Trash | Delete | `gio::File::trash`, image removed from list, toast “Image moved to trash” with **Undo** which restores the file from the freedesktop trash (`$XDG_DATA_HOME/Trash`, or `$topdir/.Trash-$uid` / `$topdir/.Trash/$uid`, matching the `.trashinfo` `Path` with the latest `DeletionDate`). Only trash folders GIO would use count: the home trash and the trash at the top of the file's mount (not every folder above it), each a real folder (no symbolic link) owned by the user and not writable by others, with private `files`/`info`, and a shared `.Trash` only with its sticky bit; a planted entry elsewhere is ignored |
| Previous / Next | Left / Right, Page Up / Page Down | Navigate list (prev/next overlay buttons too) |
| First / Last | Home / End | |
| Zoom in / out | Ctrl+= / Ctrl++ / Ctrl+KP_Add, Ctrl+- / Ctrl+KP_Subtract, mouse wheel, Ctrl+scroll, pinch | |
| Best fit | Ctrl+0 | |
| 100 % / 200 % | Ctrl+1 / Ctrl+2 | |
| Rotate right / left | Ctrl+R / Ctrl+Shift+R | View rotation |
| Save Rotation | Ctrl+S | Re-encode the rotated image into its file |
| Properties | F9, Alt+Return | Toggle sidebar |
| Image menu | Secondary click / long press on the image, Menu, Shift+F10 | Context menu at the pointer (keyboard: the image's centre), only while an image is shown: *Copy*; *Open With…*, *Set as Background…*; *Move to Trash* (the same window actions) |
| Fullscreen | F11 | Toggle; Esc leaves |
| Slideshow | F5 | Toggle |
| Show Thumbnails | Ctrl+F9 | Toggle the thumbnail strip (off by default) |
| Close window / Quit | Ctrl+W / Ctrl+Q | |

### GSettings
`org.gnome.Loupe` (through `Prefs`; in memory when the schema is not
installed): `show-properties` (`b`, default false) — initial state of the
Properties sidebar, written whenever it is shown or hidden.
`data/org.gnome.Loupe.gschema.xml` holds just this key, copied from the
published schema.

### Command line
`helix-image-viewer [FILE|DIR|URI ...]` (see *Opening images*).
Testing: `HELIX_IMAGE_VIEWER_SMOKE=1` runs a scripted walk-through (rotate,
save, next, properties, last, first, quit) — only use it on scratch
copies, since it rewrites the opened image.

## Deferred
- Printing.
- Preloading neighbouring images, colour management, HDR.

## Decisions
- Save rotation: an explicit *Save Rotation* action re-encodes the rotated
  image into its file through the pixbuf pipeline, keeping metadata where
  practical.
- Thumbnails: kept as a bottom thumbnail strip, hidden until asked for.
- Window size follows the image (no bars), like the video player follows
  the video.
- GSettings: the real `org.gnome.Loupe` key `show-properties` is used.
