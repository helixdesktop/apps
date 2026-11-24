//! A control with a keyboard shortcut is named by its label alone, with a
//! pseudo-translation too: the shortcut is passed apart from the label and
//! never taken back out of the translated tooltip.
//!
//! This is the only test in this binary: it sets the process locale and
//! starts GTK on a private headless display (skipped without one).

use gtk::prelude::*;
use helix_app_common::i18n::{self, tr};
use helix_app_common::testing::{headless_gtk, label_mismatch, property_mismatch};

#[test]
fn shortcut_is_not_part_of_the_name() {
    let dir = std::env::temp_dir().join(format!("helix-shortcut-names-{}", std::process::id()));
    let messages = dir.join("xx/LC_MESSAGES");
    std::fs::create_dir_all(&messages).unwrap();
    i18n::write_test_catalogue(
        &messages.join(format!("{}.mo", i18n::DOMAIN)),
        &[
            ("", "Content-Type: text/plain; charset=UTF-8\n"),
            ("Search", "⟦Šéàŕçĥ (ŵîţĥ ƒîļţéŕš)⟧"),
            ("{label} ({shortcut})", "⟦{label} 【{shortcut}】⟧"),
        ],
    );
    std::env::set_var("HELIX_LOCALEDIR", &dir);
    std::env::set_var("LANGUAGE", "xx");
    // LANGUAGE is ignored in the C locale; use any installed real one.
    let translated = ["en_US.UTF-8", "C.UTF-8", "en_GB.UTF-8"].iter().any(|l| {
        std::env::set_var("LC_ALL", l);
        i18n::init_for_tests();
        tr("Search") != "Search"
    });
    std::fs::remove_dir_all(&dir).ok();
    if !translated {
        eprintln!("no locale accepts LANGUAGE here; skipping");
        return;
    }
    let Some(_display) = headless_gtk() else {
        return;
    };
    let label = tr("Search");
    let button = gtk::Button::from_icon_name("system-search-symbolic");
    helix_app_common::set_label_and_shortcut(&button, label, "<Control><Shift>f");
    let keys = gtk::accelerator_get_label(
        gtk::gdk::Key::f,
        gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::SHIFT_MASK,
    );
    assert_eq!(
        button.tooltip_text().unwrap(),
        format!("⟦⟦Šéàŕçĥ (ŵîţĥ ƒîļţéŕš)⟧ 【{keys}】⟧")
    );
    // The whole translated label, its own parentheses included.
    assert_eq!(label_mismatch(&button, "⟦Šéàŕçĥ (ŵîţĥ ƒîļţéŕš)⟧"), None);
    assert_eq!(
        property_mismatch(
            &button,
            gtk::AccessibleProperty::KeyShortcuts,
            "Control+Shift+F"
        ),
        None
    );
}
