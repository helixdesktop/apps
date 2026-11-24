//! Calendar crashed under Broadway (#120): GTK 4.22 calls a NULL function
//! when `gtk_accessible_announce` runs on a widget whose accessibility
//! context is not AT-SPI (Broadway, or no accessibility bus; GTK then
//! uses its test context). The helpers in `a11y` check the context; this
//! shows, on a private headless display without AT-SPI, every kind of
//! announcement and toast the apps use, and must not crash. A second test
//! checks that no code calls `announce` on a widget directly.
//!
//! The only GTK test in this binary.

use adw::prelude::*;
use helix_app_common::{announce, announce_change, announce_settled, Urgency};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[test]
fn announcements_and_toasts_without_at_spi() {
    let Some(_display) = helix_app_common::testing::headless_gtk() else {
        return;
    };
    adw::init().unwrap();
    let window = adw::Window::new();
    let overlay = adw::ToastOverlay::new();
    let label = gtk::Label::new(Some("x"));
    overlay.set_child(Some(&label));
    window.set_content(Some(&overlay));
    window.present();
    let context = window.at_context().type_().name().to_string();
    assert_ne!(
        context, "GtkAtSpiContext",
        "the test needs a non-AT-SPI context"
    );

    let ctx = glib::MainContext::default();
    let run = |ms: u64| {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            if !ctx.iteration(false) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    };
    run(200);
    announce(&label, "Polite", Urgency::Polite);
    announce(&window, "Assertive", Urgency::Assertive);
    announce_change(&label, "Changed", Urgency::Polite);
    announce_settled(&label, "Settled");
    overlay.add_toast(adw::Toast::new("A toast"));
    overlay.add_toast(
        adw::Toast::builder()
            .title("An error")
            .priority(adw::ToastPriority::High)
            .timeout(0)
            .build(),
    );
    overlay.add_toast(
        adw::Toast::builder()
            .title("Deleted")
            .button_label("Undo")
            .timeout(1)
            .build(),
    );
    let alert = adw::AlertDialog::new(Some("Heading"), Some("Body"));
    alert.add_response("close", "Close");
    alert.present(Some(&window));
    run(300);
    alert.close();
    let prefs = adw::PreferencesDialog::new();
    prefs.present(Some(&window));
    run(200);
    prefs.add_toast(adw::Toast::new("In a dialog"));
    // announce_settled fires after 0.7 s; toasts time out after 1 s.
    run(1500);
    prefs.close();
    window.close();
    run(100);
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if name != "target" && !name.starts_with('.') {
                rust_files(&p, out);
            }
        } else if name.ends_with(".rs") {
            out.push(p);
        }
    }
}

/// Every announcement goes through the guarded helpers: a direct
/// `Accessible::announce` would crash without AT-SPI.
#[test]
fn nothing_announces_directly() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    rust_files(&root.join("apps"), &mut files);
    rust_files(&root.join("crates"), &mut files);
    let mut direct = Vec::new();
    for f in files {
        if f.ends_with("helix-app-common/src/a11y.rs") || f.ends_with("tests/broadway_announce.rs")
        {
            continue;
        }
        let text = std::fs::read_to_string(&f).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or("");
            if code.contains(".announce(") || code.contains("gtk_accessible_announce") {
                direct.push(format!("{}:{}: {}", f.display(), n + 1, line.trim()));
            }
        }
    }
    assert!(
        direct.is_empty(),
        "use helix_app_common::announce (it checks for an AT-SPI context):\n{}",
        direct.join("\n")
    );
}
