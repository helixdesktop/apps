//! Browser window: sidebar, header (path bar, navigation), tabs, actions.

use adw::prelude::*;
use gtk::{gdk, gio, glib};
use helix_app_common::Labelled;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use super::sidebar::Sidebar;
use super::tab::{info_file, info_is_dir, info_target, Tab};
use super::{
    dialogs, jobs, properties, ClipState, Shared, Undo, GRID_ZOOM, LIST_ZOOM, OTHER_LOCATIONS,
};
use crate::ops::Job;
use crate::pathbar::{self, Target};
use crate::sorting::SortKey;
use helix_app_common::i18n::tr;
use helix_app_common::nc_;
use helix_app_common::trf;
use helix_app_common::trnf;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OpenIn {
    Current,
    NewTab,
    NewWindow,
}

/// Shortcuts that act on the file view only, so text entries keep these
/// keys: (trigger, action). Listed in the Keyboard Shortcuts window.
pub const VIEW_SHORTCUTS: &[(&str, &str)] = &[
    ("Delete", "win.trash"),
    ("KP_Delete", "win.trash"),
    ("<Shift>Delete", "win.delete"),
    ("<Shift>KP_Delete", "win.delete"),
    ("<Control>a", "win.select-all"),
    ("<Control>c", "win.copy"),
    ("<Control>x", "win.cut"),
    ("<Control>v", "win.paste"),
    ("<Control>z", "win.undo"),
    ("F2", "win.rename"),
    ("Menu", "win.context-menu"),
    ("<Shift>F10", "win.context-menu"),
    ("<Control>Return", "win.open-new-tab"),
    ("<Alt>Down", "win.open"),
];

pub struct Window {
    pub shared: Rc<Shared>,
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    split: adw::OverlaySplitView,
    tab_view: adw::TabView,
    tabs: RefCell<Vec<(adw::TabPage, Rc<Tab>)>>,
    path_stack: gtk::Stack,
    crumbs: gtk::Box,
    crumbs_scroll: gtk::ScrolledWindow,
    location_entry: gtk::Entry,
    search_bar: gtk::SearchBar,
    search_entry: gtk::SearchEntry,
    /// The header's and the narrow bottom bar's view toggle.
    view_btns: [gtk::Button; 2],
    pub jobs_button: gtk::MenuButton,
    pub jobs_box: gtk::Box,
    /// The jobs button's icon: a pie of the combined progress.
    pub jobs_icon: gtk::DrawingArea,
    status: gtk::Label,
    banner: adw::Banner,
    sidebar: RefCell<Option<Rc<Sidebar>>>,
    updating_search: Cell<bool>,
    crumbs_uri: RefCell<String>,
}

/// Picks copy or move for a drop: modifiers first, then "move within the
/// same filesystem, copy across".
pub fn preferred_drop_action(
    target: &gtk::DropTarget,
    files: Option<&[gio::File]>,
    dest: &gio::File,
) -> gdk::DragAction {
    let state = target.current_event_state();
    if state.contains(gdk::ModifierType::CONTROL_MASK) {
        return gdk::DragAction::COPY;
    }
    if state.contains(gdk::ModifierType::SHIFT_MASK) {
        return gdk::DragAction::MOVE;
    }
    let files: Vec<gio::File> = match files {
        Some(f) => f.to_vec(),
        None => target
            .value()
            .and_then(|v| v.get::<gdk::FileList>().ok())
            .map(|l| l.files())
            .unwrap_or_default(),
    };
    match files.first() {
        Some(f) if same_filesystem(f, dest) => gdk::DragAction::MOVE,
        Some(_) => gdk::DragAction::COPY,
        // Data not available yet during motion: assume move.
        None => gdk::DragAction::MOVE,
    }
}

fn same_filesystem(a: &gio::File, b: &gio::File) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (a.path(), b.path()) {
        (Some(pa), Some(pb)) => match (std::fs::symlink_metadata(&pa), std::fs::metadata(&pb)) {
            (Ok(ma), Ok(mb)) => ma.dev() == mb.dev(),
            _ => false,
        },
        (None, None) => {
            let host = |f: &gio::File| {
                let uri = f.uri().to_string();
                uri.split_once("://")
                    .map(|(s, r)| format!("{s}://{}", r.split('/').next().unwrap_or("")))
                    .unwrap_or(uri)
            };
            host(a) == host(b)
        }
        _ => false,
    }
}

fn home() -> gio::File {
    gio::File::for_path(glib::home_dir())
}

/// User-facing name of a location.
pub fn location_title(file: &gio::File) -> String {
    let uri = file.uri();
    if uri == OTHER_LOCATIONS {
        return tr("Other Locations").into();
    }
    if let Some(p) = local_path(file) {
        if p == glib::home_dir() {
            return tr(nc_!("home folder", "Home")).into();
        }
        if p == std::path::Path::new("/") {
            return tr("Computer").into();
        }
    }
    match pathbar::crumbs_for_uri(&uri) {
        Some(c) if !file.is_native() => c.last().map(|c| c.label.clone()).unwrap_or_default(),
        _ => crate::ops::display_name(file),
    }
}

impl Window {
    pub fn new(
        shared: &Rc<Shared>,
        location: Option<gio::File>,
        select: Vec<gio::File>,
    ) -> Rc<Window> {
        let window = adw::ApplicationWindow::builder()
            .application(&shared.app)
            .title(tr("Files"))
            .default_width(890)
            .default_height(550)
            .icon_name("system-file-manager")
            .width_request(360)
            .height_request(294)
            .build();
        {
            use helix_app_common::window_state::{persist, SizeKeys, Store};
            persist(
                &window,
                Store::settings(
                    &shared.window_state,
                    SizeKeys::Pair("initial-size"),
                    "maximized",
                    "helix-files",
                ),
            );
        }

        // ----- content header -----
        let header = adw::HeaderBar::new();
        let sidebar_btn = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-symbolic")
            .tooltip_text(tr("Show Sidebar"))
            .build()
            .labelled(tr("Show Sidebar"));
        let back_btn = gtk::Button::builder()
            .icon_name("go-previous-symbolic")
            .tooltip_text(tr("Back"))
            .action_name("win.back")
            .build()
            .labelled(tr("Back"));
        let forward_btn = gtk::Button::builder()
            .icon_name("go-next-symbolic")
            .tooltip_text(tr("Forward"))
            .action_name("win.forward")
            .build()
            .labelled(tr("Forward"));
        header.pack_start(&sidebar_btn);
        header.pack_start(&back_btn);
        header.pack_start(&forward_btn);

        let crumbs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        let crumbs_scroll = gtk::ScrolledWindow::builder()
            .child(&crumbs)
            .vscrollbar_policy(gtk::PolicyType::Never)
            .hscrollbar_policy(gtk::PolicyType::External)
            .hexpand(true)
            .propagate_natural_width(true)
            .build();
        let location_entry = gtk::Entry::builder()
            .hexpand(true)
            .placeholder_text(tr("Enter a location"))
            .build()
            .labelled(tr("Location"));
        location_entry.set_input_purpose(gtk::InputPurpose::Url);
        let path_stack = gtk::Stack::new();
        path_stack.set_hhomogeneous(false);
        path_stack.add_named(&crumbs_scroll, Some("crumbs"));
        path_stack.add_named(&location_entry, Some("entry"));
        path_stack.set_hexpand(true);
        path_stack.set_margin_start(6);
        path_stack.set_margin_end(6);
        header.set_title_widget(Some(&path_stack));

        let main_menu = gio::Menu::new();
        let sec = gio::Menu::new();
        sec.append(Some(tr("New _Window")), Some("app.new-window"));
        sec.append(Some(tr("New _Tab")), Some("win.new-tab"));
        main_menu.append_section(None, &sec);
        let sec = gio::Menu::new();
        sec.append(Some(tr("_Undo")), Some("win.undo"));
        main_menu.append_section(None, &sec);
        let sec = gio::Menu::new();
        sec.append(Some(tr("_Preferences")), Some("app.preferences"));
        sec.append(Some(tr("_Keyboard Shortcuts")), Some("win.show-shortcuts"));
        sec.append(Some(tr("_About Files")), Some("app.about"));
        main_menu.append_section(None, &sec);
        let menu_btn = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&main_menu)
            .tooltip_text(tr("Main Menu"))
            .primary(true)
            .build()
            .labelled(tr("Main Menu"));
        header.pack_end(&menu_btn);

        let view_menu = gio::Menu::new();
        let sort = gio::Menu::new();
        for key in SortKey::ALL {
            let item = gio::MenuItem::new(Some(key.label()), None);
            item.set_action_and_target_value(Some("win.sort"), Some(&key.nick().to_variant()));
            sort.append_item(&item);
        }
        sort.append(Some(tr("_Reversed Order")), Some("win.sort-reversed"));
        view_menu.append_section(Some(tr("Sort")), &sort);
        let zoom = gio::Menu::new();
        zoom.append(Some(tr("Zoom _In")), Some("win.zoom-in"));
        zoom.append(Some(tr("Zoom _Out")), Some("win.zoom-out"));
        zoom.append(Some(tr("_Reset Size")), Some("win.zoom-reset"));
        view_menu.append_section(None, &zoom);
        let misc = gio::Menu::new();
        misc.append(Some(tr("Show _Hidden Files")), Some("win.show-hidden"));
        view_menu.append_section(None, &misc);
        let view_menu_btn = gtk::MenuButton::builder()
            .icon_name("view-sort-descending-symbolic")
            .menu_model(&view_menu)
            .tooltip_text(tr("View Options"))
            .build()
            .labelled(tr("View Options"));
        header.pack_end(&view_menu_btn);
        let view_btn = gtk::Button::builder()
            .action_name("win.toggle-view")
            .build();
        header.pack_end(&view_btn);
        let search_btn = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text(tr("Search"))
            .build()
            .labelled(tr("Search"));
        header.pack_end(&search_btn);

        let jobs_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        jobs_box.set_margin_top(6);
        jobs_box.set_margin_bottom(6);
        jobs_box.set_margin_start(6);
        jobs_box.set_margin_end(6);
        jobs_box.set_width_request(320);
        let jobs_popover = gtk::Popover::builder().child(&jobs_box).build();
        let jobs_icon = gtk::DrawingArea::builder()
            .content_width(16)
            .content_height(16)
            .halign(gtk::Align::Center)
            .valign(gtk::Align::Center)
            .build();
        {
            let jobs_box = jobs_box.downgrade();
            jobs_icon.set_draw_func(move |a, cr, _, _| {
                if let Some(b) = jobs_box.upgrade() {
                    jobs::draw_indicator(a, cr, &b);
                }
            });
            helix_app_common::style::redraw_on_style_change(&jobs_icon);
        }
        let jobs_button = gtk::MenuButton::builder()
            .child(&jobs_icon)
            .popover(&jobs_popover)
            .tooltip_text(tr("File Operations"))
            .visible(false)
            .build()
            .labelled(tr("File Operations"));
        header.pack_end(&jobs_button);

        // ----- search bar -----
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(tr("Search current folder"))
            .hexpand(true)
            .build()
            .labelled(tr("Search Current Folder"));
        // The clamp sizes the entry; a wide minimum would stop the window
        // from shrinking (the hidden search bar still counts).
        search_entry.set_width_chars(10);
        let search_clamp = adw::Clamp::builder()
            .maximum_size(500)
            .child(&search_entry)
            .build();
        let search_bar = gtk::SearchBar::builder().child(&search_clamp).build();
        search_bar.connect_entry(&search_entry);
        search_bar.set_key_capture_widget(Some(&window));
        search_btn
            .bind_property("active", &search_bar, "search-mode-enabled")
            .bidirectional()
            .sync_create()
            .build();

        // ----- tabs -----
        let tab_view = adw::TabView::new();
        tab_view.set_vexpand(true);
        let tab_bar = adw::TabBar::builder()
            .view(&tab_view)
            .autohide(true)
            .build();
        let banner = adw::Banner::builder()
            .title(tr("Trash"))
            .button_label(tr("_Empty…"))
            .action_name("win.empty-trash")
            .build();

        let status = gtk::Label::builder()
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::Middle)
            .build();
        status.add_css_class("dim-label");
        status.add_css_class("caption");
        status.add_css_class("files-status");

        let content = adw::ToolbarView::new();
        content.add_top_bar(&header);
        content.add_top_bar(&tab_bar);
        content.add_top_bar(&search_bar);
        content.add_top_bar(&banner);
        content.set_content(Some(&tab_view));
        content.add_bottom_bar(&status);

        let split = adw::OverlaySplitView::builder()
            .content(&content)
            .sidebar_width_fraction(0.2)
            .min_sidebar_width(180.0)
            .max_sidebar_width(280.0)
            .build();
        split
            .bind_property("show-sidebar", &sidebar_btn, "active")
            .bidirectional()
            .sync_create()
            .build();
        let toasts = adw::ToastOverlay::new();
        toasts.set_child(Some(&split));
        window.set_content(Some(&toasts));

        // Narrow windows: back/forward and the view controls move from the
        // header to a bottom bar so the path bar keeps room.
        let nav_bar = gtk::ActionBar::builder().visible(false).build();
        let back_btn2 = helix_app_common::a11y::icon_button("go-previous-symbolic", tr("Back"));
        back_btn2.set_action_name(Some("win.back"));
        let forward_btn2 = helix_app_common::a11y::icon_button("go-next-symbolic", tr("Forward"));
        forward_btn2.set_action_name(Some("win.forward"));
        let view_btn2 = gtk::Button::builder()
            .action_name("win.toggle-view")
            .build();
        let view_menu_btn2 = helix_app_common::a11y::icon_menu_button(
            "view-sort-descending-symbolic",
            tr("View Options"),
        );
        view_menu_btn2.set_menu_model(Some(&view_menu));
        view_menu_btn2.set_direction(gtk::ArrowType::Up);
        nav_bar.pack_start(&back_btn2);
        nav_bar.pack_start(&forward_btn2);
        nav_bar.pack_end(&view_menu_btn2);
        nav_bar.pack_end(&view_btn2);
        content.add_bottom_bar(&nav_bar);
        let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            500.0,
            adw::LengthUnit::Sp,
        ));
        narrow.add_setter(&split, "collapsed", Some(&true.to_value()));
        for w in [
            back_btn.upcast_ref::<gtk::Widget>(),
            forward_btn.upcast_ref(),
            view_btn.upcast_ref(),
            view_menu_btn.upcast_ref(),
        ] {
            narrow.add_setter(w, "visible", Some(&false.to_value()));
        }
        narrow.add_setter(&nav_bar, "visible", Some(&true.to_value()));
        let view_btns = [view_btn, view_btn2];

        // Collapse sidebar on narrow windows.
        let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            600.0,
            adw::LengthUnit::Sp,
        ));
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(bp);
        // Added last: of several matching breakpoints the last one applies.
        window.add_breakpoint(narrow);
        split
            .bind_property("collapsed", &sidebar_btn, "visible")
            .sync_create()
            .build();
        sidebar_btn.set_visible(false);

        let win = Rc::new(Window {
            shared: shared.clone(),
            window,
            toasts,
            split,
            tab_view,
            tabs: RefCell::default(),
            path_stack,
            crumbs,
            crumbs_scroll,
            location_entry,
            search_bar,
            search_entry,
            view_btns,
            jobs_button,
            jobs_box,
            jobs_icon,
            status,
            banner,
            sidebar: RefCell::default(),
            updating_search: Cell::new(false),
            crumbs_uri: RefCell::default(),
        });
        let sidebar = Sidebar::new(&win);
        let sidebar_page = adw::ToolbarView::new();
        let sh = adw::HeaderBar::builder().show_title(true).build();
        sh.set_title_widget(Some(&adw::WindowTitle::new(tr("Files"), "")));
        sidebar_page.add_top_bar(&sh);
        sidebar_page.set_content(Some(&sidebar.widget));
        win.split.set_sidebar(Some(&sidebar_page));
        *win.sidebar.borrow_mut() = Some(sidebar);

        win.install_actions();
        win.connect_signals();
        shared.windows.borrow_mut().push(win.clone());
        let start = location.unwrap_or_else(home);
        win.open_tab(&start, select, true);
        win.apply_settings();
        win
    }

    fn connect_signals(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        self.tab_view.connect_selected_page_notify(move |_| {
            if let Some(w) = weak.upgrade() {
                if let Some(t) = w.current_tab() {
                    w.tab_changed(&t);
                    t.focus_view();
                }
            }
        });
        let weak = Rc::downgrade(self);
        self.tab_view.connect_close_page(move |view, page| {
            if let Some(w) = weak.upgrade() {
                w.tabs.borrow_mut().retain(|(p, _)| p != page);
                if view.n_pages() <= 1 {
                    let win = w.window.clone();
                    glib::idle_add_local_once(move || win.close());
                }
            }
            glib::Propagation::Proceed
        });
        let weak = Rc::downgrade(self);
        self.window.connect_close_request(move |_| {
            if let Some(w) = weak.upgrade() {
                if let Some(s) = w.sidebar.borrow_mut().take() {
                    s.shutdown();
                }
                w.shared.remove_window(&w);
            }
            glib::Propagation::Proceed
        });

        // Location entry.
        let weak = Rc::downgrade(self);
        self.location_entry.connect_activate(move |e| {
            let Some(w) = weak.upgrade() else { return };
            let text = e.text().trim().to_string();
            if text.is_empty() {
                return;
            }
            let file = if text.starts_with('~') {
                gio::File::for_parse_name(&expand_tilde(&text))
            } else {
                gio::File::for_parse_name(&text)
            };
            w.navigate_smart(file);
            if !w.shared.view_settings().location_entry {
                w.path_stack.set_visible_child_name("crumbs");
            }
            if let Some(t) = w.current_tab() {
                t.focus_view();
            }
        });
        let key = gtk::EventControllerKey::new();
        let weak = Rc::downgrade(self);
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gdk::Key::Escape {
                if let Some(w) = weak.upgrade() {
                    w.hide_location_entry();
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
        self.location_entry.add_controller(key);

        // Search.
        let weak = Rc::downgrade(self);
        self.search_entry.connect_search_changed(move |e| {
            let Some(w) = weak.upgrade() else { return };
            if w.updating_search.get() {
                return;
            }
            if let Some(t) = w.current_tab() {
                t.set_search(&e.text());
            }
        });
        let weak = Rc::downgrade(self);
        self.search_bar
            .connect_search_mode_enabled_notify(move |bar| {
                let Some(w) = weak.upgrade() else { return };
                if !bar.is_search_mode() {
                    if let Some(t) = w.current_tab() {
                        if t.is_searching() {
                            t.set_search("");
                        }
                        t.focus_view();
                    }
                }
            });

        // Mouse back/forward buttons.
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.set_propagation_phase(gtk::PropagationPhase::Capture);
        let weak = Rc::downgrade(self);
        click.connect_pressed(move |g, _, _, _| {
            let Some(w) = weak.upgrade() else { return };
            let s = w.shared.view_settings();
            if !s.extra_buttons {
                return;
            }
            let b = g.current_button();
            if b == s.back_button {
                w.go_back();
                g.set_state(gtk::EventSequenceState::Claimed);
            } else if b == s.forward_button {
                w.go_forward();
                g.set_state(gtk::EventSequenceState::Claimed);
            }
        });
        self.window.add_controller(click);

        // View-local shortcuts (not global so text entries keep them).
        let sc = gtk::ShortcutController::new();
        sc.set_scope(gtk::ShortcutScope::Local);
        for (trigger, action) in VIEW_SHORTCUTS {
            sc.add_shortcut(gtk::Shortcut::new(
                gtk::ShortcutTrigger::parse_string(trigger),
                Some(gtk::NamedAction::new(action)),
            ));
        }
        self.tab_view.add_controller(sc);
    }

    // ----- tabs -----

    pub fn current_tab(&self) -> Option<Rc<Tab>> {
        let page = self.tab_view.selected_page()?;
        self.tabs
            .borrow()
            .iter()
            .find(|(p, _)| *p == page)
            .map(|(_, t)| t.clone())
    }

    fn page_of(&self, tab: &Rc<Tab>) -> Option<adw::TabPage> {
        self.tabs
            .borrow()
            .iter()
            .find(|(_, t)| Rc::ptr_eq(t, tab))
            .map(|(p, _)| p.clone())
    }

    pub fn open_tab(self: &Rc<Self>, location: &gio::File, select: Vec<gio::File>, focus: bool) {
        let tab = Tab::new(&self.shared, Rc::downgrade(self), location);
        let page = self.tab_view.append(&tab.root);
        self.tabs.borrow_mut().push((page.clone(), tab.clone()));
        tab.load(location, true, select);
        if focus {
            self.tab_view.set_selected_page(&page);
        }
        self.tab_changed(&tab);
    }

    /// Called by a tab whenever its location, loading state, items or
    /// selection change.
    pub fn tab_changed(self: &Rc<Self>, tab: &Rc<Tab>) {
        let loc = tab.location();
        let title = if tab.is_searching() {
            trf!("Search: {}", tab.search_text())
        } else {
            location_title(&loc)
        };
        if let Some(page) = self.page_of(tab) {
            page.set_title(&title);
            page.set_loading(tab.is_loading());
            page.set_tooltip(&loc.parse_name());
        }
        let is_current = self.current_tab().is_some_and(|t| Rc::ptr_eq(&t, tab));
        if !is_current {
            return;
        }
        self.window.set_title(Some(&title));
        {
            let h = tab.history.borrow();
            self.action_set_enabled("back", h.can_go_back());
            self.action_set_enabled("forward", h.can_go_forward());
        }
        self.action_set_enabled(
            "up",
            loc.parent().is_some() && !tab.is_other_locations() && !tab.is_starred(),
        );
        self.rebuild_crumbs(&loc);
        if self.path_stack.visible_child_name().as_deref() == Some("entry")
            && !self.location_entry.has_focus()
        {
            self.location_entry.set_text(&display_location(&loc));
        }
        self.banner
            .set_revealed(tab.is_trash() && tab.n_items() > 0 && !tab.is_searching());
        // Keep the search bar in sync with this tab.
        let text = tab.search_text();
        if self.search_entry.text() != text {
            self.updating_search.set(true);
            self.search_entry.set_text(&text);
            self.updating_search.set(false);
        }
        if !text.is_empty() && !self.search_bar.is_search_mode() {
            self.search_bar.set_search_mode(true);
        }
        self.update_status(tab);
        self.update_selection_actions(tab);
        if let Some(s) = self.sidebar.borrow().as_ref() {
            s.highlight(&loc);
        }
    }

    fn action_set_enabled(&self, name: &str, enabled: bool) {
        if let Some(a) = self
            .window
            .lookup_action(name)
            .and_downcast::<gio::SimpleAction>()
        {
            a.set_enabled(enabled);
        }
    }

    fn update_selection_actions(&self, tab: &Tab) {
        let infos = tab.selected_infos();
        let any = !infos.is_empty();
        let in_trash = tab.is_trash();
        let other = tab.is_other_locations() || tab.is_starred();
        for a in ["open", "open-with", "copy", "copy-location"] {
            self.action_set_enabled(a, any);
        }
        self.action_set_enabled("cut", any && !in_trash);
        self.action_set_enabled("trash", any && !in_trash);
        self.action_set_enabled("delete", any);
        self.action_set_enabled("rename", infos.len() == 1 && !in_trash);
        self.action_set_enabled("restore", any && in_trash);
        self.action_set_enabled("create-link", any && !in_trash);
        self.action_set_enabled("properties", true);
        self.action_set_enabled("new-folder", !in_trash && !other && !tab.is_searching());
        self.action_set_enabled("paste", !in_trash && !other && !tab.is_searching());
        let (star, unstar) = self.star_choices(&tab.selected_files());
        self.action_set_enabled("star", star && !in_trash);
        self.action_set_enabled("unstar", unstar && !in_trash);
        let dirs = any && infos.iter().all(info_is_dir);
        self.action_set_enabled("open-new-tab", dirs);
        self.action_set_enabled("open-new-window", dirs);
    }

    /// Whether `files` can be starred (some are not yet) and unstarred
    /// (all are).
    fn star_choices(&self, files: &[gio::File]) -> (bool, bool) {
        let stars = &self.shared.stars;
        if files.is_empty() || !files.iter().all(|f| stars.can_star(f)) {
            return (false, false);
        }
        let all = files.iter().all(|f| stars.is_starred(f));
        (!all, all)
    }

    /// The stars changed: the views, Starred and the menus follow.
    pub fn stars_changed(self: &Rc<Self>) {
        let tabs: Vec<Rc<Tab>> = self.tabs.borrow().iter().map(|(_, t)| t.clone()).collect();
        for t in &tabs {
            t.stars_changed();
        }
        if let Some(t) = self.current_tab() {
            self.update_selection_actions(&t);
        }
    }

    /// Stars (`on`) or unstars the selection.
    fn star_selection(self: &Rc<Self>, on: bool) {
        let Some(tab) = self.current_tab() else {
            return;
        };
        let files = tab.selected_files();
        if files.is_empty() {
            return;
        }
        self.shared.stars.set(&self.shared, &files, on);
        helix_app_common::announce(
            &tab.root,
            if on { tr("Starred") } else { tr("Unstarred") },
            helix_app_common::Urgency::Polite,
        );
    }

    fn update_status(&self, tab: &Tab) {
        if tab.is_other_locations() {
            self.status.set_text("");
            return;
        }
        let infos = tab.selected_infos();
        let text = match infos.len() {
            0 => {
                let n = tab.n_items() as u64;
                let base = crate::fmt::item_count(n);
                if tab.is_loading() {
                    if tab.is_searching() {
                        trf!("{base} — Searching…", base = base)
                    } else {
                        trf!("{base} — Loading…", base = base)
                    }
                } else {
                    base
                }
            }
            1 => {
                let i = &infos[0];
                if info_is_dir(i) {
                    trf!("“{}” selected", i.display_name())
                } else {
                    trf!(
                        "“{}” selected ({})",
                        i.display_name(),
                        crate::fmt::size(i.size().max(0) as u64)
                    )
                }
            }
            n => {
                let bytes: u64 = infos
                    .iter()
                    .filter(|i| !info_is_dir(i))
                    .map(|i| i.size().max(0) as u64)
                    .sum();
                let dirs = infos.iter().filter(|i| info_is_dir(i)).count();
                if dirs == n {
                    trnf!("{n} folder selected", "{n} folders selected", n)
                } else {
                    trnf!(
                        "{n} item selected ({bytes})",
                        "{n} items selected ({bytes})",
                        n,
                        bytes = crate::fmt::size(bytes)
                    )
                }
            }
        };
        self.status.set_text(&text);
    }

    pub fn apply_settings(self: &Rc<Self>) {
        let s = self.shared.view_settings();
        for (_, t) in self.tabs.borrow().iter() {
            t.apply_settings();
        }
        let set_state = |name: &str, v: glib::Variant| {
            if let Some(a) = self
                .window
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                a.set_state(&v);
            }
        };
        set_state("show-hidden", s.show_hidden.to_variant());
        set_state("sort", s.sort_key.nick().to_variant());
        set_state("sort-reversed", s.reversed.to_variant());
        set_state(
            "view-mode",
            if s.list_view { "list" } else { "grid" }.to_variant(),
        );
        let (icon, label) = if s.list_view {
            ("view-grid-symbolic", tr("Grid View"))
        } else {
            ("view-list-symbolic", tr("List View"))
        };
        for b in &self.view_btns {
            helix_app_common::a11y::set_icon_and_label(b, icon, label);
        }
        if s.location_entry {
            self.show_location_entry(false);
        } else if !self.location_entry.has_focus() {
            self.path_stack.set_visible_child_name("crumbs");
        }
    }

    pub fn toast(&self, text: &str) {
        let t = adw::Toast::new(text);
        t.set_use_markup(false);
        self.toasts.add_toast(t);
    }

    pub fn add_toast(&self, toast: adw::Toast) {
        self.toasts.add_toast(toast);
    }

    // ----- path bar -----

    fn path_roots(&self) -> Vec<pathbar::Root> {
        let mut roots = vec![pathbar::Root {
            path: glib::home_dir(),
            label: tr(nc_!("home folder", "Home")).into(),
            icon: "user-home-symbolic",
        }];
        for m in gio::VolumeMonitor::get().mounts() {
            if let Some(p) = local_path(&m.root()) {
                if p != std::path::Path::new("/") {
                    roots.push(pathbar::Root {
                        path: p,
                        label: m.name().to_string(),
                        icon: "drive-removable-media-symbolic",
                    });
                }
            }
        }
        roots
    }

    fn rebuild_crumbs(self: &Rc<Self>, loc: &gio::File) {
        let uri = loc.uri().to_string();
        if *self.crumbs_uri.borrow() == uri {
            return;
        }
        *self.crumbs_uri.borrow_mut() = uri;
        while let Some(c) = self.crumbs.first_child() {
            self.crumbs.remove(&c);
        }
        let crumbs = match local_path(loc) {
            Some(p) => pathbar::crumbs_for_path(&p, &self.path_roots()),
            None => pathbar::crumbs_for_uri(&loc.uri()).unwrap_or_default(),
        };
        let n = crumbs.len();
        for (i, c) in crumbs.into_iter().enumerate() {
            if i > 0 {
                // The crumbs themselves are the path; the slashes between
                // them are only drawn.
                let sep = gtk::Label::builder()
                    .label("/")
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .build();
                sep.add_css_class("crumb-sep");
                sep.add_css_class("dim-label");
                self.crumbs.append(&sep);
            }
            let content = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            if let Some(icon) = c.icon {
                content.append(&gtk::Image::from_icon_name(icon));
            }
            if !c.label.is_empty() {
                let l = gtk::Label::new(Some(&c.label));
                if i + 1 == n {
                    l.add_css_class("heading");
                }
                content.append(&l);
            }
            let btn = gtk::Button::builder().child(&content).build();
            if c.label.is_empty() {
                helix_app_common::set_accessible_label(&btn, &c.name());
            }
            btn.add_css_class("flat");
            btn.add_css_class("crumb");
            let file = match &c.target {
                Target::Path(p) => gio::File::for_path(p),
                Target::Uri(u) => gio::File::for_uri(u),
            };
            btn.set_tooltip_text(Some(&display_location(&file)));
            let weak = Rc::downgrade(self);
            btn.connect_clicked(move |_| {
                if let Some(w) = weak.upgrade() {
                    w.navigate(&file);
                }
            });
            // Crumbs accept drops.
            self.attach_drop(btn.upcast_ref(), {
                let f = match &c.target {
                    Target::Path(p) => gio::File::for_path(p),
                    Target::Uri(u) => gio::File::for_uri(u),
                };
                move || f.clone()
            });
            self.crumbs.append(&btn);
        }
        let adj = self.crumbs_scroll.hadjustment();
        glib::idle_add_local_once(move || adj.set_value(adj.upper()));
    }

    fn show_location_entry(&self, focus: bool) {
        if let Some(t) = self.current_tab() {
            self.location_entry
                .set_text(&display_location(&t.location()));
        }
        self.path_stack.set_visible_child_name("entry");
        if focus {
            self.location_entry.grab_focus();
            self.location_entry.select_region(0, -1);
        }
    }

    fn hide_location_entry(&self) {
        if !self.shared.view_settings().location_entry {
            self.path_stack.set_visible_child_name("crumbs");
        }
        if let Some(t) = self.current_tab() {
            t.focus_view();
        }
    }

    /// Attaches a file drop target to `widget`, dropping into `dest()`.
    pub fn attach_drop(
        self: &Rc<Self>,
        widget: &gtk::Widget,
        dest: impl Fn() -> gio::File + 'static,
    ) {
        let target = gtk::DropTarget::new(
            gdk::FileList::static_type(),
            gdk::DragAction::COPY | gdk::DragAction::MOVE,
        );
        // Load the file list while hovering so the action can depend on it.
        target.set_preload(true);
        let dest = Rc::new(dest);
        let d = dest.clone();
        target.connect_motion(move |t, _, _| preferred_drop_action(t, None, &d()));
        let weak = Rc::downgrade(self);
        target.connect_drop(move |t, value, _, _| {
            let (Some(w), Ok(list)) = (weak.upgrade(), value.get::<gdk::FileList>()) else {
                return false;
            };
            let files = list.files();
            let dst = dest();
            let action = preferred_drop_action(t, Some(&files), &dst);
            w.drop_files(files, &dst, action)
        });
        widget.add_controller(target);
    }

    // ----- navigation -----

    pub fn navigate(self: &Rc<Self>, file: &gio::File) {
        match self.current_tab() {
            Some(t) => {
                // Leave search when navigating.
                if self.search_bar.is_search_mode() {
                    self.search_bar.set_search_mode(false);
                }
                t.load(file, true, vec![]);
                t.focus_view();
            }
            None => self.open_tab(file, vec![], true),
        }
    }

    /// Navigates to `file`; regular files open their folder with the file
    /// selected.
    pub fn navigate_smart(self: &Rc<Self>, file: gio::File) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let info = file
                .query_info_future(
                    "standard::type",
                    gio::FileQueryInfoFlags::NONE,
                    glib::Priority::DEFAULT,
                )
                .await;
            let Some(w) = weak.upgrade() else { return };
            match info {
                Ok(i) if i.file_type() == gio::FileType::Regular => {
                    if let (Some(parent), Some(t)) = (file.parent(), w.current_tab()) {
                        t.load(&parent, true, vec![file.clone()]);
                    }
                }
                _ => w.navigate(&file),
            }
        });
    }

    fn go_back(self: &Rc<Self>) {
        let Some(t) = self.current_tab() else { return };
        let from = t.location();
        let target = t.history.borrow_mut().go_back();
        if let Some(uri) = target {
            t.load(&gio::File::for_uri(&uri), false, vec![from]);
        }
    }

    fn go_forward(self: &Rc<Self>) {
        let Some(t) = self.current_tab() else { return };
        let target = t.history.borrow_mut().go_forward();
        if let Some(uri) = target {
            t.load(&gio::File::for_uri(&uri), false, vec![]);
        }
    }

    fn go_up(self: &Rc<Self>) {
        let Some(t) = self.current_tab() else { return };
        let from = t.location();
        if let Some(parent) = from.parent() {
            t.load(&parent, true, vec![from]);
        }
    }

    /// Opens items: folders navigate (or open in tabs/windows), files
    /// launch their default application.
    pub fn open_infos(self: &Rc<Self>, infos: &[gio::FileInfo], mode: OpenIn) {
        let mut dirs = Vec::new();
        let mut files = Vec::new();
        for info in infos {
            let Some(target) = info_target(info) else {
                continue;
            };
            if info_is_dir(info) {
                dirs.push(target);
            } else {
                files.push((target, info));
            }
        }
        match (mode, dirs.len()) {
            (_, 0) => {}
            (OpenIn::Current, 1) => self.navigate(&dirs[0]),
            (OpenIn::NewWindow, _) => {
                for d in dirs {
                    Window::new(&self.shared, Some(d), vec![]).window.present();
                }
            }
            _ => {
                for d in dirs {
                    self.open_tab(&d, vec![], false);
                }
            }
        }
        for (f, info) in files {
            self.launch(&f, info);
        }
    }

    /// Opens `file` (listed as `info`) in the default application for
    /// its content type, as GNOME Files does: GIO hands the application
    /// the file's path (for a share, its GVfs FUSE path) or its URI.
    /// Without a default application, the Open With dialog.
    ///
    /// Not `GtkFileLauncher`: it goes through the OpenURI portal, which
    /// asks which application to use for every type until one has been
    /// chosen a few times, and for a remote file looks for a handler of
    /// its URI scheme (`smb`) rather than a viewer, then gives up silently.
    fn launch(self: &Rc<Self>, file: &gio::File, info: &gio::FileInfo) {
        let content_type = info
            .content_type()
            .or_else(|| info.attribute_string("standard::fast-content-type"))
            .map(|s| s.to_string())
            .unwrap_or_else(|| "application/octet-stream".into());
        let Some(app) = gio::AppInfo::default_for_type(&content_type, false) else {
            super::dialogs::open_with(self, vec![info.clone()]);
            return;
        };
        let ctx = WidgetExt::display(&self.window).app_launch_context();
        match app.launch(std::slice::from_ref(file), Some(&ctx)) {
            Ok(()) => {
                gtk::RecentManager::default().add_item(&file.uri());
            }
            Err(e) => self.toast(&trf!(
                "Could not open file: {}",
                helix_app_common::errors::glib_detail(&e)
            )),
        }
    }

    pub fn mount_volume(self: &Rc<Self>, vol: &gio::Volume, new_tab: bool) {
        let op = gtk::MountOperation::new(Some(&self.window));
        let weak = Rc::downgrade(self);
        let v = vol.clone();
        vol.mount(
            gio::MountMountFlags::NONE,
            Some(&op),
            None::<&gio::Cancellable>,
            move |res| {
                let Some(w) = weak.upgrade() else { return };
                match res {
                    Ok(()) => {
                        if let Some(root) = v.get_mount().map(|m| m.root()) {
                            if new_tab {
                                w.open_tab(&root, vec![], true);
                            } else {
                                w.navigate(&root);
                            }
                        }
                    }
                    Err(e) if e.matches(gio::IOErrorEnum::FailedHandled) => {}
                    Err(e) => w.toast(&trf!(
                        "Unable to mount “{}”: {}",
                        v.name(),
                        helix_app_common::errors::glib_detail(&e)
                    )),
                }
            },
        );
    }

    pub fn unmount(self: &Rc<Self>, mount: &gio::Mount) {
        let op = gtk::MountOperation::new(Some(&self.window));
        // Leave the mount first so it is not busy.
        let root = mount.root();
        for (_, t) in self.tabs.borrow().iter() {
            let loc = t.location();
            if loc.equal(&root) || loc.has_prefix(&root) {
                t.load(&home(), true, vec![]);
            }
        }
        let weak = Rc::downgrade(self);
        let name = mount.name();
        let cb = move |res: Result<(), glib::Error>| {
            if let (Some(w), Err(e)) = (weak.upgrade(), res) {
                if !e.matches(gio::IOErrorEnum::FailedHandled) {
                    w.toast(&trf!(
                        "Unable to eject “{name}”: {error}",
                        name = name,
                        error = helix_app_common::errors::glib_detail(&e)
                    ));
                }
            }
        };
        if mount.can_eject() {
            mount.eject_with_operation(
                gio::MountUnmountFlags::NONE,
                Some(&op),
                None::<&gio::Cancellable>,
                cb,
            );
        } else {
            mount.unmount_with_operation(
                gio::MountUnmountFlags::NONE,
                Some(&op),
                None::<&gio::Cancellable>,
                cb,
            );
        }
    }

    // ----- clipboard -----

    fn set_clipboard(&self, files: Vec<gio::File>, cut: bool) {
        if files.is_empty() {
            return;
        }
        let uris: Vec<String> = files.iter().map(|f| f.uri().to_string()).collect();
        let text: Vec<String> = files.iter().map(|f| f.parse_name().to_string()).collect();
        let providers = [
            gdk::ContentProvider::for_value(&gdk::FileList::from_array(&files).to_value()),
            gdk::ContentProvider::for_bytes(
                "x-special/gnome-copied-files",
                &glib::Bytes::from_owned(
                    crate::clipformat::format_copied_files(cut, &uris).into_bytes(),
                ),
            ),
            gdk::ContentProvider::for_bytes(
                "text/plain;charset=utf-8",
                &glib::Bytes::from_owned(text.join("\n").into_bytes()),
            ),
        ];
        let _ = self
            .window
            .clipboard()
            .set_content(Some(&gdk::ContentProvider::new_union(&providers)));
        *self.shared.clipboard.borrow_mut() = Some(ClipState { files, cut });
        self.cut_changed();
    }

    /// Shows or clears the "cut" dimming in every tab.
    fn cut_changed(&self) {
        for w in self.shared.windows.borrow().iter() {
            for (_, t) in w.tabs.borrow().iter() {
                t.clipboard_changed();
            }
        }
    }

    async fn read_clipboard(&self) -> Option<ClipState> {
        let clipboard = self.window.clipboard();
        if clipboard.is_local() {
            if let Some(c) = self.shared.clipboard.borrow().clone() {
                return Some(c);
            }
        }
        let formats = clipboard.formats();
        if formats.contain_mime_type("x-special/gnome-copied-files") {
            if let Ok((stream, _)) = clipboard
                .read_future(&["x-special/gnome-copied-files"], glib::Priority::DEFAULT)
                .await
            {
                let mut data = Vec::new();
                while let Ok(bytes) = stream
                    .read_bytes_future(65536, glib::Priority::DEFAULT)
                    .await
                {
                    if bytes.is_empty() {
                        break;
                    }
                    data.extend_from_slice(&bytes);
                }
                if let Some((cut, uris)) =
                    crate::clipformat::parse_copied_files(&String::from_utf8_lossy(&data))
                {
                    return Some(ClipState {
                        files: uris.iter().map(|u| gio::File::for_uri(u)).collect(),
                        cut,
                    });
                }
            }
        }
        if let Ok(v) = clipboard
            .read_value_future(gdk::FileList::static_type(), glib::Priority::DEFAULT)
            .await
        {
            if let Ok(list) = v.get::<gdk::FileList>() {
                let files = list.files();
                if !files.is_empty() {
                    return Some(ClipState { files, cut: false });
                }
            }
        }
        None
    }

    fn paste(self: &Rc<Self>) {
        let Some(tab) = self.current_tab() else {
            return;
        };
        let dest = tab.location();
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(w) = weak.upgrade() else { return };
            let Some(clip) = w.read_clipboard().await else {
                w.toast(tr("Nothing to paste"));
                return;
            };
            if clip.cut {
                // A cut can only be pasted once.
                w.window
                    .clipboard()
                    .set_content(None::<&gdk::ContentProvider>)
                    .ok();
                *w.shared.clipboard.borrow_mut() = None;
                w.cut_changed();
                w.start_job(
                    Job::Move {
                        sources: clip.files,
                        dest: dest.clone(),
                    },
                    Some(tab),
                );
            } else {
                w.start_job(
                    Job::Copy {
                        sources: clip.files,
                        dest: dest.clone(),
                    },
                    Some(tab),
                );
            }
        });
    }

    // ----- operations -----

    /// Runs `job`; when it finishes, created items are selected in `tab`.
    pub fn start_job(self: &Rc<Self>, job: Job, tab: Option<Rc<Tab>>) {
        let weak_tab = tab.as_ref().map(Rc::downgrade);
        let is_trash = matches!(job, Job::Trash(_));
        let weak = Rc::downgrade(self);
        jobs::start(self, job, move |out| {
            let Some(w) = weak.upgrade() else { return };
            if let Some(t) = weak_tab.and_then(|t| t.upgrade()) {
                if !out.created.is_empty() {
                    let in_view: Vec<gio::File> = out
                        .created
                        .iter()
                        .filter(|f| f.parent().is_some_and(|p| p.equal(&t.location())))
                        .cloned()
                        .collect();
                    if !in_view.is_empty() {
                        t.select_when_listed(in_view);
                    }
                }
            }
            if !out.moved.is_empty() {
                w.shared.stars.moved(&out.moved);
            }
            // Starred has no folder to watch: read it again.
            let tabs: Vec<Rc<Tab>> = w.tabs.borrow().iter().map(|(_, t)| t.clone()).collect();
            for t in tabs.iter().filter(|t| t.is_starred()) {
                t.reload();
            }
            if is_trash && !out.trashed.is_empty() {
                w.shared
                    .undo
                    .borrow_mut()
                    .push(Undo::Trash(out.trashed.clone()));
                let msg = if out.trashed.len() == 1 {
                    trf!(
                        "“{}” moved to Trash",
                        crate::ops::display_name(&out.trashed[0])
                    )
                } else {
                    trnf!(
                        "{n} item moved to Trash",
                        "{n} items moved to Trash",
                        out.trashed.len()
                    )
                };
                let toast = adw::Toast::builder()
                    .title(msg)
                    .button_label(tr("_Undo"))
                    .action_name("win.undo")
                    .use_markup(false)
                    .build();
                w.add_toast(toast);
            }
            if !out.untrashable.is_empty() {
                w.offer_delete(out.untrashable.clone());
            }
        });
    }

    /// `files` could not be put in the Trash (their filesystem has none):
    /// offer to delete them permanently, as Nautilus does.
    fn offer_delete(self: &Rc<Self>, files: Vec<gio::File>) {
        let weak = Rc::downgrade(self);
        let heading = if files.len() == 1 {
            trf!(
                "“{}” Can’t Be Put in the Trash",
                crate::ops::display_name(&files[0])
            )
        } else {
            trnf!(
                "{n} Item Can’t Be Put in the Trash",
                "{n} Items Can’t Be Put in the Trash",
                files.len()
            )
        };
        dialogs::confirm(
            &self.window,
            &heading,
            tr("This location has no Trash. Delete permanently instead? Deleted items will be permanently lost."),
            tr("_Delete"),
            move || {
                if let Some(w) = weak.upgrade() {
                    w.start_job(Job::Delete(files.clone()), None);
                }
            },
        );
    }

    pub fn drop_files(
        self: &Rc<Self>,
        files: Vec<gio::File>,
        dest: &gio::File,
        action: gdk::DragAction,
    ) -> bool {
        if files.is_empty() || dest.uri() == OTHER_LOCATIONS {
            return false;
        }
        // Dropping onto the folder the items already live in does nothing.
        if action == gdk::DragAction::MOVE
            && files
                .iter()
                .all(|f| f.parent().is_some_and(|p| p.equal(dest)))
        {
            return false;
        }
        if files.iter().any(|f| f.equal(dest)) {
            return false;
        }
        let tab = self.current_tab().filter(|t| t.location().equal(dest));
        let job = if dest.uri_scheme().as_deref() == Some("trash") {
            Job::Trash(files)
        } else if action == gdk::DragAction::MOVE {
            Job::Move {
                sources: files,
                dest: dest.clone(),
            }
        } else {
            Job::Copy {
                sources: files,
                dest: dest.clone(),
            }
        };
        self.start_job(job, tab);
        true
    }

    fn trash_selection(self: &Rc<Self>) {
        let Some(tab) = self.current_tab() else {
            return;
        };
        if tab.is_trash() {
            return self.delete_selection();
        }
        let files = tab.selected_files();
        if !files.is_empty() {
            self.start_job(Job::Trash(files), Some(tab));
        }
    }

    fn delete_selection(self: &Rc<Self>) {
        let Some(tab) = self.current_tab() else {
            return;
        };
        let files = tab.selected_files();
        if files.is_empty() {
            return;
        }
        let weak = Rc::downgrade(self);
        let heading = if files.len() == 1 {
            trf!(
                "Permanently Delete “{}”?",
                crate::ops::display_name(&files[0])
            )
        } else {
            trnf!(
                "Permanently Delete {n} Item?",
                "Permanently Delete {n} Items?",
                files.len()
            )
        };
        dialogs::confirm(
            &self.window,
            &heading,
            tr("If you delete an item, it will be permanently lost."),
            tr("_Delete"),
            move || {
                if let Some(w) = weak.upgrade() {
                    w.start_job(Job::Delete(files.clone()), None);
                }
            },
        );
    }

    fn empty_trash(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        dialogs::confirm(
            &self.window,
            tr("Empty Trash?"),
            tr("All items in the Trash will be permanently deleted."),
            tr("_Empty Trash"),
            move || {
                if let Some(w) = weak.upgrade() {
                    w.start_job(Job::EmptyTrash, None);
                }
            },
        );
    }

    fn undo(self: &Rc<Self>) {
        let Some(u) = self.shared.undo.borrow_mut().pop() else {
            self.toast(tr("Nothing to undo"));
            return;
        };
        match u {
            Undo::Trash(files) => {
                let tab = self.current_tab();
                self.start_job(Job::Untrash(files), tab);
            }
            Undo::Rename { file, old_name } => {
                let weak = Rc::downgrade(self);
                glib::spawn_future_local(async move {
                    let res = file
                        .set_display_name_future(&old_name, glib::Priority::DEFAULT)
                        .await;
                    let Some(w) = weak.upgrade() else { return };
                    match res {
                        Ok(back) => w.shared.stars.moved(&[(file, back)]),
                        Err(e) => w.toast(&trf!(
                            "Could not undo rename: {}",
                            helix_app_common::errors::glib_detail(&e)
                        )),
                    }
                });
            }
        }
    }

    fn rename_selection(self: &Rc<Self>) {
        let Some(tab) = self.current_tab() else {
            return;
        };
        let infos = tab.selected_infos();
        let [info] = infos.as_slice() else { return };
        let Some(file) = info_file(info) else { return };
        let old = Some(info.edit_name().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| info.display_name().to_string());
        let is_dir = info_is_dir(info);
        let weak = Rc::downgrade(self);
        let parent = file.parent();
        let undo_name = old.clone();
        dialogs::name_dialog(
            &self.window,
            if is_dir {
                tr("Rename Folder")
            } else {
                tr("Rename File")
            },
            tr("_Rename"),
            &old,
            is_dir,
            parent,
            move |new_name| {
                let Some(w) = weak.upgrade() else { return };
                let file = file.clone();
                let old = undo_name.clone();
                let tab = tab.clone();
                glib::spawn_future_local(async move {
                    match file
                        .set_display_name_future(&new_name, glib::Priority::DEFAULT)
                        .await
                    {
                        Ok(new_file) => {
                            w.shared.stars.moved(&[(file.clone(), new_file.clone())]);
                            if tab.is_starred() {
                                tab.reload();
                            }
                            w.shared.undo.borrow_mut().push(Undo::Rename {
                                file: new_file.clone(),
                                old_name: old,
                            });
                            tab.select_when_listed(vec![new_file]);
                        }
                        Err(e) => w.toast(&trf!(
                            "Could not rename: {}",
                            helix_app_common::errors::glib_detail(&e)
                        )),
                    }
                });
            },
        );
    }

    fn new_folder(self: &Rc<Self>) {
        let Some(tab) = self.current_tab() else {
            return;
        };
        let dir = tab.location();
        let weak = Rc::downgrade(self);
        let d = dir.clone();
        dialogs::name_dialog(
            &self.window,
            tr("New Folder"),
            tr("_Create"),
            "",
            true,
            Some(dir),
            move |name| {
                let Some(w) = weak.upgrade() else { return };
                let f = d.child(&name);
                let tab = tab.clone();
                glib::spawn_future_local(async move {
                    match f.make_directory_future(glib::Priority::DEFAULT).await {
                        Ok(()) => tab.select_when_listed(vec![f]),
                        Err(e) => w.toast(&trf!(
                            "Could not create folder: {}",
                            helix_app_common::errors::glib_detail(&e)
                        )),
                    }
                });
            },
        );
    }

    fn add_bookmark(self: &Rc<Self>) {
        let Some(tab) = self.current_tab() else {
            return;
        };
        let loc = tab.location();
        if tab.is_other_locations() {
            return;
        }
        let uri = loc.uri().to_string();
        let mut list = crate::bookmarks::load();
        if list.iter().any(|b| b.uri == uri) {
            self.toast(tr("Location is already bookmarked"));
            return;
        }
        list.push(crate::bookmarks::Bookmark { uri, label: None });
        match crate::bookmarks::save(&list) {
            Ok(()) => self.toast(&trf!("“{}” added to bookmarks", location_title(&loc))),
            Err(e) => self.toast(&trf!(
                "Could not save bookmarks: {e}",
                e = helix_app_common::errors::io_detail(&e)
            )),
        }
        self.shared.windows.borrow().iter().for_each(|w| {
            if let Some(s) = w.sidebar.borrow().as_ref() {
                s.rebuild();
            }
        });
    }

    // ----- context menus -----

    pub fn item_menu(&self, tab: &Tab) -> gio::Menu {
        let infos = tab.selected_infos();
        let s = self.shared.view_settings();
        let menu = gio::Menu::new();
        let dirs = !infos.is_empty() && infos.iter().all(info_is_dir);
        if tab.is_trash() {
            let sec = gio::Menu::new();
            sec.append(Some(tr("_Restore From Trash")), Some("win.restore"));
            sec.append(Some(tr("_Delete From Trash…")), Some("win.delete"));
            menu.append_section(None, &sec);
            let sec = gio::Menu::new();
            sec.append(Some(tr("_Properties")), Some("win.properties"));
            menu.append_section(None, &sec);
            return menu;
        }
        let sec = gio::Menu::new();
        sec.append(Some(tr("_Open")), Some("win.open"));
        if dirs {
            sec.append(Some(tr("Open in New _Tab")), Some("win.open-new-tab"));
            sec.append(Some(tr("Open in New _Window")), Some("win.open-new-window"));
        } else {
            sec.append(Some(tr("Open _With…")), Some("win.open-with"));
        }
        if tab.is_searching()
            || tab.is_starred()
            || tab.location().uri_scheme().as_deref() == Some("recent")
        {
            sec.append(Some(tr("Open Item _Location")), Some("win.open-location"));
        }
        menu.append_section(None, &sec);
        let (star, unstar) = self.star_choices(&tab.selected_files());
        if star || unstar {
            let sec = gio::Menu::new();
            if star {
                sec.append(Some(tr("_Star")), Some("win.star"));
            } else {
                sec.append(Some(tr("_Unstar")), Some("win.unstar"));
            }
            menu.append_section(None, &sec);
        }
        let sec = gio::Menu::new();
        sec.append(Some(tr("Cu_t")), Some("win.cut"));
        sec.append(Some(tr("_Copy")), Some("win.copy"));
        if dirs && infos.len() == 1 {
            sec.append(Some(tr("Paste Into Folder")), Some("win.paste-into"));
        }
        menu.append_section(None, &sec);
        let sec = gio::Menu::new();
        sec.append(Some(tr("Rena_me…")), Some("win.rename"));
        if s.show_link {
            sec.append(Some(tr("Create _Link")), Some("win.create-link"));
        }
        sec.append(Some(tr("Mo_ve to Trash")), Some("win.trash"));
        if s.show_delete {
            sec.append(Some(tr("_Delete Permanently…")), Some("win.delete"));
        }
        menu.append_section(None, &sec);
        let sec = gio::Menu::new();
        sec.append(Some(tr("Copy Location")), Some("win.copy-location"));
        sec.append(Some(tr("_Properties")), Some("win.properties"));
        menu.append_section(None, &sec);
        menu
    }

    pub fn background_menu(&self, tab: &Tab) -> gio::Menu {
        let menu = gio::Menu::new();
        if tab.is_starred() {
            let sec = gio::Menu::new();
            sec.append(Some(tr("Select _All")), Some("win.select-all"));
            menu.append_section(None, &sec);
            return menu;
        }
        if tab.is_trash() {
            let sec = gio::Menu::new();
            sec.append(Some(tr("_Empty Trash…")), Some("win.empty-trash"));
            sec.append(Some(tr("Select _All")), Some("win.select-all"));
            menu.append_section(None, &sec);
            return menu;
        }
        let sec = gio::Menu::new();
        sec.append(Some(tr("New _Folder…")), Some("win.new-folder"));
        menu.append_section(None, &sec);
        let sec = gio::Menu::new();
        sec.append(Some(tr("_Paste")), Some("win.paste"));
        sec.append(Some(tr("Select _All")), Some("win.select-all"));
        menu.append_section(None, &sec);
        let sec = gio::Menu::new();
        sec.append(Some(tr("Add to _Bookmarks")), Some("win.bookmark"));
        sec.append(Some(tr("Show _Hidden Files")), Some("win.show-hidden"));
        sec.append(Some(tr("_Properties")), Some("win.folder-properties"));
        menu.append_section(None, &sec);
        menu
    }

    // ----- actions -----

    fn install_actions(self: &Rc<Self>) {
        let win = &self.window;
        let add = |name: &str, f: fn(&Rc<Window>)| {
            let a = gio::SimpleAction::new(name, None);
            let weak = Rc::downgrade(self);
            a.connect_activate(move |_, _| {
                if let Some(w) = weak.upgrade() {
                    f(&w);
                }
            });
            win.add_action(&a);
        };
        add("back", |w| w.go_back());
        add("forward", |w| w.go_forward());
        add("up", |w| w.go_up());
        add("home", |w| w.navigate(&home()));
        add("reload", |w| {
            if let Some(t) = w.current_tab() {
                t.reload();
            }
        });
        add("location", |w| w.show_location_entry(true));
        add("search", |w| {
            let on = !w.search_bar.is_search_mode();
            w.search_bar.set_search_mode(on);
            if on {
                w.search_entry.grab_focus();
            }
        });
        add("new-tab", |w| {
            let loc = w.current_tab().map(|t| t.location()).unwrap_or_else(home);
            w.open_tab(&loc, vec![], true);
        });
        add("close-tab", |w| {
            if w.tab_view.n_pages() <= 1 {
                w.window.close();
            } else if let Some(p) = w.tab_view.selected_page() {
                w.tab_view.close_page(&p);
            }
        });
        add("toggle-sidebar", |w| {
            w.split.set_show_sidebar(!w.split.shows_sidebar())
        });
        add("toggle-view", |w| {
            let list = w.shared.view_settings().list_view;
            w.set_view_mode(!list);
        });
        add("zoom-in", |w| w.zoom(1));
        add("zoom-out", |w| w.zoom(-1));
        add("zoom-reset", |w| w.zoom(0));
        add("bookmark", |w| w.add_bookmark());
        add("new-folder", |w| w.new_folder());
        add("rename", |w| w.rename_selection());
        add("trash", |w| w.trash_selection());
        add("delete", |w| w.delete_selection());
        add("empty-trash", |w| w.empty_trash());
        add("undo", |w| w.undo());
        add("star", |w| w.star_selection(true));
        add("unstar", |w| w.star_selection(false));
        add("show-shortcuts", |w| super::shortcuts::show(&w.window));
        add("restore", |w| {
            if let Some(t) = w.current_tab() {
                let files = t.selected_files();
                if !files.is_empty() {
                    w.start_job(Job::Restore(files), None);
                }
            }
        });
        add("create-link", |w| {
            if let Some(t) = w.current_tab() {
                let files = t.selected_files();
                if !files.is_empty() {
                    w.start_job(
                        Job::Link {
                            sources: files,
                            dest: t.location(),
                        },
                        Some(t),
                    );
                }
            }
        });
        add("cut", |w| {
            if let Some(t) = w.current_tab() {
                w.set_clipboard(t.selected_files(), true);
            }
        });
        add("copy", |w| {
            if let Some(t) = w.current_tab() {
                w.set_clipboard(t.selected_files(), false);
            }
        });
        add("paste", |w| w.paste());
        add("paste-into", |w| {
            let Some(t) = w.current_tab() else { return };
            let Some(dir) = t.selected_infos().first().and_then(info_target) else {
                return;
            };
            let weak = Rc::downgrade(w);
            glib::spawn_future_local(async move {
                let Some(w) = weak.upgrade() else { return };
                let Some(clip) = w.read_clipboard().await else {
                    return;
                };
                if clip.cut {
                    w.window
                        .clipboard()
                        .set_content(None::<&gdk::ContentProvider>)
                        .ok();
                    *w.shared.clipboard.borrow_mut() = None;
                    w.cut_changed();
                    w.start_job(
                        Job::Move {
                            sources: clip.files,
                            dest: dir,
                        },
                        None,
                    );
                } else {
                    w.start_job(
                        Job::Copy {
                            sources: clip.files,
                            dest: dir,
                        },
                        None,
                    );
                }
            });
        });
        add("copy-location", |w| {
            if let Some(t) = w.current_tab() {
                let text: Vec<String> = t.selected_files().iter().map(display_location).collect();
                w.window.clipboard().set_text(&text.join("\n"));
            }
        });
        add("select-all", |w| {
            if let Some(t) = w.current_tab() {
                t.select_all();
            }
        });
        add("invert-selection", |w| {
            if let Some(t) = w.current_tab() {
                t.invert_selection();
            }
        });
        add("open", |w| {
            if let Some(t) = w.current_tab() {
                w.open_infos(&t.selected_infos(), OpenIn::Current);
            }
        });
        add("open-new-tab", |w| {
            if let Some(t) = w.current_tab() {
                w.open_infos(&t.selected_infos(), OpenIn::NewTab);
            }
        });
        add("open-new-window", |w| {
            if let Some(t) = w.current_tab() {
                w.open_infos(&t.selected_infos(), OpenIn::NewWindow);
            }
        });
        add("open-location", |w| {
            let Some(t) = w.current_tab() else { return };
            if let Some(f) = t.selected_infos().first().and_then(info_target) {
                if let Some(parent) = f.parent() {
                    if w.search_bar.is_search_mode() {
                        w.search_bar.set_search_mode(false);
                    }
                    t.load(&parent, true, vec![f]);
                }
            }
        });
        add("open-with", |w| {
            if let Some(t) = w.current_tab() {
                let infos = t.selected_infos();
                if !infos.is_empty() {
                    dialogs::open_with(w, infos);
                }
            }
        });
        add("properties", |w| {
            let Some(t) = w.current_tab() else { return };
            let files = t.selected_files();
            if files.is_empty() {
                if !t.is_other_locations() && !t.is_starred() {
                    properties::show(w, vec![t.location()]);
                }
            } else {
                properties::show(w, files);
            }
        });
        add("folder-properties", |w| {
            if let Some(t) = w.current_tab() {
                properties::show(w, vec![t.location()]);
            }
        });
        add("context-menu", |w| {
            if let Some(t) = w.current_tab() {
                t.popup_selection_menu();
            }
        });

        // Stateful actions.
        let s = self.shared.view_settings();
        let show_hidden =
            gio::SimpleAction::new_stateful("show-hidden", None, &s.show_hidden.to_variant());
        let weak = Rc::downgrade(self);
        show_hidden.connect_activate(move |a, _| {
            let Some(w) = weak.upgrade() else { return };
            let v = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            w.shared.prefs.set("show-hidden-files", v);
            w.shared.settings_changed();
        });
        win.add_action(&show_hidden);

        let sort = gio::SimpleAction::new_stateful(
            "sort",
            Some(glib::VariantTy::STRING),
            &s.sort_key.nick().to_variant(),
        );
        let weak = Rc::downgrade(self);
        sort.connect_activate(move |_, v| {
            let (Some(w), Some(nick)) = (weak.upgrade(), v.and_then(|v| v.get::<String>())) else {
                return;
            };
            w.shared.prefs.set_enum_nick("default-sort-order", &nick);
            w.shared.settings_changed();
        });
        win.add_action(&sort);

        let rev = gio::SimpleAction::new_stateful("sort-reversed", None, &s.reversed.to_variant());
        let weak = Rc::downgrade(self);
        rev.connect_activate(move |a, _| {
            let Some(w) = weak.upgrade() else { return };
            let v = !a.state().and_then(|s| s.get::<bool>()).unwrap_or(false);
            w.shared.prefs.set("default-sort-in-reverse-order", v);
            w.shared.settings_changed();
        });
        win.add_action(&rev);

        let mode = gio::SimpleAction::new_stateful(
            "view-mode",
            Some(glib::VariantTy::STRING),
            &(if s.list_view { "list" } else { "grid" }).to_variant(),
        );
        let weak = Rc::downgrade(self);
        mode.connect_activate(move |_, v| {
            let (Some(w), Some(m)) = (weak.upgrade(), v.and_then(|v| v.get::<String>())) else {
                return;
            };
            w.set_view_mode(m == "list");
        });
        win.add_action(&mode);
    }

    fn set_view_mode(&self, list: bool) {
        self.shared.prefs.set_enum_nick(
            "default-folder-viewer",
            if list { "list-view" } else { "icon-view" },
        );
        self.shared.settings_changed();
        if let Some(t) = self.current_tab() {
            t.focus_view();
        }
    }

    /// `delta` > 0 zooms in, < 0 out, 0 resets.
    fn zoom(&self, delta: i32) {
        let s = self.shared.view_settings();
        let (prefs, table, current, default): (
            &helix_app_common::Prefs,
            &[(&str, i32)],
            usize,
            usize,
        ) = if s.list_view {
            (&self.shared.list_prefs, &LIST_ZOOM, s.list_zoom, 1)
        } else {
            (&self.shared.icon_prefs, &GRID_ZOOM, s.grid_zoom, 2)
        };
        let next = match delta {
            0 => default,
            d if d > 0 => (current + 1).min(table.len() - 1),
            _ => current.saturating_sub(1),
        };
        prefs.set_enum_nick("default-zoom-level", table[next].0);
        self.shared.settings_changed();
    }
}

/// What the location entry shows: a path for local files, else the URI.
/// How a location is written for people: a local file's path, else its
/// URI. (Not `path()` for remote ones: for a GVfs location that asks the
/// GVfs daemon over D-Bus, synchronously, and the sidebar does this for
/// every place while a window opens; it held a new window for seconds.
/// Their FUSE path is no better to read than the URI either.)
pub fn display_location(f: &gio::File) -> String {
    match local_path(f) {
        Some(p) => p.display().to_string(),
        None => f.uri().to_string(),
    }
}

/// A local file's path, without asking anyone: `None` for remote (GVfs)
/// locations, whose `path()` is a synchronous D-Bus call to the GVfs
/// daemon (their FUSE path), which must not run on the main thread for
/// every place, crumb or title.
pub fn local_path(f: &gio::File) -> Option<std::path::PathBuf> {
    if f.is_native() {
        f.path()
    } else {
        None
    }
}

fn expand_tilde(text: &str) -> String {
    let home = glib::home_dir();
    match text.strip_prefix('~') {
        Some("") => home.display().to_string(),
        Some(rest) if rest.starts_with('/') => format!("{}{rest}", home.display()),
        _ => text.to_string(),
    }
}
