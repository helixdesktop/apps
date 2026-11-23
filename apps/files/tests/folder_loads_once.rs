//! A folder loads once. Files used to turn monitoring on after a folder
//! had loaded, and GTK then drops every item and loads the folder again
//! (documented for `gtk_directory_list_set_monitored`): a big folder
//! showed, went blank and refilled. Monitoring from the start, as Files
//! does now, loads it once and still follows changes.

use gtk::prelude::*;
use gtk::{gio, glib};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

fn run_until(main: &glib::MainLoop, done: impl Fn() -> bool + 'static) {
    let m = main.clone();
    glib::timeout_add_local(Duration::from_millis(20), move || {
        if done() {
            m.quit();
            return glib::ControlFlow::Break;
        }
        glib::ControlFlow::Continue
    });
    let m = main.clone();
    let guard = glib::timeout_add_local_once(Duration::from_secs(20), move || m.quit());
    main.run();
    let _ = guard;
}

#[test]
fn a_monitored_folder_loads_once_and_follows_changes() {
    let Some(_display) = helix_app_common::testing::headless_gtk() else {
        return;
    };
    let dir = std::env::temp_dir().join(format!("helix-files-loads-once-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..2000 {
        std::fs::write(dir.join(format!("f{i}.txt")), "x").unwrap();
    }
    // As `Tab::new` / `Tab::load` set it up.
    let list = gtk::DirectoryList::new(Some("standard::name"), None::<&gio::File>);
    list.set_monitored(true);
    list.set_io_priority(glib::Priority::DEFAULT_IDLE);
    let removed = Rc::new(Cell::new(0u32));
    let r = removed.clone();
    list.connect_items_changed(move |_, _, rem, _| r.set(r.get() + rem));
    let loads = Rc::new(Cell::new(0u32));
    let l = loads.clone();
    list.connect_loading_notify(move |d| {
        if !d.is_loading() {
            l.set(l.get() + 1);
        }
    });
    list.set_file(Some(&gio::File::for_path(&dir)));
    let main = glib::MainLoop::new(None, false);
    let l = list.clone();
    run_until(&main, move || !l.is_loading() && l.n_items() == 2000);
    assert_eq!(list.n_items(), 2000);
    // Give a reload (if any) the chance to start.
    let waited = Rc::new(RefCell::new(std::time::Instant::now()));
    let w = waited.clone();
    run_until(&main, move || {
        w.borrow().elapsed() > Duration::from_millis(300)
    });
    assert_eq!(removed.get(), 0, "items were dropped after loading");
    assert_eq!(loads.get(), 1, "the folder loaded more than once");
    assert_eq!(list.n_items(), 2000);

    // Still monitored: a new file shows up.
    std::fs::write(dir.join("new.txt"), "x").unwrap();
    let l = list.clone();
    run_until(&main, move || l.n_items() == 2001);
    assert_eq!(list.n_items(), 2001);
    let _ = std::fs::remove_dir_all(&dir);
}
