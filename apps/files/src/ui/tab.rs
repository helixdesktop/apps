//! One tab: a folder listing (grid or list), search results, or the
//! "Other Locations" page.

use adw::prelude::*;
use gtk::{gdk, gio, glib, graphene};
use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use super::starred::STARRED;
use super::{Shared, ViewSettings, Window, FILE_ATTRS, GRID_ZOOM, LIST_ZOOM, OTHER_LOCATIONS};
use crate::history::History;
use crate::search;
use crate::sorting::{self, SortKey};
use helix_app_common::i18n::tr;
use helix_app_common::trf;

const ITEM_KEY: &str = "helix-files-item";
const STAR_URI_KEY: &str = "helix-files-star-uri";
const INVALID: u32 = u32::MAX;

/// The `GFile` a listed `GFileInfo` describes.
pub fn info_file(info: &gio::FileInfo) -> Option<gio::File> {
    info.attribute_object("standard::file")
        .and_downcast::<gio::File>()
}

/// What a missing thumbnail of `file` (described by `info`) is made from:
/// a file of a type a thumbnailer handles.
fn thumbnail_source(
    info: &gio::FileInfo,
    file: &gio::File,
    mtime: u64,
) -> Option<super::thumbnailer::Source> {
    let content_type = info
        .content_type()
        .or_else(|| info.attribute_string("standard::fast-content-type"))?;
    let mime = gio::content_type_get_mime_type(&content_type)?.to_string();
    if !super::thumbnailer::can_make(&mime) {
        return None;
    }
    Some(super::thumbnailer::Source {
        // A remote file's (FUSE) path is found on the worker thread.
        path: super::window::local_path(file),
        uri: file.uri().to_string(),
        mime,
        mtime,
    })
}

/// The file to open for an item: its `standard::target-uri` if any.
pub fn info_target(info: &gio::FileInfo) -> Option<gio::File> {
    match info.attribute_string("standard::target-uri") {
        Some(uri) if !uri.is_empty() => Some(gio::File::for_uri(&uri)),
        _ => info_file(info),
    }
}

pub fn info_is_dir(info: &gio::FileInfo) -> bool {
    match info.file_type() {
        gio::FileType::Directory | gio::FileType::Mountable => true,
        gio::FileType::Shortcut => info.attribute_string("standard::target-uri").is_some(),
        _ => false,
    }
}

pub fn content_type(info: &gio::FileInfo) -> String {
    info.content_type()
        .or_else(|| info.attribute_string("standard::fast-content-type"))
        .map(|s| s.to_string())
        .unwrap_or_default()
}

pub fn type_description(info: &gio::FileInfo) -> String {
    let ct = content_type(info);
    if ct.is_empty() {
        if info_is_dir(info) {
            return tr("Folder").into();
        }
        return tr("Unknown").into();
    }
    gio::content_type_get_description(&ct).to_string()
}

fn item_entry_cmp(a: &gio::FileInfo, b: &gio::FileInfo, s: &ViewSettings) -> std::cmp::Ordering {
    let an = a.display_name();
    let bn = b.display_name();
    let (at, bt) = if s.sort_key == SortKey::Type {
        (type_description(a), type_description(b))
    } else {
        Default::default()
    };
    let ea = sorting::Entry {
        name: an.as_str(),
        is_dir: info_is_dir(a),
        size: a.size().max(0) as u64,
        type_desc: &at,
        mtime: a.attribute_uint64("time::modified") as i64,
    };
    let eb = sorting::Entry {
        name: bn.as_str(),
        is_dir: info_is_dir(b),
        size: b.size().max(0) as u64,
        type_desc: &bt,
        mtime: b.attribute_uint64("time::modified") as i64,
    };
    sorting::compare(&ea, &eb, s.sort_key, s.reversed, s.dirs_first)
}

/// Stores the list item on its widget so pointer positions can be mapped to
/// item positions.
fn tag_widget(widget: &gtk::Widget, item: &glib::Object) {
    unsafe { widget.set_data(ITEM_KEY, item.downgrade()) };
}

fn tagged_position(widget: &gtk::Widget) -> Option<u32> {
    let weak = unsafe { widget.data::<glib::WeakRef<glib::Object>>(ITEM_KEY) }?;
    let obj = unsafe { weak.as_ref() }.upgrade()?;
    let pos = obj.property::<u32>("position");
    (pos != INVALID).then_some(pos)
}

/// A row's star mark (hidden until its file is starred), known to the
/// tab so that it follows changes.
fn star_mark(tab: &Weak<Tab>, class: &str) -> gtk::Image {
    let star = gtk::Image::from_icon_name("starred-symbolic");
    star.add_css_class(class);
    star.set_halign(gtk::Align::End);
    // In the icon's corner on the grid, centred after a list row's name.
    star.set_valign(if class == "grid-star" {
        gtk::Align::Start
    } else {
        gtk::Align::Center
    });
    star.set_visible(false);
    helix_app_common::set_accessible_label(&star, tr("Starred"));
    if let Some(t) = tab.upgrade() {
        t.star_marks.borrow_mut().push(star.downgrade());
    }
    star
}

/// The file shown by a widget tagged with [`tag_widget`], while its list
/// item is bound.
fn tagged_file(widget: &gtk::Widget) -> Option<gio::File> {
    let weak = unsafe { widget.data::<glib::WeakRef<glib::Object>>(ITEM_KEY) }?;
    let obj = unsafe { weak.as_ref() }.upgrade()?;
    let info = obj
        .property::<Option<glib::Object>>("item")
        .and_downcast::<gio::FileInfo>()?;
    info_file(&info)
}

/// An item widget that can show the "cut" dimming, known to the tab so
/// that the dimming follows the clipboard without new widgets.
fn track_cut(tab: &Weak<Tab>, widget: &gtk::Widget) {
    if let Some(t) = tab.upgrade() {
        let mut marks = t.cut_marks.borrow_mut();
        // Widgets of replaced factories are gone.
        marks.retain(|w| w.upgrade().is_some());
        marks.push(widget.downgrade());
    }
}

/// Shows an item as cut (dimmed) and says so to assistive technology.
fn mark_cut(item: &impl IsA<gtk::Widget>, cut: bool) {
    let item = item.as_ref();
    if cut {
        item.add_css_class("cut-item");
        item.update_property(&[gtk::accessible::Property::Description(tr("Cut"))]);
    } else if item.has_css_class("cut-item") {
        item.remove_css_class("cut-item");
        item.reset_property(gtk::AccessibleProperty::Description);
    }
}

pub struct Tab {
    pub shared: Rc<Shared>,
    win: RefCell<Weak<Window>>,
    pub root: gtk::Overlay,
    stack: gtk::Stack,
    status: adw::StatusPage,
    spinner: gtk::Spinner,
    other: adw::Bin,
    location: RefCell<gio::File>,
    pub history: RefCell<History>,
    dir_list: gtk::DirectoryList,
    search_store: gio::ListStore,
    /// Starred: the starred files, read when shown and when the stars
    /// change (`starred_serial` drops late readings).
    starred_store: gio::ListStore,
    starred_serial: Cell<u64>,
    starred_loading: Cell<bool>,
    /// The list's star marks (each knows its row's URI).
    star_marks: RefCell<Vec<glib::WeakRef<gtk::Image>>>,
    /// The item widgets that show the "cut" dimming (see `track_cut`).
    cut_marks: RefCell<Vec<glib::WeakRef<gtk::Widget>>>,
    filter: gtk::CustomFilter,
    filter_model: gtk::FilterListModel,
    sorter: gtk::CustomSorter,
    pub selection: gtk::MultiSelection,
    grid: gtk::GridView,
    list: gtk::ColumnView,
    columns: Vec<(String, gtk::ColumnViewColumn)>,
    settings: Rc<RefCell<ViewSettings>>,
    pending_select: RefCell<Vec<gio::File>>,
    search_cancel: RefCell<Option<gio::Cancellable>>,
    search_text: RefCell<String>,
    searching: Cell<bool>,
    syncing_sort: Cell<bool>,
    popover: gtk::PopoverMenu,
    mount_attempted: Cell<bool>,
    /// Selected files, to keep the selection when the listing replaces
    /// items (file changed on disk, re-sorting).
    selected_uris: RefCell<std::collections::HashSet<String>>,
    restoring: Cell<bool>,
    /// The next pending match replaces the selection (later ones add).
    pending_fresh: Cell<bool>,
    /// Whether "Unable to Open Folder" is showing (announced once).
    error_shown: Cell<bool>,
}

impl Tab {
    pub fn new(shared: &Rc<Shared>, win: Weak<Window>, location: &gio::File) -> Rc<Tab> {
        let settings = Rc::new(RefCell::new(shared.view_settings()));

        // Monitored from the start (GTK's default). Turning monitoring on
        // after a folder loaded makes GTK drop every item and load the
        // folder again, so a big folder showed, went blank and refilled.
        // A location that cannot be monitored just gets no monitor.
        let dir_list = gtk::DirectoryList::new(Some(FILE_ATTRS), None::<&gio::File>);
        dir_list.set_monitored(true);
        dir_list.set_io_priority(glib::Priority::DEFAULT_IDLE);
        let search_store = gio::ListStore::new::<gio::FileInfo>();
        let starred_store = gio::ListStore::new::<gio::FileInfo>();

        let s = settings.clone();
        let filter = gtk::CustomFilter::new(move |obj| {
            let Some(info) = obj.downcast_ref::<gio::FileInfo>() else {
                return false;
            };
            s.borrow().show_hidden
                || !(info.boolean("standard::is-hidden") || info.boolean("standard::is-backup"))
        });
        let filter_model = gtk::FilterListModel::new(Some(dir_list.clone()), Some(filter.clone()));
        filter_model.set_incremental(false);
        let s = settings.clone();
        let sorter = gtk::CustomSorter::new(move |a, b| {
            match (
                a.downcast_ref::<gio::FileInfo>(),
                b.downcast_ref::<gio::FileInfo>(),
            ) {
                (Some(a), Some(b)) => item_entry_cmp(a, b, &s.borrow()).into(),
                _ => gtk::Ordering::Equal,
            }
        });
        let sort_model = gtk::SortListModel::new(Some(filter_model.clone()), Some(sorter.clone()));
        let selection = gtk::MultiSelection::new(Some(sort_model));

        let grid = gtk::GridView::new(Some(selection.clone()), None::<gtk::ListItemFactory>);
        grid.set_enable_rubberband(true);
        grid.set_max_columns(64);
        grid.set_min_columns(1);
        let list = gtk::ColumnView::new(Some(selection.clone()));
        list.set_enable_rubberband(true);
        list.set_show_column_separators(false);
        list.add_css_class("data-table");
        let mut columns = Vec::new();
        for (id, title, key) in [
            ("name", tr("Name"), SortKey::Name),
            ("size", tr("Size"), SortKey::Size),
            ("type", tr("Type"), SortKey::Type),
            ("date_modified", tr("Modified"), SortKey::Modified),
        ] {
            let col = gtk::ColumnViewColumn::new(Some(title), None::<gtk::ListItemFactory>);
            col.set_id(Some(id));
            col.set_resizable(true);
            if id == "name" {
                col.set_expand(true);
            } else {
                col.set_fixed_width(if id == "date_modified" { 150 } else { 110 });
            }
            // Column sorters only identify the column; real sorting is ours.
            let key_sorter = gtk::CustomSorter::new(move |_, _| {
                let _ = key;
                gtk::Ordering::Equal
            });
            col.set_sorter(Some(&key_sorter));
            list.append_column(&col);
            columns.push((id.to_string(), col));
        }

        let grid_scroll = gtk::ScrolledWindow::builder()
            .child(&grid)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        let list_scroll = gtk::ScrolledWindow::builder().child(&list).build();
        let other = adw::Bin::new();
        let stack = gtk::Stack::new();
        stack.add_named(&grid_scroll, Some("grid"));
        stack.add_named(&list_scroll, Some("list"));
        stack.add_named(&other, Some("other"));

        let root = gtk::Overlay::new();
        root.set_child(Some(&stack));
        root.set_vexpand(true);
        root.set_hexpand(true);
        let status = adw::StatusPage::new();
        status.set_visible(false);
        status.set_can_target(false);
        // Shown (and the page made clickable) only for a folder error.
        let retry = gtk::Button::builder()
            .label(tr("_Try Again"))
            .use_underline(true)
            .action_name("win.reload")
            .halign(gtk::Align::Center)
            .visible(false)
            .build();
        retry.add_css_class("pill");
        status.set_child(Some(&retry));
        root.add_overlay(&status);
        let spinner = gtk::Spinner::new();
        helix_app_common::set_accessible_label(&spinner, tr("Loading folder"));
        spinner.set_halign(gtk::Align::End);
        spinner.set_valign(gtk::Align::End);
        spinner.set_margin_end(18);
        spinner.set_margin_bottom(18);
        spinner.set_size_request(24, 24);
        spinner.set_can_target(false);
        root.add_overlay(&spinner);

        let popover = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
        popover.set_parent(&root);
        popover.set_has_arrow(false);
        popover.set_halign(gtk::Align::Start);
        let p = popover.clone();
        root.connect_destroy(move |_| p.unparent());

        let tab = Rc::new(Tab {
            shared: shared.clone(),
            win: RefCell::new(win),
            root,
            stack,
            status,
            spinner,
            other,
            location: RefCell::new(location.clone()),
            history: RefCell::default(),
            dir_list,
            search_store,
            starred_store,
            starred_serial: Cell::new(0),
            starred_loading: Cell::new(false),
            star_marks: RefCell::default(),
            cut_marks: RefCell::default(),
            filter,
            filter_model,
            sorter,
            selection,
            grid,
            list,
            columns,
            settings,
            pending_select: RefCell::default(),
            search_cancel: RefCell::default(),
            search_text: RefCell::default(),
            searching: Cell::new(false),
            syncing_sort: Cell::new(false),
            popover,
            mount_attempted: Cell::new(false),
            selected_uris: RefCell::default(),
            restoring: Cell::new(false),
            pending_fresh: Cell::new(false),
            error_shown: Cell::new(false),
        });
        tab.connect_signals();
        tab.rebuild_factories();
        tab.apply_view_settings(None);
        tab
    }

    fn window(&self) -> Option<Rc<Window>> {
        self.win.borrow().upgrade()
    }

    fn notify_window(self: &Rc<Self>) {
        if let Some(w) = self.window() {
            w.tab_changed(self);
        }
    }

    pub fn location(&self) -> gio::File {
        self.location.borrow().clone()
    }

    pub fn is_other_locations(&self) -> bool {
        self.location.borrow().uri() == OTHER_LOCATIONS
    }

    pub fn is_trash(&self) -> bool {
        self.location.borrow().uri_scheme().as_deref() == Some("trash")
    }

    /// Whether the tab shows Starred.
    pub fn is_starred(&self) -> bool {
        self.location.borrow().uri() == STARRED
    }

    pub fn is_loading(&self) -> bool {
        self.dir_list.is_loading() || self.searching.get() || self.starred_loading.get()
    }

    pub fn is_searching(&self) -> bool {
        !self.search_text.borrow().is_empty()
    }

    // ----- signals -----

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.dir_list.connect_loading_notify(move |_| {
            if let Some(t) = weak.upgrade() {
                t.update_status();
                if !t.dir_list.is_loading() {
                    t.select_pending();
                }
                t.notify_window();
            }
        });
        let weak = Rc::downgrade(self);
        self.dir_list.connect_error_notify(move |dl| {
            let Some(t) = weak.upgrade() else { return };
            if let Some(err) = dl.error() {
                if err.matches(gio::IOErrorEnum::NotMounted) && !t.mount_attempted.get() {
                    t.mount_attempted.set(true);
                    t.mount_and_reload();
                    return;
                }
            }
            t.update_status();
        });
        let weak = Rc::downgrade(self);
        self.selection
            .connect_items_changed(move |_, pos, _, added| {
                if let Some(t) = weak.upgrade() {
                    t.restore_selection(pos, added);
                    if added > 0 && !t.pending_select.borrow().is_empty() {
                        t.select_pending();
                    }
                    t.update_status();
                    t.notify_window();
                }
            });
        let weak = Rc::downgrade(self);
        self.selection.connect_selection_changed(move |_, _, _| {
            if let Some(t) = weak.upgrade() {
                if !t.restoring.get() {
                    *t.selected_uris.borrow_mut() = t
                        .selected_files()
                        .iter()
                        .map(|f| f.uri().to_string())
                        .collect();
                }
                t.notify_window();
            }
        });

        // Activation.
        let weak = Rc::downgrade(self);
        self.grid.connect_activate(move |_, pos| {
            if let Some(t) = weak.upgrade() {
                t.activate(pos);
            }
        });
        let weak = Rc::downgrade(self);
        self.list.connect_activate(move |_, pos| {
            if let Some(t) = weak.upgrade() {
                t.activate(pos);
            }
        });

        // Column header sorting → our settings.
        if let Some(sorter) = self.list.sorter() {
            let weak = Rc::downgrade(self);
            sorter.connect_changed(move |sorter, _| {
                let Some(t) = weak.upgrade() else { return };
                if t.syncing_sort.get() {
                    return;
                }
                let Some(cvs) = sorter.downcast_ref::<gtk::ColumnViewSorter>() else {
                    return;
                };
                let Some(col) = cvs.primary_sort_column() else {
                    return;
                };
                let key = match col.id().as_deref() {
                    Some("size") => SortKey::Size,
                    Some("type") => SortKey::Type,
                    Some("date_modified") => SortKey::Modified,
                    _ => SortKey::Name,
                };
                let reversed = cvs.primary_sort_order() == gtk::SortType::Descending;
                let p = &t.shared.prefs;
                p.set_enum_nick("default-sort-order", key.nick());
                p.set("default-sort-in-reverse-order", reversed);
                t.shared.settings_changed();
            });
        }

        let views: [gtk::Widget; 2] = [self.grid.clone().upcast(), self.list.clone().upcast()];
        for view in views {
            self.attach_pointer(&view);
            self.attach_dnd(&view);
        }
    }

    fn attach_pointer(self: &Rc<Self>, view: &gtk::Widget) {
        let click = gtk::GestureClick::new();
        click.set_button(0);
        // Capture phase so item widgets cannot swallow secondary clicks.
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        let v = view.clone();
        click.connect_pressed(move |g, n, x, y| {
            let Some(t) = weak.upgrade() else { return };
            let button = g.current_button();
            let pos = t.position_at(&v, x, y);
            match button {
                gdk::BUTTON_SECONDARY => {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    match pos {
                        Some(p) => {
                            if !t.selection.is_selected(p) {
                                t.selection.select_item(p, true);
                            }
                            t.popup_menu(&v, x, y, true);
                        }
                        None => {
                            t.selection.unselect_all();
                            t.popup_menu(&v, x, y, false);
                        }
                    }
                }
                gdk::BUTTON_MIDDLE => {
                    g.set_state(gtk::EventSequenceState::Claimed);
                    if let Some(p) = pos {
                        if let Some(info) = t.info_at(p) {
                            if info_is_dir(&info) {
                                if let (Some(w), Some(f)) = (t.window(), info_target(&info)) {
                                    w.open_tab(&f, vec![], false);
                                }
                            }
                        }
                    }
                }
                gdk::BUTTON_PRIMARY if pos.is_none() && n == 1 => {
                    // Click on empty space clears the selection.
                    t.selection.unselect_all();
                }
                _ => {}
            }
        });
        view.add_controller(click);

        // Touch: long-press opens the item menu (selecting the item) or,
        // on empty space, the background menu.
        let long_press = gtk::GestureLongPress::new();
        long_press.set_touch_only(true);
        let weak = Rc::downgrade(self);
        let v = view.clone();
        long_press.connect_pressed(move |g, x, y| {
            let Some(t) = weak.upgrade() else { return };
            g.set_state(gtk::EventSequenceState::Claimed);
            match t.position_at(&v, x, y) {
                Some(p) => {
                    if !t.selection.is_selected(p) {
                        t.selection.select_item(p, true);
                    }
                    t.popup_menu(&v, x, y, true);
                }
                None => {
                    t.selection.unselect_all();
                    t.popup_menu(&v, x, y, false);
                }
            }
        });
        view.add_controller(long_press);
    }

    fn position_at(&self, view: &gtk::Widget, x: f64, y: f64) -> Option<u32> {
        let mut w = view.pick(x, y, gtk::PickFlags::DEFAULT);
        while let Some(cur) = w {
            if let Some(pos) = tagged_position(&cur) {
                return Some(pos);
            }
            if &cur == view {
                break;
            }
            w = cur.parent();
        }
        None
    }

    fn attach_dnd(self: &Rc<Self>, view: &gtk::Widget) {
        let source = gtk::DragSource::new();
        source.set_actions(gdk::DragAction::COPY | gdk::DragAction::MOVE | gdk::DragAction::LINK);
        let weak = Rc::downgrade(self);
        let v = view.clone();
        source.connect_prepare(move |src, x, y| {
            let t = weak.upgrade()?;
            let pos = t.position_at(&v, x, y)?;
            if !t.selection.is_selected(pos) {
                t.selection.select_item(pos, true);
            }
            let files = t.selected_files();
            if files.is_empty() {
                return None;
            }
            if let Some(icon) = t.info_at(pos).and_then(|i| i.icon()) {
                let theme = gtk::IconTheme::for_display(&v.display());
                let paintable = theme.lookup_by_gicon(
                    &icon,
                    48,
                    1,
                    gtk::TextDirection::None,
                    gtk::IconLookupFlags::empty(),
                );
                src.set_icon(Some(&paintable), 16, 16);
            }
            Some(gdk::ContentProvider::for_value(
                &gdk::FileList::from_array(&files).to_value(),
            ))
        });
        view.add_controller(source);

        let target = gtk::DropTarget::new(
            gdk::FileList::static_type(),
            gdk::DragAction::COPY | gdk::DragAction::MOVE,
        );
        // Load the file list while hovering so the action can depend on it.
        target.set_preload(true);
        let weak = Rc::downgrade(self);
        let v = view.clone();
        target.connect_motion(move |target, x, y| {
            let Some(t) = weak.upgrade() else {
                return gdk::DragAction::empty();
            };
            if t.is_other_locations() || t.is_searching() || t.is_starred() {
                return gdk::DragAction::empty();
            }
            let dest = t.drop_destination(&v, x, y);
            super::window::preferred_drop_action(target, None, &dest)
        });
        let weak = Rc::downgrade(self);
        let v = view.clone();
        target.connect_drop(move |target, value, x, y| {
            let Some(t) = weak.upgrade() else {
                return false;
            };
            let Ok(list) = value.get::<gdk::FileList>() else {
                return false;
            };
            let files = list.files();
            let dest = t.drop_destination(&v, x, y);
            let Some(w) = t.window() else { return false };
            let action = super::window::preferred_drop_action(target, Some(&files), &dest);
            w.drop_files(files, &dest, action)
        });
        view.add_controller(target);
    }

    /// The folder under the pointer, or the current folder.
    fn drop_destination(&self, view: &gtk::Widget, x: f64, y: f64) -> gio::File {
        if let Some(info) = self.position_at(view, x, y).and_then(|p| self.info_at(p)) {
            if info_is_dir(&info) {
                if let Some(f) = info_target(&info) {
                    return f;
                }
            }
        }
        self.location()
    }

    // ----- factories -----

    pub fn rebuild_factories(self: &Rc<Self>) {
        self.grid.set_factory(Some(&self.grid_factory()));
        for (id, col) in &self.columns {
            col.set_factory(Some(&self.column_factory(id)));
        }
    }

    fn is_cut(&self, file: &gio::File) -> bool {
        self.shared
            .clipboard
            .borrow()
            .as_ref()
            .is_some_and(|c| c.cut && c.files.iter().any(|f| f.equal(file)))
    }

    /// The clipboard changed: the bound items whose cut state changed are
    /// dimmed or undimmed in place. (New factories would make GTK
    /// recreate every item widget, and the folder flashed as if reloaded.)
    pub fn clipboard_changed(&self) {
        self.cut_marks.borrow_mut().retain(|w| match w.upgrade() {
            Some(widget) => {
                let cut = tagged_file(&widget).is_some_and(|f| self.is_cut(&f));
                if widget.has_css_class("cut-item") != cut {
                    mark_cut(&widget, cut);
                }
                true
            }
            None => false,
        });
    }

    fn thumbnail_allowed(&self, file: &gio::File) -> bool {
        match self.settings.borrow().thumbnails.as_str() {
            "always" => true,
            "never" => false,
            _ => file.is_native(),
        }
    }

    fn grid_factory(self: &Rc<Self>) -> gtk::SignalListItemFactory {
        let f = gtk::SignalListItemFactory::new();
        let size = GRID_ZOOM[self.settings.borrow().grid_zoom].1;
        let weak = Rc::downgrade(self);
        f.connect_setup(move |_, obj| {
            let item = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
            let b = gtk::Box::new(gtk::Orientation::Vertical, 4);
            b.add_css_class("grid-item");
            let img = gtk::Image::new();
            img.set_pixel_size(size);
            img.set_size_request(size, size);
            let label = gtk::Label::builder()
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .lines(3)
                .ellipsize(gtk::pango::EllipsizeMode::End)
                .justify(gtk::Justification::Center)
                .max_width_chars(if size >= 128 { 18 } else { 12 })
                .width_chars(if size >= 128 { 14 } else { 9 })
                .build();
            // Starred files are marked in the icon's corner.
            let icon = gtk::Overlay::new();
            icon.set_child(Some(&img));
            icon.add_overlay(&star_mark(&weak, "grid-star"));
            b.append(&icon);
            b.append(&label);
            item.set_child(Some(&b));
            tag_widget(b.upcast_ref(), obj);
            track_cut(&weak, b.upcast_ref());
        });
        let weak = Rc::downgrade(self);
        f.connect_bind(move |_, obj| {
            let Some(t) = weak.upgrade() else { return };
            let item = obj.downcast_ref::<gtk::ListItem>().expect("ListItem");
            let Some(info) = item.item().and_downcast::<gio::FileInfo>() else {
                return;
            };
            let Some(b) = item.child().and_downcast::<gtk::Box>() else {
                return;
            };
            let icon = b.first_child().and_downcast::<gtk::Overlay>().unwrap();
            let img = icon.child().and_downcast::<gtk::Image>().unwrap();
            if let Some(star) = icon.last_child().and_downcast::<gtk::Image>() {
                t.show_star(&star, info_file(&info).as_ref());
            }
            let label = b.last_child().and_downcast::<gtk::Label>().unwrap();
            label.set_text(&info.display_name());
            b.set_tooltip_text(Some(&info.display_name()));
            match info.icon() {
                Some(icon) => img.set_from_gicon(&icon),
                None => img.set_icon_name(Some("text-x-generic")),
            }
            let file = info_file(&info);
            mark_cut(&b, file.as_ref().is_some_and(|f| t.is_cut(f)));
            // Thumbnails: looked up (or made) for the tiles shown.
            if let Some(file) = file.filter(|f| t.thumbnail_allowed(f)) {
                if info.file_type() == gio::FileType::Regular {
                    if let Some(mtime) = info
                        .modification_date_time()
                        .and_then(|d| u64::try_from(d.to_unix()).ok())
                    {
                        let make = thumbnail_source(&info, &file, mtime);
                        let video = info
                            .content_type()
                            .or_else(|| info.attribute_string("standard::fast-content-type"))
                            .and_then(|t| gio::content_type_get_mime_type(&t))
                            .is_some_and(|m| super::filmstrip::is_video(&m));
                        super::thumbs::request(
                            file.uri().to_string(),
                            mtime,
                            video,
                            make,
                            &img,
                            item,
                            &info,
                        );
                    }
                }
            }
        });
        f
    }

    fn column_factory(self: &Rc<Self>, id: &str) -> gtk::SignalListItemFactory {
        let f = gtk::SignalListItemFactory::new();
        let icon_size = LIST_ZOOM[self.settings.borrow().list_zoom].1;
        let is_name = id == "name";
        let weak = Rc::downgrade(self);
        f.connect_setup(move |_, obj| {
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            b.set_hexpand(true);
            if is_name {
                let img = gtk::Image::new();
                img.set_pixel_size(icon_size);
                b.append(&img);
            }
            let label = gtk::Label::builder()
                .xalign(0.0)
                .ellipsize(gtk::pango::EllipsizeMode::Middle)
                .hexpand(true)
                .build();
            if !is_name {
                label.add_css_class("dim-label");
                label.add_css_class("numeric");
            }
            b.append(&label);
            if is_name {
                // Starred files are marked after their name.
                b.append(&star_mark(&weak, "dim-label"));
            }
            obj.set_property("child", &b);
            tag_widget(b.upcast_ref(), obj);
            if is_name {
                track_cut(&weak, b.upcast_ref());
            }
        });
        let weak = Rc::downgrade(self);
        let id = id.to_string();
        f.connect_bind(move |_, obj| {
            let Some(t) = weak.upgrade() else { return };
            let Some(info) = obj
                .property::<Option<glib::Object>>("item")
                .and_downcast::<gio::FileInfo>()
            else {
                return;
            };
            let Some(b) = obj
                .property::<Option<gtk::Widget>>("child")
                .and_downcast::<gtk::Box>()
            else {
                return;
            };
            let label = if id == "name" {
                b.first_child()
                    .and_then(|i| i.next_sibling())
                    .and_downcast::<gtk::Label>()
                    .unwrap()
            } else {
                b.last_child().and_downcast::<gtk::Label>().unwrap()
            };
            let text = match id.as_str() {
                "name" => {
                    let img = b.first_child().and_downcast::<gtk::Image>().unwrap();
                    match info.icon() {
                        Some(icon) => img.set_from_gicon(&icon),
                        None => img.set_icon_name(Some("text-x-generic")),
                    }
                    let file = info_file(&info);
                    mark_cut(&b, file.as_ref().is_some_and(|f| t.is_cut(f)));
                    if let Some(star) = b.last_child().and_downcast::<gtk::Image>() {
                        t.show_star(&star, file.as_ref());
                    }
                    info.display_name().to_string()
                }
                "size" => {
                    if info_is_dir(&info) {
                        "—".into()
                    } else {
                        crate::fmt::size(info.size().max(0) as u64)
                    }
                }
                "type" => type_description(&info),
                _ => {
                    let detailed = t.settings.borrow().detailed_dates;
                    let secs = info.attribute_uint64("time::modified") as i64;
                    match (
                        glib::DateTime::from_unix_local(secs),
                        glib::DateTime::now_local(),
                    ) {
                        (Ok(dt), Ok(now)) if secs > 0 => crate::fmt::date(&dt, &now, detailed),
                        _ => String::new(),
                    }
                }
            };
            label.set_text(&text);
        });
        f
    }

    // ----- settings -----

    /// Applies `self.shared` settings; `old` is the previous state (None on
    /// first application).
    fn apply_view_settings(self: &Rc<Self>, old: Option<ViewSettings>) {
        let s = self.settings.borrow().clone();
        self.grid.set_single_click_activate(s.single_click);
        self.list.set_single_click_activate(s.single_click);
        for (id, col) in &self.columns {
            col.set_visible(id == "name" || s.columns.iter().any(|c| c == id));
        }
        if !self.is_other_locations() {
            self.stack
                .set_visible_child_name(if s.list_view { "list" } else { "grid" });
        }
        if let Some(old) = &old {
            if old.show_hidden != s.show_hidden {
                self.filter.changed(if s.show_hidden {
                    gtk::FilterChange::LessStrict
                } else {
                    gtk::FilterChange::MoreStrict
                });
            }
            if (old.sort_key, old.reversed, old.dirs_first)
                != (s.sort_key, s.reversed, s.dirs_first)
            {
                self.sorter.changed(gtk::SorterChange::Different);
            }
            if (
                old.grid_zoom,
                old.list_zoom,
                old.detailed_dates,
                &old.thumbnails,
            ) != (s.grid_zoom, s.list_zoom, s.detailed_dates, &s.thumbnails)
            {
                self.rebuild_factories();
            }
        }
        // Reflect the sort in the column headers.
        let col = self.columns.iter().find(|(id, _)| {
            id == match s.sort_key {
                SortKey::Name => "name",
                SortKey::Size => "size",
                SortKey::Type => "type",
                SortKey::Modified => "date_modified",
            }
        });
        self.syncing_sort.set(true);
        self.list.sort_by_column(
            col.map(|(_, c)| c),
            if s.reversed {
                gtk::SortType::Descending
            } else {
                gtk::SortType::Ascending
            },
        );
        self.syncing_sort.set(false);
    }

    pub fn apply_settings(self: &Rc<Self>) {
        let new = self.shared.view_settings();
        let old = self.settings.replace(new.clone());
        if old != new {
            self.apply_view_settings(Some(old));
        }
    }

    pub fn focus_view(&self) {
        if self.settings.borrow().list_view {
            self.list.grab_focus();
        } else {
            self.grid.grab_focus();
        }
    }

    // ----- navigation -----

    /// Shows `file`. `record` adds it to history; `select` lists children
    /// to select once loaded.
    pub fn load(self: &Rc<Self>, file: &gio::File, record: bool, select: Vec<gio::File>) {
        self.cancel_search_worker();
        self.selected_uris.borrow_mut().clear();
        self.search_text.borrow_mut().clear();
        self.filter_model.set_model(Some(&self.dir_list));
        if record {
            self.history.borrow_mut().visit(&file.uri());
        }
        *self.location.borrow_mut() = file.clone();
        *self.pending_select.borrow_mut() = select;
        self.pending_fresh.set(true);
        self.mount_attempted.set(false);
        if file.uri() == OTHER_LOCATIONS {
            self.dir_list.set_file(None::<&gio::File>);
            self.build_other_locations();
            self.stack.set_visible_child_name("other");
        } else if file.uri() == STARRED {
            self.dir_list.set_file(None::<&gio::File>);
            self.starred_store.remove_all();
            self.filter_model.set_model(Some(&self.starred_store));
            self.fill_starred();
            let list_view = self.settings.borrow().list_view;
            self.stack
                .set_visible_child_name(if list_view { "list" } else { "grid" });
        } else {
            self.dir_list.set_file(Some(file));
            let list_view = self.settings.borrow().list_view;
            self.stack
                .set_visible_child_name(if list_view { "list" } else { "grid" });
        }
        self.update_status();
        self.notify_window();
    }

    /// Reads the starred files (Starred), keeping the rows that stay.
    fn fill_starred(self: &Rc<Self>) {
        let serial = self.starred_serial.get() + 1;
        self.starred_serial.set(serial);
        let files = self.shared.stars.listed();
        self.starred_loading.set(!self.shared.stars.is_loaded());
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let mut infos = Vec::new();
            for f in files {
                // Gone files (starred, then deleted elsewhere) are left out.
                if let Ok(info) = f
                    .query_info_future(
                        FILE_ATTRS,
                        gio::FileQueryInfoFlags::NONE,
                        glib::Priority::DEFAULT_IDLE,
                    )
                    .await
                {
                    info.set_attribute_object("standard::file", &f);
                    infos.push(info);
                }
            }
            let Some(t) = weak.upgrade() else { return };
            if t.starred_serial.get() != serial || !t.is_starred() {
                return;
            }
            let n = t.starred_store.n_items();
            t.starred_store.splice(0, n, &infos);
            t.starred_loading.set(!t.shared.stars.is_loaded());
            t.update_status();
            t.notify_window();
        });
        self.update_status();
    }

    /// Shows `star` (a row's mark) when `file` is starred, and keeps the
    /// file with it for later changes.
    fn show_star(&self, star: &gtk::Image, file: Option<&gio::File>) {
        let uri = file.map(|f| f.uri().to_string()).unwrap_or_default();
        star.set_visible(file.is_some_and(|f| self.shared.stars.is_starred(f)));
        // SAFETY: the key only ever holds a `String`.
        unsafe { star.set_data(STAR_URI_KEY, uri) };
    }

    /// The stars changed: Starred is read again, the marks follow.
    pub fn stars_changed(self: &Rc<Self>) {
        if self.is_starred() {
            self.fill_starred();
        }
        let stars = &self.shared.stars;
        self.star_marks.borrow_mut().retain(|w| match w.upgrade() {
            Some(star) => {
                // SAFETY: the key only ever holds a `String`.
                let uri = unsafe {
                    star.data::<String>(STAR_URI_KEY)
                        .map(|p| p.as_ref().clone())
                }
                .unwrap_or_default();
                star.set_visible(!uri.is_empty() && stars.is_starred(&gio::File::for_uri(&uri)));
                true
            }
            None => false,
        });
    }

    pub fn reload(self: &Rc<Self>) {
        let loc = self.location();
        let selected = self.selected_files();
        self.dir_list.set_file(None::<&gio::File>);
        self.load(&loc, false, selected);
        if !self.search_text.borrow().is_empty() {
            let text = self.search_text.borrow().clone();
            self.set_search(&text);
        }
    }

    fn mount_and_reload(self: &Rc<Self>) {
        let file = self.location();
        let op = self
            .window()
            .map(|w| gtk::MountOperation::new(Some(&w.window)));
        let weak = Rc::downgrade(self);
        file.mount_enclosing_volume(
            gio::MountMountFlags::NONE,
            op.as_ref(),
            None::<&gio::Cancellable>,
            move |res| {
                let Some(t) = weak.upgrade() else { return };
                match res {
                    Ok(()) => {
                        let loc = t.location();
                        t.dir_list.set_file(None::<&gio::File>);
                        t.dir_list.set_file(Some(&loc));
                    }
                    Err(e) if e.matches(gio::IOErrorEnum::FailedHandled) => t.update_status(),
                    Err(e) => {
                        if let Some(w) = t.window() {
                            w.toast(&trf!(
                                "Unable to access location: {}",
                                helix_app_common::errors::glib_detail(&e)
                            ));
                        }
                        t.update_status();
                    }
                }
            },
        );
    }

    /// Shows or hides the status page's "Try Again" button; the page only
    /// takes pointer input while it is shown, so it never blocks the view.
    fn set_status_retry(&self, show: bool) {
        if let Some(retry) = self.status.child() {
            retry.set_visible(show);
        }
        self.status.set_can_target(show);
    }

    fn update_status(&self) {
        let loading = self.is_loading();
        self.spinner.set_visible(loading);
        self.spinner.set_spinning(loading);
        if self.is_other_locations() {
            self.status.set_visible(false);
            return;
        }
        let empty = self.selection.n_items() == 0;
        if let (Some(err), false) = (self.dir_list.error(), self.is_searching()) {
            if !self.dir_list.is_loading() {
                self.status.set_icon_name(Some("dialog-error-symbolic"));
                self.status.set_title(tr("Unable to Open Folder"));
                self.status
                    .set_description(Some(&helix_app_common::errors::glib_text(&err)));
                self.set_status_retry(true);
                self.status.set_visible(true);
                if !self.error_shown.replace(true) {
                    helix_app_common::announce(
                        &self.root,
                        &trf!(
                            "Unable to open folder: {}",
                            helix_app_common::errors::glib_detail(&err)
                        ),
                        helix_app_common::Urgency::Assertive,
                    );
                }
                return;
            }
        }
        self.error_shown.set(false);
        self.set_status_retry(false);
        if empty && !loading {
            if self.is_searching() {
                self.status.set_icon_name(Some("system-search-symbolic"));
                self.status.set_title(tr("No Results Found"));
                self.status
                    .set_description(Some(tr("Try a different search.")));
            } else if self.is_starred() && !self.shared.stars.available() {
                self.status.set_icon_name(Some("starred-symbolic"));
                self.status.set_title(tr("Starred Files Unavailable"));
                self.status
                    .set_description(Some(tr("The stars GNOME Files keeps could not be read.")));
            } else if self.is_starred() {
                self.status.set_icon_name(Some("starred-symbolic"));
                self.status.set_title(tr("No Starred Files"));
                self.status
                    .set_description(Some(tr("Files and folders you star appear here.")));
            } else if self.is_trash() {
                self.status.set_icon_name(Some("user-trash-symbolic"));
                self.status.set_title(tr("Trash is Empty"));
                self.status.set_description(None);
            } else {
                self.status.set_icon_name(Some("folder-symbolic"));
                self.status.set_title(tr("Folder is Empty"));
                self.status.set_description(None);
            }
            self.status.set_visible(true);
        } else {
            self.status.set_visible(false);
        }
    }

    fn select_pending(&self) {
        let pending = self.pending_select.borrow().clone();
        if pending.is_empty() {
            return;
        }
        let mut found = Vec::new();
        for pos in 0..self.selection.n_items() {
            if let Some(f) = self.info_at(pos).and_then(|i| info_file(&i)) {
                if pending.iter().any(|p| p.equal(&f)) {
                    found.push((pos, f));
                }
            }
        }
        if found.is_empty() {
            return;
        }
        let fresh = self.pending_fresh.replace(false);
        if fresh {
            self.selection.unselect_all();
        }
        for (pos, _) in &found {
            self.selection.select_item(*pos, false);
        }
        if fresh {
            let first = found[0].0;
            if self.settings.borrow().list_view {
                self.list
                    .scroll_to(first, None, gtk::ListScrollFlags::FOCUS, None);
            } else {
                self.grid
                    .scroll_to(first, gtk::ListScrollFlags::FOCUS, None);
            }
        }
        self.pending_select
            .borrow_mut()
            .retain(|p| !found.iter().any(|(_, f)| f.equal(p)));
    }

    fn restore_selection(&self, pos: u32, added: u32) {
        if added == 0 || self.selected_uris.borrow().is_empty() {
            return;
        }
        self.restoring.set(true);
        for i in pos..pos + added {
            if self.selection.is_selected(i) {
                continue;
            }
            let Some(f) = self.info_at(i).and_then(|i| info_file(&i)) else {
                continue;
            };
            if self.selected_uris.borrow().contains(f.uri().as_str()) {
                self.selection.select_item(i, false);
            }
        }
        self.restoring.set(false);
    }

    /// Selects these files when they appear in the listing.
    pub fn select_when_listed(&self, files: Vec<gio::File>) {
        *self.pending_select.borrow_mut() = files;
        self.pending_fresh.set(true);
        self.select_pending();
    }

    // ----- items & selection -----

    pub fn info_at(&self, pos: u32) -> Option<gio::FileInfo> {
        self.selection.item(pos).and_downcast::<gio::FileInfo>()
    }

    pub fn selected_infos(&self) -> Vec<gio::FileInfo> {
        let set = self.selection.selection();
        let n = set.size();
        (0..n)
            .filter_map(|i| self.info_at(set.nth(i as u32)))
            .collect()
    }

    pub fn selected_files(&self) -> Vec<gio::File> {
        self.selected_infos().iter().filter_map(info_file).collect()
    }

    pub fn n_items(&self) -> u32 {
        self.selection.n_items()
    }

    pub fn select_all(&self) {
        self.selection.select_all();
    }

    pub fn invert_selection(&self) {
        let n = self.selection.n_items();
        let selected = self.selection.selection();
        let all = gtk::Bitset::new_range(0, n);
        all.subtract(&selected);
        self.selection
            .set_selection(&all, &gtk::Bitset::new_range(0, n));
    }

    fn activate(self: &Rc<Self>, pos: u32) {
        let Some(w) = self.window() else { return };
        let infos = if self.selection.is_selected(pos) {
            self.selected_infos()
        } else {
            self.info_at(pos).into_iter().collect()
        };
        w.open_infos(&infos, super::window::OpenIn::Current);
    }

    // ----- context menus -----

    fn popup_menu(&self, view: &gtk::Widget, x: f64, y: f64, on_item: bool) {
        let Some(w) = self.window() else { return };
        let menu = if on_item {
            w.item_menu(self)
        } else {
            w.background_menu(self)
        };
        self.popover.set_menu_model(Some(&menu));
        let pt = view
            .compute_point(&self.root, &graphene::Point::new(x as f32, y as f32))
            .unwrap_or(graphene::Point::new(x as f32, y as f32));
        self.popover.set_pointing_to(Some(&gdk::Rectangle::new(
            pt.x() as i32,
            pt.y() as i32,
            1,
            1,
        )));
        self.popover.popup();
    }

    /// Opens the context menu for the selection (keyboard: Menu key).
    pub fn popup_selection_menu(&self) {
        let Some(w) = self.window() else { return };
        let on_item = !self.selected_infos().is_empty();
        let menu = if on_item {
            w.item_menu(self)
        } else {
            w.background_menu(self)
        };
        self.popover.set_menu_model(Some(&menu));
        let (w, h) = (self.root.width(), self.root.height());
        self.popover
            .set_pointing_to(Some(&gdk::Rectangle::new(w / 2, h / 3, 1, 1)));
        self.popover.popup();
    }

    // ----- search -----

    fn cancel_search_worker(&self) {
        if let Some(c) = self.search_cancel.borrow_mut().take() {
            c.cancel();
        }
        self.searching.set(false);
    }

    pub fn search_text(&self) -> String {
        self.search_text.borrow().clone()
    }

    pub fn set_search(self: &Rc<Self>, text: &str) {
        self.cancel_search_worker();
        *self.search_text.borrow_mut() = text.to_string();
        let query = search::Query::new(text);
        if query.is_empty() || self.is_other_locations() || self.is_starred() {
            self.search_text.borrow_mut().clear();
            if self.is_starred() {
                self.filter_model.set_model(Some(&self.starred_store));
            } else {
                self.filter_model.set_model(Some(&self.dir_list));
            }
            self.update_status();
            self.notify_window();
            return;
        }
        self.search_store.remove_all();
        self.filter_model.set_model(Some(&self.search_store));
        let root = self.location();
        let settings = self.settings.borrow().clone();
        let depth = search::depth_for(&settings.recursive, root.is_native());
        let cancel = gio::Cancellable::new();
        *self.search_cancel.borrow_mut() = Some(cancel.clone());
        self.searching.set(true);
        self.update_status();

        let (tx, rx) = async_channel::unbounded::<Option<Vec<search::Hit>>>();
        let worker_cancel = cancel.clone();
        let show_hidden = settings.show_hidden;
        std::thread::spawn(move || {
            search::run(
                &root,
                &query,
                depth,
                show_hidden,
                &worker_cancel,
                &mut |batch| {
                    let _ = tx.send_blocking(Some(batch));
                },
            );
            let _ = tx.send_blocking(None);
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(msg) = rx.recv().await {
                let Some(t) = weak.upgrade() else { return };
                if cancel.is_cancelled() {
                    return;
                }
                match msg {
                    Some(batch) => {
                        let infos: Vec<gio::FileInfo> = batch.into_iter().map(hit_info).collect();
                        t.search_store.extend_from_slice(&infos);
                    }
                    None => {
                        t.searching.set(false);
                        let n = t.search_store.n_items();
                        let found = if n == 0 {
                            tr("No results").to_string()
                        } else {
                            helix_app_common::trnf!(
                                "Search finished: {n} item found",
                                "Search finished: {n} items found",
                                n
                            )
                        };
                        helix_app_common::announce(
                            &t.root,
                            &found,
                            helix_app_common::Urgency::Polite,
                        );
                        t.update_status();
                        t.notify_window();
                        return;
                    }
                }
            }
        });
        self.notify_window();
    }

    // ----- other locations -----

    fn build_other_locations(self: &Rc<Self>) {
        let page = adw::PreferencesPage::new();
        let computer = adw::PreferencesGroup::builder()
            .title(tr("On This Computer"))
            .build();
        let monitor = gio::VolumeMonitor::get();
        let weak = Rc::downgrade(self);
        let add_row = |group: &adw::PreferencesGroup,
                       title: &str,
                       subtitle: &str,
                       icon: &gio::Icon,
                       target: gio::File| {
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(subtitle)
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Image::from_gicon(icon));
            row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
            let weak = weak.clone();
            row.connect_activated(move |_| {
                if let Some(t) = weak.upgrade() {
                    t.load(&target, true, vec![]);
                }
            });
            group.add(&row);
        };
        add_row(
            &computer,
            tr("Computer"),
            "/",
            &gio::ThemedIcon::new("drive-harddisk-symbolic").upcast(),
            gio::File::for_path("/"),
        );
        let remote = adw::PreferencesGroup::builder()
            .title(tr("Networks"))
            .build();
        add_row(
            &remote,
            tr("Network"),
            tr("Browse the local network"),
            &gio::ThemedIcon::new("network-workgroup-symbolic").upcast(),
            gio::File::for_uri("network:///"),
        );
        for mount in monitor.mounts() {
            if mount.is_shadowed() {
                continue;
            }
            let root = mount.root();
            let subtitle = super::window::display_location(&root);
            let group = if root.is_native() { &computer } else { &remote };
            add_row(
                group,
                &mount.name(),
                &subtitle,
                &mount.symbolic_icon(),
                root,
            );
        }
        for vol in monitor.volumes() {
            if vol.get_mount().is_some() || !vol.can_mount() {
                continue;
            }
            let row = adw::ActionRow::builder()
                .title(vol.name())
                .subtitle(tr("Not mounted"))
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Image::from_gicon(&vol.symbolic_icon()));
            let weak = weak.clone();
            row.connect_activated(move |_| {
                if let Some(w) = weak.upgrade().and_then(|t| t.window()) {
                    w.mount_volume(&vol, false);
                }
            });
            computer.add(&row);
        }
        let connect = adw::PreferencesGroup::builder()
            .title(tr("Connect to Server"))
            .description("For example sftp://host/, smb://server/share, ftp://host/, dav://host/")
            .build();
        let entry = adw::EntryRow::builder()
            .title(tr("Server Address"))
            .show_apply_button(true)
            .build();
        entry.set_input_purpose(gtk::InputPurpose::Url);
        let weak = Rc::downgrade(self);
        entry.connect_apply(move |e| {
            let text = e.text().trim().to_string();
            if text.is_empty() {
                return;
            }
            if let Some(t) = weak.upgrade() {
                let f = gio::File::for_parse_name(&text);
                t.load(&f, true, vec![]);
            }
        });
        connect.add(&entry);
        page.add(&computer);
        page.add(&remote);
        page.add(&connect);
        self.other.set_child(Some(&page));
    }
}

/// Builds a `GFileInfo` for a search hit.
fn hit_info(hit: search::Hit) -> gio::FileInfo {
    let info = gio::FileInfo::new();
    let file = gio::File::for_uri(&hit.uri);
    info.set_name(&hit.name);
    info.set_display_name(&hit.display_name);
    info.set_file_type(if hit.is_dir {
        gio::FileType::Directory
    } else {
        gio::FileType::Regular
    });
    info.set_size(hit.size as i64);
    let ct = if hit.is_dir {
        "inode/directory".to_string()
    } else {
        hit.content_type
    };
    info.set_content_type(&ct);
    info.set_icon(&gio::content_type_get_icon(&ct));
    info.set_symbolic_icon(&gio::content_type_get_symbolic_icon(&ct));
    info.set_attribute_uint64("time::modified", hit.mtime.max(0) as u64);
    info.set_is_hidden(hit.name.starts_with('.'));
    info.set_attribute_boolean("standard::is-backup", hit.name.ends_with('~'));
    info.set_attribute_object("standard::file", &file);
    info
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::ClipState;
    use helix_app_common::testing::{headless_gtk, property_mismatch};
    use std::time::{Duration, Instant};

    /// Runs the default main context until `done` or 20 seconds passed.
    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let ctx = glib::MainContext::default();
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            if !ctx.iteration(false) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// Runs the default main context for `ms` milliseconds.
    fn settle(ms: u64) {
        let deadline = Instant::now() + Duration::from_millis(ms);
        let ctx = glib::MainContext::default();
        while Instant::now() < deadline {
            if !ctx.iteration(false) {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }

    /// The bound item widgets of `view` (grid or list) that can show the
    /// cut dimming, by URI.
    fn bound_items(tab: &Tab, view: &gtk::Widget) -> Vec<(String, gtk::Widget)> {
        let mut items: Vec<(String, gtk::Widget)> = tab
            .cut_marks
            .borrow()
            .iter()
            .filter_map(|w| w.upgrade())
            .filter(|w| w.is_ancestor(view) && w.is_mapped())
            .filter_map(|w| Some((tagged_file(&w)?.uri().to_string(), w)))
            .collect();
        items.sort_by(|a, b| a.0.cmp(&b.0));
        items
    }

    fn set_clip(tab: &Tab, clip: Option<ClipState>) {
        *tab.shared.clipboard.borrow_mut() = clip;
        tab.clipboard_changed();
        settle(300);
    }

    fn assert_cut(items: &[(String, gtk::Widget)], cut: &[&gio::File], when: &str) {
        for (uri, w) in items {
            let want = cut.iter().any(|f| f.uri() == *uri);
            assert_eq!(w.has_css_class("cut-item"), want, "{when}: {uri} dimmed");
            if want {
                assert_eq!(
                    property_mismatch(w, gtk::AccessibleProperty::Description, tr("Cut")),
                    None,
                    "{when}: {uri} described as cut"
                );
            } else {
                assert!(
                    !gtk::test_accessible_has_property(w, gtk::AccessibleProperty::Description),
                    "{when}: {uri} still described"
                );
            }
        }
    }

    /// Cutting or copying dims or undims the items in place (#211): the
    /// clipboard used to give every view new factories, and GTK then
    /// recreated every item widget, so the folder flashed as if reloaded.
    /// Runs [`cut_and_copy_keep_the_item_widgets_child`] in a child
    /// process, as this binary's other GTK test needs GTK on its own.
    #[test]
    fn cut_and_copy_keep_the_item_widgets() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ui::tab::tests::cut_and_copy_keep_the_item_widgets_child",
            ])
            .args(["--ignored", "--nocapture"])
            .env("HELIX_FILES_CUT_CHILD", "1")
            .env("GSETTINGS_BACKEND", "memory")
            .env("GDK_DEBUG", "no-portals")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    #[ignore = "run by cut_and_copy_keep_the_item_widgets"]
    fn cut_and_copy_keep_the_item_widgets_child() {
        if std::env::var_os("HELIX_FILES_CUT_CHILD").is_none() {
            return;
        }
        let Some(_display) = headless_gtk() else {
            return;
        };
        adw::init().unwrap();
        let dir = std::env::temp_dir().join(format!("helix-files-cut-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<gio::File> = ["a.txt", "b.txt", "c.txt"]
            .iter()
            .map(|n| {
                std::fs::write(dir.join(n), "x").unwrap();
                gio::File::for_path(dir.join(n))
            })
            .collect();
        let shared = Rc::new(Shared {
            app: adw::Application::builder()
                .application_id("org.helixdesktop.Files.Test")
                .flags(gio::ApplicationFlags::NON_UNIQUE)
                .build(),
            prefs: helix_app_common::Prefs::new(super::super::PREFS),
            window_state: helix_app_common::Prefs::new(super::super::WINDOW_STATE),
            list_prefs: helix_app_common::Prefs::new(super::super::LIST_VIEW),
            icon_prefs: helix_app_common::Prefs::new(super::super::ICON_VIEW),
            chooser_prefs: helix_app_common::Prefs::new(super::super::CHOOSER),
            clipboard: RefCell::default(),
            undo: RefCell::default(),
            windows: RefCell::default(),
            stars: super::super::starred::Stars::new(),
        });
        let tab = Tab::new(&shared, Weak::new(), &gio::File::for_path(&dir));
        // No thumbnailing (nor a thumbnail cache) in the test.
        tab.settings.borrow_mut().thumbnails = "never".into();
        tab.load(&gio::File::for_path(&dir), false, Vec::new());
        let window = gtk::Window::builder()
            .default_width(800)
            .default_height(600)
            .child(&tab.root)
            .build();
        window.present();

        for (name, view) in [
            ("grid", tab.grid.clone().upcast::<gtk::Widget>()),
            ("list", tab.list.clone().upcast::<gtk::Widget>()),
        ] {
            tab.stack.set_visible_child_name(name);
            wait_for(&format!("the {name} items"), || {
                helix_app_common::testing::follow_default_size(&window);
                bound_items(&tab, &view).len() == files.len()
            });
            settle(300);
            let before = bound_items(&tab, &view);
            assert_cut(&before, &[], &format!("{name}, nothing cut"));

            let steps: [(&str, Option<ClipState>, Vec<&gio::File>); 4] = [
                (
                    "cut a and b",
                    Some(ClipState {
                        files: vec![files[0].clone(), files[1].clone()],
                        cut: true,
                    }),
                    vec![&files[0], &files[1]],
                ),
                (
                    "copy c",
                    Some(ClipState {
                        files: vec![files[2].clone()],
                        cut: false,
                    }),
                    vec![],
                ),
                (
                    "cut c",
                    Some(ClipState {
                        files: vec![files[2].clone()],
                        cut: true,
                    }),
                    vec![&files[2]],
                ),
                ("paste the cut", None, vec![]),
            ];
            for (step, clip, cut) in steps {
                set_clip(&tab, clip);
                let after = bound_items(&tab, &view);
                let when = format!("{name}, {step}");
                assert_eq!(after.len(), before.len(), "{when}: items shown");
                for ((uri, a), (_, b)) in before.iter().zip(&after) {
                    assert_eq!(a, b, "{when}: {uri}'s widget was recreated");
                }
                assert_cut(&after, &cut, &when);
            }
        }
        drop(window);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
