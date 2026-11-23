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
    // Its display socket is listening once the web one is.
    for _ in 0..100 {
        if web.exists() {
            break;
        }
        if let Ok(Some(status)) = daemon.0.try_wait() {
            return Err(format!("gtk4-broadwayd :{display} exited ({status})"));
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let ready = web.exists();
    std::fs::remove_file(&web).ok();
    if !ready {
        return Err(format!(
            "gtk4-broadwayd :{display} made no socket at {} in 5 s",
            web.display()
        ));
    }
    std::env::set_var("GDK_BACKEND", "broadway");
    std::env::set_var("BROADWAY_DISPLAY", format!(":{display}"));
    std::env::set_var("GTK_A11Y", "test");
    gtk::init().map_err(|e| format!("GTK can't open Broadway display :{display}: {e}"))?;
    Ok(daemon)
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
