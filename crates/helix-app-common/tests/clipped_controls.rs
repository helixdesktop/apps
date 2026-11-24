//! The size check (`HELIX_A11Y_CHECK=size`) reports controls a narrow
//! window cuts off, and not those that fit or scroll.
//!
//! The only GTK test in this binary.

use gtk::prelude::*;

#[test]
fn reports_cut_off_controls() {
    let Some(_display) = helix_app_common::testing::headless_gtk() else {
        return;
    };
    let window = gtk::Window::new();
    window.set_default_size(360, 294);
    window.set_resizable(false);
    // An overlay does not grow for its overlay children, so the window
    // stays narrow and cuts the second button off, as a breakpoint
    // layout can.
    let column = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let inside = gtk::Button::with_label("Inside");
    inside.set_halign(gtk::Align::Start);
    column.append(&inside);
    // A button in scrolled content is reached by scrolling.
    let scrolled_box = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let deep = gtk::Button::with_label("Scrolled");
    deep.set_margin_top(1000);
    scrolled_box.append(&deep);
    let scrolled = gtk::ScrolledWindow::builder()
        .child(&scrolled_box)
        .min_content_height(100)
        .build();
    column.append(&scrolled);
    let overlay = gtk::Overlay::new();
    overlay.set_child(Some(&column));
    let outside = gtk::Button::with_label("Outside");
    outside.set_halign(gtk::Align::Start);
    outside.set_valign(gtk::Align::Start);
    outside.set_margin_start(500);
    overlay.add_overlay(&outside);
    overlay.set_measure_overlay(&outside, false);
    window.set_child(Some(&overlay));
    window.present();
    let ctx = glib::MainContext::default();
    let end = std::time::Instant::now() + std::time::Duration::from_millis(500);
    while std::time::Instant::now() < end {
        ctx.iteration(false);
    }
    let found = helix_app_common::a11y::clipped_controls(&window);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].contains("Outside"), "{found:?}");
    window.close();
}
