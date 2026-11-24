//! Helpers for tests that need GTK: a private headless display and GTK's
//! own view of accessible properties. Not for use in the apps.

use gtk::glib;
use gtk::prelude::*;
use std::process::{Child, Command, Stdio};

/// A private headless display (`gtk4-broadwayd`, reached through a socket
/// in the test's runtime directory); stopped when dropped.
pub struct Headless(Child);

impl Drop for Headless {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Starts [`Headless`] and initialises GTK on it, on the calling thread,
/// with GTK's test accessibility backend. `None` when that is not
/// possible (no `gtk4-broadwayd`); the caller then returns, skipping its
/// test. With `HELIX_REQUIRE_GTK=1` (set in CI) that is a panic instead,
/// so CI can't pass with the GTK tests silently skipped (#201). Call it
/// once per test process, from a test that is alone in its binary or the
/// only one that uses GTK.
pub fn headless_gtk() -> Option<Headless> {
    match start_headless() {
        Ok(display) => Some(display),
        Err(why) => {
            skip_gtk_test(&format!("no headless GTK display: {why}"));
            None
        }
    }
}

/// Reports that a GTK test can't run here, for `why`, so the caller can
/// return; with `HELIX_REQUIRE_GTK=1` it panics instead.
pub fn skip_gtk_test(why: &str) {
    if std::env::var("HELIX_REQUIRE_GTK").is_ok_and(|v| v == "1") {
        panic!("{why}, and HELIX_REQUIRE_GTK=1");
    }
    eprintln!("{why}; skipping");
}

fn start_headless() -> Result<Headless, String> {
    // URIs such as network:/// would start gvfs daemons otherwise.
    std::env::set_var("GIO_USE_VFS", "local");
    let runtime = glib::user_runtime_dir();
    let display = 40 + std::process::id() % 500;
    let web = runtime.join(format!("helix-test-web-{display}.socket"));
    let child = Command::new("gtk4-broadwayd")
        .arg(format!(":{display}"))
        .arg("--unixsocket")
        .arg(&web)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("can't start gtk4-broadwayd (libgtk-4-bin): {e}"))?;
    let mut daemon = Headless(child);
    // GTK connects to the display socket, which gtk4-broadwayd creates
    // and listens on only after its web one: wait until it takes a
    // connection (#209; GTK failed to start beside a busy CPU).
    let socket = runtime.join(format!("broadway{}.socket", display + 1));
    let mut ready = false;
    for _ in 0..100 {
        if std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            ready = true;
            break;
        }
        if let Ok(Some(status)) = daemon.0.try_wait() {
            std::fs::remove_file(&web).ok();
            return Err(format!("gtk4-broadwayd :{display} exited ({status})"));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    std::fs::remove_file(&web).ok();
    if !ready {
        return Err(format!(
            "gtk4-broadwayd :{display} took no connection at {} in 5 s",
            socket.display()
        ));
    }
    std::env::set_var("GDK_BACKEND", "broadway");
    std::env::set_var("BROADWAY_DISPLAY", format!(":{display}"));
    std::env::set_var("GTK_A11Y", "test");
    gtk::init().map_err(|e| format!("GTK can't open Broadway display :{display}: {e}"))?;
    // Broadway sets no font resolution, as a desktop session does; then
    // libadwaita before 1.7 (Ubuntu 24.04's 1.5) takes `sp` lengths as 0
    // and no breakpoint in `sp` ever applies (libadwaita commit 9759d3fd,
    // "length-unit: Also fall back if gtk-xft-dpi is default value").
    if let Some(settings) = gtk::Settings::default() {
        if settings.gtk_xft_dpi() <= 0 {
            settings.set_gtk_xft_dpi(96 * 1024);
        }
    }
    Ok(daemon)
}

/// Makes a mapped `window` on the [`Headless`] display take its default
/// size, as a compositor does when GTK asks for it. GTK's Broadway backend
/// only does that itself from GTK 4.16 (GTK commit 9a8e5b07, "Add missed
/// compute_size and request_layout vfunc to GdkBroadwaySurface"); before
/// that (Ubuntu 24.04's 4.14) it sizes a window only when GTK presents
/// it again, which `gtk_window_unmaximize` does (`gtk_window_present`
/// only focuses a visible window). Does nothing from 4.16 on, or
/// while the window is maximized or fullscreen. Call it while waiting for
/// a resize.
///
/// It presents the window once for each new default size, and again only
/// after [`REPRESENT_AFTER`]: presenting resizes the window's surface at
/// once, but the window takes that size only when its frame clock lays it
/// out. Every present makes `gtk4-broadwayd` answer with a configure event,
/// which GLib dispatches at `G_PRIORITY_DEFAULT`, ahead of the frame clock
/// (`GDK_PRIORITY_EVENTS + 1` and `GDK_PRIORITY_REDRAW`). Presenting on
/// every turn of a waiting loop kept an answer ready on every turn, so the
/// window never laid out and the wait ran out (#209).
pub fn follow_default_size(window: &impl IsA<gtk::Window>) {
    let window = window.as_ref();
    if gtk::check_version(4, 16, 0).is_none()
        || !window.is_mapped()
        || window.is_maximized()
        || window.is_fullscreen()
    {
        return;
    }
    let (width, height) = window.default_size();
    if (width <= 0 || window.width() == width) && (height <= 0 || window.height() == height) {
        return;
    }
    let now = std::time::Instant::now();
    let due = PRESENTED.with_borrow_mut(|presented| {
        presented.retain(|(w, _, _)| w.upgrade().is_some());
        match presented
            .iter_mut()
            .find(|(w, _, _)| w.upgrade().as_ref() == Some(window))
        {
            Some((_, size, at)) if *size == (width, height) && now - *at < REPRESENT_AFTER => false,
            Some((_, size, at)) => {
                (*size, *at) = ((width, height), now);
                true
            }
            None => {
                presented.push((window.downgrade(), (width, height), now));
                true
            }
        }
    });
    if due {
        window.unmaximize();
    }
}

/// How long [`follow_default_size`] lets a window lay out at a size it
/// was presented at before presenting it again. Broadway paints at most
/// once a second when no browser is connected to it.
const REPRESENT_AFTER: std::time::Duration = std::time::Duration::from_millis(1500);

/// A window [`follow_default_size`] presented, at which default size, and
/// when.
type Presented = (glib::WeakRef<gtk::Window>, (i32, i32), std::time::Instant);

thread_local! {
    static PRESENTED: std::cell::RefCell<Vec<Presented>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// `None` when GTK's own accessible `property` of `widget` is the string
/// `expected`, else what GTK says about it.
pub fn property_mismatch(
    widget: &impl IsA<gtk::Accessible>,
    property: gtk::AccessibleProperty,
    expected: &str,
) -> Option<String> {
    use glib::translate::{IntoGlib, ToGlibPtr};
    let expected = std::ffi::CString::new(expected).unwrap();
    // SAFETY: `widget` is a live GtkAccessible; the string properties take
    // one string argument; the result is NULL or a newly allocated message.
    unsafe {
        let accessible: *mut gtk::ffi::GtkAccessible = widget.as_ref().to_glib_none().0;
        let message = gtk::ffi::gtk_test_accessible_check_property(
            accessible,
            property.into_glib(),
            expected.as_ptr(),
        );
        if message.is_null() {
            return None;
        }
        let text = std::ffi::CStr::from_ptr(message)
            .to_string_lossy()
            .into_owned();
        glib::ffi::g_free(message.cast());
        Some(text)
    }
}

/// [`property_mismatch`] for the accessible label (name).
pub fn label_mismatch(widget: &impl IsA<gtk::Accessible>, expected: &str) -> Option<String> {
    property_mismatch(widget, gtk::AccessibleProperty::Label, expected)
}
