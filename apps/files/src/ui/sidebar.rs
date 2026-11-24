//! Places sidebar: standard places, user dirs, bookmarks, devices, network.

use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use super::{Window, OTHER_LOCATIONS};
use crate::bookmarks;
use helix_app_common::i18n::tr;
use helix_app_common::nc_;
use helix_app_common::trf;

#[derive(Clone)]
enum Kind {
    Location(gio::File),
    Bookmark(usize, gio::File),
    Mount(gio::Mount),
    Volume(gio::Volume),
}

#[derive(Clone)]
struct Place {
    kind: Kind,
    section: u8,
    label: String,
    icon: gio::Icon,
}

impl Place {
    fn file(&self) -> Option<gio::File> {
        match &self.kind {
            Kind::Location(f) | Kind::Bookmark(_, f) => Some(f.clone()),
            Kind::Mount(m) => Some(m.root()),
            Kind::Volume(v) => v.get_mount().map(|m| m.root()),
        }
    }
}

pub struct Sidebar {
    pub widget: gtk::ScrolledWindow,
    list: gtk::ListBox,
    places: RefCell<Vec<Place>>,
    win: Weak<Window>,
    monitor: gio::VolumeMonitor,
    handlers: RefCell<Vec<glib::SignalHandlerId>>,
    file_monitors: RefCell<Vec<gio::FileMonitor>>,
    popover: gtk::PopoverMenu,
    menu_target: Cell<Option<usize>>,
    trash_full: Cell<bool>,
    /// The window closed: late volume, mount and trash updates are
    /// ignored.
    closed: Cell<bool>,
}

fn themed(name: &str) -> gio::Icon {
    gio::ThemedIcon::new(name).upcast()
}

fn location(label: &str, icon: &str, file: gio::File, section: u8) -> Place {
    Place {
        kind: Kind::Location(file),
        section,
        label: label.into(),
        icon: themed(icon),
    }
}

/// The places that are always there: Home, Recent, Starred, Trash and the user
/// folders, then Network and Other Locations (the last two).
fn fixed_places(trash_full: bool) -> Vec<Place> {
    let home = glib::home_dir();
    let mut v = vec![
        location(
            tr(nc_!("home folder", "Home")),
            "user-home-symbolic",
            gio::File::for_path(&home),
            0,
        ),
        location(
            tr("Recent"),
            "document-open-recent-symbolic",
            gio::File::for_uri("recent:///"),
            0,
        ),
        location(
            tr("Starred"),
            "starred-symbolic",
            gio::File::for_uri(super::starred::STARRED),
            0,
        ),
        location(
            tr("Trash"),
            if trash_full {
                "user-trash-full-symbolic"
            } else {
                "user-trash-symbolic"
            },
            gio::File::for_uri("trash:///"),
            0,
        ),
    ];
    for (dir, icon) in [
        (glib::UserDirectory::Desktop, "user-desktop-symbolic"),
        (glib::UserDirectory::Documents, "folder-documents-symbolic"),
        (glib::UserDirectory::Downloads, "folder-download-symbolic"),
        (glib::UserDirectory::Music, "folder-music-symbolic"),
        (glib::UserDirectory::Pictures, "folder-pictures-symbolic"),
        (glib::UserDirectory::Videos, "folder-videos-symbolic"),
    ] {
        if let Some(p) = glib::user_special_dir(dir) {
            if p != home && p.is_dir() {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                v.push(location(&name, icon, gio::File::for_path(&p), 1));
            }
        }
    }
    v.push(location(
        tr("Network"),
        "network-workgroup-symbolic",
        gio::File::for_uri("network:///"),
        4,
    ));
    v.push(location(
        tr("Other Locations"),
        "list-add-symbolic",
        gio::File::for_uri(OTHER_LOCATIONS),
        4,
    ));
    v
}

fn bookmark_places() -> Vec<Place> {
    bookmarks::load()
        .into_iter()
        .enumerate()
        .map(|(i, b)| bookmark_place(i, &b))
        .collect()
}

fn bookmark_place(i: usize, b: &bookmarks::Bookmark) -> Place {
    let file = gio::File::for_uri(&b.uri);
    let icon = if file.is_native() {
        "folder-symbolic"
    } else {
        "folder-remote-symbolic"
    };
    Place {
        kind: Kind::Bookmark(i, file),
        section: 2,
        label: b.display_label(),
        icon: themed(icon),
    }
}

/// A sidebar row showing `p`: its icon and label in `content` (where the
/// eject button goes), its location as the tooltip.
fn place_row(p: &Place) -> (gtk::ListBoxRow, gtk::Box) {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    content.append(&gtk::Image::from_gicon(&p.icon));
    let label = gtk::Label::builder()
        .label(&p.label)
        .xalign(0.0)
        .hexpand(true)
        .ellipsize(gtk::pango::EllipsizeMode::End)
        .build();
    content.append(&label);
    let row = gtk::ListBoxRow::builder()
        .child(&content)
        .tooltip_text(
            p.file()
                .map(|f| super::window::display_location(&f))
                .unwrap_or_default(),
        )
        .build();
    // GTK would name the row after its tooltip (a path or URI such as
    // "trash:///"); screen readers should say the label.
    helix_app_common::set_accessible_label(&row, &p.label);
    row.set_widget_name(&p.section.to_string());
    (row, content)
}

impl Sidebar {
    pub fn new(win: &Rc<Window>) -> Rc<Sidebar> {
        let list = gtk::ListBox::new();
        list.add_css_class("navigation-sidebar");
        list.set_activate_on_single_click(true);
        let widget = gtk::ScrolledWindow::builder()
            .child(&list)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .build();
        // Parented to the scrolled window, not the list: a GtkListBox's
        // dispose runs `gtk_list_box_remove_all`, which never returns while
        // a non-row child (this popover) is left, so closing the window
        // froze it. The destroy handler below runs only after that.
        let popover = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
        popover.set_parent(&widget);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        let p = popover.clone();
        widget.connect_destroy(move |_| p.unparent());

        let sb = Rc::new(Sidebar {
            widget,
            list,
            places: RefCell::default(),
            win: Rc::downgrade(win),
            monitor: gio::VolumeMonitor::get(),
            handlers: RefCell::default(),
            file_monitors: RefCell::default(),
            popover,
            menu_target: Cell::new(None),
            trash_full: Cell::new(false),
            closed: Cell::new(false),
        });
        sb.list.set_header_func(|row, before| {
            let section = |r: &gtk::ListBoxRow| r.widget_name().to_string();
            match before {
                Some(b) if section(b) != section(row) => {
                    row.set_header(Some(&gtk::Separator::new(gtk::Orientation::Horizontal)));
                }
                _ => row.set_header(None::<&gtk::Widget>),
            }
        });
        sb.connect();
        sb.rebuild();
        sb.refresh_trash();
        sb
    }

    fn connect(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.list.connect_row_activated(move |_, row| {
            if let Some(s) = weak.upgrade() {
                s.activate(row.index() as usize, false);
            }
        });

        // Middle click → new tab; right click → menu.
        let click = gtk::GestureClick::new();
        click.set_button(0);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |g, _, x, y| {
            let Some(s) = weak.upgrade() else { return };
            let Some(row) = s.list.row_at_y(y as i32) else {
                return;
            };
            let idx = row.index() as usize;
            match g.current_button() {
                gdk::BUTTON_MIDDLE => {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    s.activate(idx, true);
                }
                gdk::BUTTON_SECONDARY => {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    s.popup(idx, x, y);
                }
                _ => {}
            }
        });
        self.list.add_controller(click);

        // Touch: long-press opens the row's menu.
        let long_press = gtk::GestureLongPress::new();
        long_press.set_touch_only(true);
        let weak = Rc::downgrade(self);
        long_press.connect_pressed(move |g, x, y| {
            let Some(s) = weak.upgrade() else { return };
            if let Some(row) = s.list.row_at_y(y as i32) {
                g.set_state(gtk::EventSequenceState::Claimed);
                s.popup(row.index() as usize, x, y);
            }
        });
        self.list.add_controller(long_press);

        // Keyboard: Menu / Shift+F10 open the focused row's menu, F2
        // renames a focused bookmark.
        let keys = gtk::ShortcutController::new();
        let weak = Rc::downgrade(self);
        let open_menu = gtk::CallbackAction::new(move |_, _| {
            weak.upgrade().is_some_and(|s| s.popup_focused()).into()
        });
        for trigger in ["Menu", "<Shift>F10"] {
            keys.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(open_menu.clone()),
            ));
        }
        let weak = Rc::downgrade(self);
        keys.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("F2"),
            Some(gtk::CallbackAction::new(move |_, _| {
                let Some(s) = weak.upgrade() else {
                    return glib::Propagation::Proceed;
                };
                let Some(idx) = s.focused_index() else {
                    return glib::Propagation::Proceed;
                };
                let is_bookmark = matches!(
                    s.places.borrow().get(idx).map(|p| &p.kind),
                    Some(Kind::Bookmark(..))
                );
                if is_bookmark {
                    s.rename_bookmark(idx);
                }
                is_bookmark.into()
            })),
        ));
        self.list.add_controller(keys);

        // Actions for the context menu.
        let group = gio::SimpleActionGroup::new();
        let add = |name: &str, f: fn(&Rc<Sidebar>, usize)| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(s) = weak.upgrade() {
                    if let Some(i) = s.menu_target.get() {
                        f(&s, i);
                    }
                }
            });
            group.add_action(&a);
        };
        add("open", |s, i| s.activate(i, false));
        add("open-tab", |s, i| s.activate(i, true));
        add("open-window", |s, i| {
            let place = s.places.borrow().get(i).cloned();
            if let (Some(w), Some(f)) = (s.win.upgrade(), place.and_then(|p| p.file())) {
                Window::new(&w.shared, Some(f), vec![]).window.present();
            }
        });
        add("rename", |s, i| s.rename_bookmark(i));
        add("remove", |s, i| s.remove_bookmark(i));
        add("eject", |s, i| s.eject(i));
        add("properties", |s, i| {
            let place = s.places.borrow().get(i).cloned();
            if let (Some(w), Some(f)) = (s.win.upgrade(), place.and_then(|p| p.file())) {
                super::properties::show(&w, vec![f]);
            }
        });
        add("empty-trash", |s, _| {
            if let Some(w) = s.win.upgrade() {
                gtk::prelude::WidgetExt::activate_action(&w.window, "win.empty-trash", None).ok();
            }
        });
        // On the scrolled window: the context menu is its child, not the
        // list's (see `new`), and must find these actions too.
        self.widget.insert_action_group("sidebar", Some(&group));

        // Device changes.
        let mut handlers = Vec::new();
        macro_rules! watch {
            ($connect:ident) => {{
                let weak = Rc::downgrade(self);
                handlers.push(self.monitor.$connect(move |_, _| {
                    if let Some(s) = weak.upgrade() {
                        s.rebuild();
                    }
                }));
            }};
        }
        watch!(connect_mount_added);
        watch!(connect_mount_removed);
        watch!(connect_mount_changed);
        watch!(connect_volume_added);
        watch!(connect_volume_removed);
        watch!(connect_volume_changed);
        *self.handlers.borrow_mut() = handlers;

        // Bookmarks file and trash changes.
        let mut monitors = Vec::new();
        if let Ok(m) = gio::File::for_path(bookmarks::file_path())
            .monitor_file(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
        {
            let weak = Rc::downgrade(self);
            m.connect_changed(move |_, _, _, _| {
                if let Some(s) = weak.upgrade() {
                    s.rebuild();
                }
            });
            monitors.push(m);
        }
        if let Ok(m) = gio::File::for_uri("trash:///")
            .monitor_directory(gio::FileMonitorFlags::NONE, None::<&gio::Cancellable>)
        {
            let weak = Rc::downgrade(self);
            m.connect_changed(move |_, _, _, _| {
                if let Some(s) = weak.upgrade() {
                    s.refresh_trash();
                }
            });
            monitors.push(m);
        }
        *self.file_monitors.borrow_mut() = monitors;
    }

    /// Disconnects global signal handlers (window closing).
    pub fn shutdown(&self) {
        self.closed.set(true);
        for h in self.handlers.borrow_mut().drain(..) {
            self.monitor.disconnect(h);
        }
        for m in self.file_monitors.borrow_mut().drain(..) {
            m.cancel();
        }
    }

    fn refresh_trash(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let full = gio::File::for_uri("trash:///")
                .query_info_future(
                    "trash::item-count",
                    gio::FileQueryInfoFlags::NONE,
                    glib::Priority::LOW,
                )
                .await
                .map(|i| i.attribute_uint32("trash::item-count") > 0)
                .unwrap_or(false);
            if let Some(s) = weak.upgrade() {
                if s.trash_full.replace(full) != full {
                    s.rebuild();
                }
            }
        });
    }

    fn collect(&self) -> Vec<Place> {
        let mut v = fixed_places(self.trash_full.get());
        let network = v.split_off(v.len() - 2);
        v.extend(bookmark_places());
        for m in self.monitor.mounts() {
            if m.is_shadowed() {
                continue;
            }
            v.push(Place {
                label: m.name().to_string(),
                icon: m.symbolic_icon(),
                kind: Kind::Mount(m),
                section: 3,
            });
        }
        for vol in self.monitor.volumes() {
            if vol.get_mount().is_some() {
                continue;
            }
            v.push(Place {
                label: vol.name().to_string(),
                icon: vol.symbolic_icon(),
                kind: Kind::Volume(vol),
                section: 3,
            });
        }
        v.extend(network);
        v
    }

    pub fn rebuild(self: &Rc<Self>) {
        if self.closed.get() {
            return;
        }
        // Keyboard focus stays on the same place (or moves to a neighbour
        // when it was removed) across the rebuild.
        let list = self.list.clone();
        helix_app_common::focus::rebuild_keeping_focus(&list, None, || self.rebuild_rows());
    }

    fn rebuild_rows(self: &Rc<Self>) {
        let places = self.collect();
        helix_app_common::clear_list_box(&self.list);
        for (i, p) in places.iter().enumerate() {
            let (row, b) = place_row(p);
            if let Kind::Mount(m) = &p.kind {
                if m.can_eject() || m.can_unmount() {
                    let (verb, spoken) = if m.can_eject() {
                        (tr("Eject"), trf!("Eject {name}", name = p.label))
                    } else {
                        (tr("Unmount"), trf!("Unmount {name}", name = p.label))
                    };
                    let btn = gtk::Button::builder()
                        .icon_name("media-eject-symbolic")
                        .tooltip_text(verb)
                        .valign(gtk::Align::Center)
                        .build();
                    helix_app_common::set_accessible_label(&btn, &spoken);
                    btn.add_css_class("flat");
                    btn.add_css_class("sidebar-eject");
                    let weak = Rc::downgrade(self);
                    btn.connect_clicked(move |_| {
                        if let Some(s) = weak.upgrade() {
                            s.eject(i);
                        }
                    });
                    b.append(&btn);
                }
            }
            let key = p
                .file()
                .map_or_else(|| p.label.clone(), |f| f.uri().to_string());
            helix_app_common::focus::set_key(&row, &key);
            if let (Some(w), Some(f)) = (self.win.upgrade(), p.file()) {
                if f.uri() != OTHER_LOCATIONS
                    && f.uri_scheme().as_deref() != Some("recent")
                    && f.uri_scheme().as_deref() != Some("starred")
                    && f.uri_scheme().as_deref() != Some("network")
                {
                    w.attach_drop(row.upcast_ref(), move || f.clone());
                }
            }
            self.list.append(&row);
        }
        *self.places.borrow_mut() = places;
        if let Some(t) = self.win.upgrade().and_then(|w| w.current_tab()) {
            self.highlight(&t.location());
        }
    }

    pub fn highlight(&self, loc: &gio::File) {
        let idx = self
            .places
            .borrow()
            .iter()
            .position(|p| p.file().is_some_and(|f| f.equal(loc)));
        match idx.and_then(|i| self.list.row_at_index(i as i32)) {
            Some(row) => self.list.select_row(Some(&row)),
            None => self.list.unselect_all(),
        }
    }

    fn activate(&self, idx: usize, new_tab: bool) {
        let Some(place) = self.places.borrow().get(idx).cloned() else {
            return;
        };
        let Some(w) = self.win.upgrade() else { return };
        match &place.kind {
            Kind::Volume(v) if v.get_mount().is_none() => w.mount_volume(v, new_tab),
            _ => {
                if let Some(f) = place.file() {
                    if new_tab {
                        w.open_tab(&f, vec![], true);
                    } else {
                        w.navigate(&f);
                    }
                }
            }
        }
        if let Some(t) = w.current_tab() {
            self.highlight(&t.location());
        }
    }

    fn popup(&self, idx: usize, x: f64, y: f64) {
        let Some(place) = self.places.borrow().get(idx).cloned() else {
            return;
        };
        self.menu_target.set(Some(idx));
        let menu = gio::Menu::new();
        let sec = gio::Menu::new();
        sec.append(Some(tr("_Open")), Some("sidebar.open"));
        sec.append(Some(tr("Open in New _Tab")), Some("sidebar.open-tab"));
        sec.append(Some(tr("Open in New _Window")), Some("sidebar.open-window"));
        menu.append_section(None, &sec);
        let sec = gio::Menu::new();
        match &place.kind {
            Kind::Bookmark(..) => {
                sec.append(Some(tr("Rena_me…")), Some("sidebar.rename"));
                sec.append(Some(tr("_Remove from Bookmarks")), Some("sidebar.remove"));
            }
            Kind::Mount(m) if m.can_eject() => {
                sec.append(Some(tr("_Eject")), Some("sidebar.eject"))
            }
            Kind::Mount(m) if m.can_unmount() => {
                sec.append(Some(tr("_Unmount")), Some("sidebar.eject"))
            }
            Kind::Location(f) if f.uri_scheme().as_deref() == Some("trash") => {
                sec.append(Some(tr("_Empty Trash…")), Some("sidebar.empty-trash"))
            }
            _ => {}
        }
        if place.file().is_some_and(|f| f.uri() != OTHER_LOCATIONS) {
            sec.append(Some(tr("_Properties")), Some("sidebar.properties"));
        }
        menu.append_section(None, &sec);
        self.popover.set_menu_model(Some(&menu));
        // (x, y) is in list coordinates; the popover hangs off `widget`.
        let at = self
            .list
            .compute_point(&self.widget, &graphene::Point::new(x as f32, y as f32))
            .unwrap_or(graphene::Point::new(x as f32, y as f32));
        self.popover.set_pointing_to(Some(&gdk::Rectangle::new(
            at.x() as i32,
            at.y() as i32,
            1,
            1,
        )));
        self.popover.popup();
    }

    /// The index of the row holding keyboard focus (or else the selected
    /// row).
    fn focused_index(&self) -> Option<usize> {
        self.list
            .focus_child()
            .and_downcast::<gtk::ListBoxRow>()
            .or_else(|| self.list.selected_row())
            .map(|r| r.index() as usize)
    }

    /// Opens the menu of the focused row, below it. Returns whether a menu
    /// was shown.
    fn popup_focused(&self) -> bool {
        let Some(idx) = self.focused_index() else {
            return false;
        };
        let Some(row) = self.list.row_at_index(idx as i32) else {
            return false;
        };
        let (x, y) = row
            .compute_bounds(&self.list)
            .map(|b| (f64::from(b.x() + 24.0), f64::from(b.y() + b.height())))
            .unwrap_or((24.0, 0.0));
        self.popup(idx, x, y);
        true
    }

    fn eject(&self, idx: usize) {
        let Some(place) = self.places.borrow().get(idx).cloned() else {
            return;
        };
        if let (Kind::Mount(m), Some(w)) = (&place.kind, self.win.upgrade()) {
            w.unmount(m);
        }
    }

    fn remove_bookmark(&self, idx: usize) {
        let Some(Kind::Bookmark(bi, _)) = self.places.borrow().get(idx).map(|p| p.kind.clone())
        else {
            return;
        };
        let mut list = bookmarks::load();
        if bi < list.len() {
            list.remove(bi);
            if let Err(e) = bookmarks::save(&list) {
                if let Some(w) = self.win.upgrade() {
                    w.toast(&trf!(
                        "Could not save bookmarks: {e}",
                        e = helix_app_common::errors::io_detail(&e)
                    ));
                }
            }
        }
    }

    fn rename_bookmark(self: &Rc<Self>, idx: usize) {
        let Some(place) = self.places.borrow().get(idx).cloned() else {
            return;
        };
        let Kind::Bookmark(bi, _) = place.kind else {
            return;
        };
        let Some(w) = self.win.upgrade() else { return };
        let entry = adw::EntryRow::builder()
            .title(tr("Bookmark _Name"))
            .use_underline(true)
            .text(&place.label)
            .activates_default(true)
            .build();
        let list_box = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .css_classes(["boxed-list"])
            .build();
        list_box.append(&entry);
        let dialog = adw::AlertDialog::builder()
            .heading(tr("Rename Bookmark"))
            .extra_child(&list_box)
            .default_response("rename")
            .close_response("cancel")
            .build();
        dialog.add_responses(&[("cancel", tr("_Cancel")), ("rename", tr("_Rename"))]);
        dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
        let win = self.win.clone();
        let e = entry.clone();
        dialog.connect_response(Some("rename"), move |_, _| {
            let mut list = bookmarks::load();
            if let Some(b) = list.get_mut(bi) {
                let text = e.text().trim().to_string();
                b.label = (!text.is_empty()).then_some(text);
                if let Err(err) = bookmarks::save(&list) {
                    if let Some(w) = win.upgrade() {
                        w.toast(&trf!(
                            "Could not save bookmarks: {e}",
                            e = helix_app_common::errors::io_detail(&err)
                        ));
                    }
                }
            }
        });
        dialog.present(Some(&w.window));
        glib::idle_add_local_once(move || {
            entry.grab_focus();
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use helix_app_common::testing::{headless_gtk, label_mismatch};

    /// The rows are named by their labels on AT-SPI, not by their
    /// tooltips (a path or a URI such as "trash:///").
    #[test]
    fn rows_are_named_by_their_labels() {
        let Some(_display) = headless_gtk() else {
            return;
        };
        let mut places = fixed_places(false);
        places.push(bookmark_place(
            0,
            &bookmarks::Bookmark {
                uri: "sftp://example.org/srv/share".into(),
                label: Some("Shared".into()),
            },
        ));
        let labels: Vec<&str> = places.iter().map(|p| p.label.as_str()).collect();
        for expected in [
            "Home",
            "Recent",
            "Trash",
            "Shared",
            "Network",
            "Other Locations",
        ] {
            assert!(labels.contains(&expected), "{expected} in {labels:?}");
        }
        for p in &places {
            let (row, _) = place_row(p);
            let tooltip = row.tooltip_text().unwrap_or_default();
            assert!(!tooltip.is_empty(), "{}", p.label);
            assert_eq!(label_mismatch(&row, &p.label), None, "{}", p.label);
            assert!(label_mismatch(&row, &tooltip).is_some(), "{tooltip}");
        }
    }
}
