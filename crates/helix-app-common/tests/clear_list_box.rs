//! `clear_list_box` empties a list that has a popover parented to it (as
//! Files' sidebar has its context menu), headers and a placeholder, where
//! `gtk_list_box_remove_all` never returns (it hung Files at start), and
//! the list takes new rows afterwards.

use gtk::prelude::*;

#[test]
fn clears_a_list_with_headers_and_a_placeholder() {
    let Some(_display) = helix_app_common::testing::headless_gtk() else {
        return;
    };
    let list = gtk::ListBox::new();
    let popover = gtk::PopoverMenu::from_model(None::<&gtk::gio::MenuModel>);
    popover.set_parent(&list);
    list.set_placeholder(Some(&gtk::Label::new(Some("Nothing here"))));
    list.set_header_func(|row, _| {
        if row.index() % 2 == 0 {
            row.set_header(Some(&gtk::Separator::new(gtk::Orientation::Horizontal)));
        }
    });
    let fill = |n: usize| {
        for i in 0..n {
            list.append(&gtk::Label::new(Some(&format!("row {i}"))));
        }
    };
    fill(6);
    list.invalidate_headers();
    // Headers and the placeholder are children of the list besides rows.
    let mut children = 0;
    let mut child = list.first_child();
    while let Some(c) = child {
        children += 1;
        child = c.next_sibling();
    }
    assert!(
        children > 6,
        "the popover and headers are children too ({children})"
    );

    let (tx, rx) = std::sync::mpsc::channel();
    let watchdog = std::thread::spawn(move || {
        if rx.recv_timeout(std::time::Duration::from_secs(10)).is_err() {
            eprintln!("clear_list_box did not return");
            std::process::abort();
        }
    });
    helix_app_common::clear_list_box(&list);
    tx.send(()).unwrap();
    watchdog.join().unwrap();
    assert!(list.row_at_index(0).is_none(), "every row removed");

    fill(3);
    assert!(list.row_at_index(2).is_some() && list.row_at_index(3).is_none());
    popover.unparent();
}
