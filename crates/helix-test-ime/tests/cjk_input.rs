//! CJK input through an input method in the kinds of text field the apps
//! use (#142): GTK's IBus module, talking to a fake IBus on a private bus
//! (never the host's), on a private headless display. For each field the
//! input method's preedit appears at the caret without being committed,
//! the committed text lands in the field, and the caret rectangle the
//! field reports (where the candidate window goes) is inside the field.
//!
//! The only GTK test in this binary.

use adw::prelude::*;
use helix_test_ime::{wait_for, Event, FakeIbus};
use vte4::prelude::*;

fn iterate() {
    glib::MainContext::default().iteration(false);
}

fn run(ms: u64) {
    let end = std::time::Instant::now() + std::time::Duration::from_millis(ms);
    while std::time::Instant::now() < end {
        if !glib::MainContext::default().iteration(false) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }
}

/// The field's bounds in the coordinates IBus is given (relative to the
/// window's surface).
fn surface_bounds(widget: &gtk::Widget) -> (f32, f32, f32, f32) {
    let native = widget.native().unwrap();
    let b = widget.compute_bounds(&native).unwrap();
    let (sx, sy) = native.surface_transform();
    (b.x() + sx as f32, b.y() + sy as f32, b.width(), b.height())
}

fn inside(rect: (i32, i32, i32, i32), bounds: (f32, f32, f32, f32)) -> bool {
    let (x, y, _, h) = rect;
    let (bx, by, bw, bh) = bounds;
    let slack = 3.0;
    (x as f32) >= bx - slack
        && (x as f32) <= bx + bw + slack
        && (y as f32) >= by - slack
        && ((y + h) as f32) <= by + bh + slack
}

struct Field {
    name: &'static str,
    widget: gtk::Widget,
    /// What the field holds, if it can be read back.
    text: Box<dyn Fn() -> String>,
}

fn fields() -> Vec<Field> {
    let entry = gtk::Entry::new();
    let search = gtk::SearchEntry::new();
    let row = adw::EntryRow::builder().title("Name").build();
    let rows = gtk::ListBox::new();
    rows.add_css_class("boxed-list");
    rows.append(&row);
    let view = gtk::TextView::new();
    view.set_size_request(300, 100);
    let terminal = vte4::Terminal::new();
    terminal.set_size(40, 5);
    let (e, s, r, v, t) = (
        entry.clone(),
        search.clone(),
        row.clone(),
        view.clone(),
        terminal.clone(),
    );
    vec![
        Field {
            name: "GtkEntry (Web's address, Calculator, dialogs)",
            widget: entry.upcast(),
            text: Box::new(move || e.text().to_string()),
        },
        Field {
            name: "GtkSearchEntry (search in Files, Contacts, Calendar, Settings, …)",
            widget: search.upcast(),
            text: Box::new(move || s.text().to_string()),
        },
        Field {
            name: "AdwEntryRow (Contacts and Calendar editors)",
            widget: rows.upcast(),
            text: Box::new(move || r.text().to_string()),
        },
        Field {
            name: "GtkTextView (Text Editor, event notes)",
            widget: view.upcast(),
            text: Box::new(move || {
                let b = v.buffer();
                b.text(&b.start_iter(), &b.end_iter(), true).to_string()
            }),
        },
        Field {
            name: "VteTerminal (Terminal)",
            widget: terminal.upcast(),
            text: Box::new(move || {
                t.text_format(vte4::Format::Text)
                    .map(|s| s.to_string())
                    .unwrap_or_default()
            }),
        },
    ]
}

#[test]
fn preedit_and_commit_in_text_fields() {
    let Some(ibus) = FakeIbus::start() else {
        return;
    };
    std::env::set_var("GTK_IM_MODULE", "ibus");
    std::env::set_var("IBUS_ADDRESS", ibus.address());
    let Some(_display) = helix_app_common::testing::headless_gtk() else {
        return;
    };
    adw::init().unwrap();
    let window = adw::Window::new();
    window.set_default_size(500, 300);
    window.present();
    run(200);

    for (i, field) in fields().into_iter().enumerate() {
        let holder = gtk::Box::new(gtk::Orientation::Vertical, 0);
        holder.set_margin_top(20);
        holder.set_margin_start(30);
        holder.append(&field.widget);
        window.set_content(Some(&holder));
        if let Ok(t) = field.widget.clone().downcast::<vte4::Terminal>() {
            // A child that echoes what is typed.
            t.spawn_async(
                vte4::PtyFlags::DEFAULT,
                None,
                &["/bin/cat"],
                &[],
                glib::SpawnFlags::DEFAULT,
                || {},
                -1,
                None::<&gtk::gio::Cancellable>,
                |_| {},
            );
        }
        run(200);
        ibus.clear();
        let target = match field.widget.clone().downcast::<gtk::ListBox>() {
            Ok(list) => list.row_at_index(0).unwrap().child().unwrap(),
            Err(w) => w,
        };
        target.grab_focus();
        let focused = wait_for(iterate, || {
            ibus.events().iter().any(|e| matches!(e, Event::FocusIn(_)))
        });
        if i == 0 && !focused {
            helix_app_common::testing::skip_gtk_test("GTK's IBus module (ibus-gtk4) is not loaded");
            return;
        }
        assert!(focused, "{}: no FocusIn", field.name);
        let ctx = ibus.focused().unwrap();
        helix_app_common::ime::track_composition(&target);
        assert!(
            wait_for(iterate, || ibus.cursor(ctx).is_some()),
            "{}: no caret rectangle",
            field.name
        );
        let before = ibus.cursor(ctx).unwrap();

        // Composition: preedit shown at the caret, not committed.
        assert!(ibus.preedit("にほん", 3), "{}", field.name);
        let moved = wait_for(iterate, || ibus.cursor(ctx).is_some_and(|c| c.0 > before.0));
        assert!(
            moved,
            "{}: the caret did not move with the preedit ({before:?})",
            field.name
        );
        run(100);
        assert!(
            !(field.text)().contains("にほん"),
            "{}: preedit was committed",
            field.name
        );
        // Capture-phase key handlers (Web's address bar, Text Editor,
        // Calculator) leave keys to the input method meanwhile.
        let tracked = target.is::<gtk::Entry>()
            || target.is::<gtk::SearchEntry>()
            || target.is::<gtk::TextView>();
        if tracked {
            assert!(
                helix_app_common::ime::is_composing(&target),
                "{}: composition not seen",
                field.name
            );
        }

        // A candidate is picked: committed into the field.
        ibus.preedit("", 0);
        assert!(ibus.commit("日本語"), "{}", field.name);
        let committed = wait_for(iterate, || (field.text)().contains("日本語"));
        assert!(
            committed,
            "{}: commit missing: {:?}",
            field.name,
            (field.text)()
        );
        assert!(
            !helix_app_common::ime::is_composing(&target),
            "{}",
            field.name
        );

        // The candidate window goes at the caret, inside the field.
        run(100);
        wait_for(iterate, || {
            field.widget.width() > 0 && field.widget.height() > 0
        });
        let mut rect = ibus.cursor(ctx).unwrap();
        let bounds = surface_bounds(&field.widget);
        if field.widget.is::<vte4::Terminal>() {
            // VTE reports the caret's y without the terminal's own place
            // in the window (#147); check x only.
            rect.1 = bounds.1 as i32;
            rect.3 = rect.3.min(bounds.3 as i32);
        }
        assert!(
            inside(rect, bounds),
            "{}: caret {rect:?} outside the field {bounds:?}",
            field.name
        );
        eprintln!("{}: ok, caret {rect:?} in {bounds:?}", field.name);
    }
    window.close();
    run(100);
}
