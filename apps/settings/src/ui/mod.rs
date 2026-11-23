//! GTK user interface: window, sidebar, search and the panel registry.

use helix_app_common::i18n::tr;
use helix_app_common::trf;
use helix_app_common::trnf;

mod about;
mod access;
mod agent_access;
mod appearance;
mod apps;
mod bluetooth;
mod datetime;
mod displays;
mod extensions;
mod greeter;
mod input;
mod multitasking;
mod netedit;
mod network;
mod notifications;
mod picker;
mod power;
mod printers;
mod privacy;
mod shortcuts;
mod sound;
mod users;
mod widgets;
mod wifi;

use adw::prelude::*;
use gtk::{gio, glib};
use helix_app_common::Labelled;
use helix_app_common::Prefs;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

use crate::panels::{self, PanelId};

pub const APP_ID: &str = "org.helixdesktop.Settings";
const DEFAULT_PANEL: PanelId = PanelId::Appearance;

/// A settings panel implementation.
pub trait Panel {
    /// Builds the panel's content (usually an `AdwPreferencesPage`).
    fn build(&self, ctx: &Rc<Ctx>) -> gtk::Widget;

    /// Whether the content brings its own header bars (an
    /// `AdwNavigationView` of sub-pages), so none is added around it.
    fn own_header(&self) -> bool {
        false
    }

    /// Shows the sub-page named `page` (a [`panels::PAGES`] name) in
    /// `content`, what [`Panel::build`] returned.
    fn show_page(&self, _ctx: &Rc<Ctx>, _content: &gtk::Widget, _page: &str) {}
}

/// Implementation registry: `None` for panels that are not available yet.
fn panel_impl(id: PanelId) -> Option<&'static dyn Panel> {
    Some(match id {
        PanelId::Appearance => &appearance::AppearancePanel,
        PanelId::Mouse => &input::MousePanel,
        PanelId::Keyboard => &input::KeyboardPanel,
        PanelId::Accessibility => &access::AccessibilityPanel,
        PanelId::Privacy => &privacy::PrivacyPanel,
        PanelId::Notifications => &notifications::NotificationsPanel,
        PanelId::Extensions => &extensions::ExtensionsPanel,
        PanelId::Multitasking => &multitasking::MultitaskingPanel,
        PanelId::Power => &power::PowerPanel,
        PanelId::DateTime => &datetime::DateTimePanel,
        PanelId::About => &about::AboutPanel,
        PanelId::Bluetooth => &bluetooth::BluetoothPanel,
        PanelId::Wifi => &wifi::WifiPanel,
        PanelId::Network => &network::NetworkPanel,
        PanelId::Sound => &sound::SoundPanel,
        PanelId::Display => &displays::DisplaysPanel,
        PanelId::Apps => &apps::AppsPanel,
        PanelId::Users => &users::UsersPanel,
        PanelId::LoginScreen => &greeter::LoginScreenPanel,
        PanelId::Printers => &printers::PrintersPanel,
    })
}

/// Shared state handed to panels.
pub struct Ctx {
    pub window: adw::ApplicationWindow,
    toasts: adw::ToastOverlay,
    prefs: RefCell<HashMap<&'static str, Prefs>>,
}

impl Ctx {
    /// Settings for `schema`, shared by every panel (so change handlers stay alive).
    pub fn prefs(&self, schema: &'static str) -> Prefs {
        self.prefs
            .borrow_mut()
            .entry(schema)
            .or_insert_with(|| Prefs::new(schema))
            .clone()
    }

    pub fn toast(&self, message: &str) {
        self.toasts
            .add_toast(adw::Toast::new(&glib::markup_escape_text(message)));
    }
}

/// Runs `work` on a worker thread and `done` with its result on the GTK thread.
pub fn spawn_worker<T: Send + 'static>(
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(T) + 'static,
) {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(work());
    });
    glib::spawn_future_local(async move {
        if let Ok(v) = rx.recv().await {
            done(v);
        }
    });
}

/// Keeps a panel's view of a service current: `watch` runs on a thread
/// and calls its argument for every change it sees (stopping when that
/// returns false); `changed` then runs on the GTK thread, at most once per
/// `settle`, while `state` lives.
pub fn follow<S: 'static>(
    state: &Rc<S>,
    settle: std::time::Duration,
    watch: impl FnOnce(&mut dyn FnMut() -> bool) + Send + 'static,
    changed: impl Fn(&Rc<S>) + 'static,
) {
    let (tx, rx) = async_channel::unbounded::<()>();
    std::thread::spawn(move || {
        let mut tell = move || tx.send_blocking(()).is_ok();
        watch(&mut tell);
    });
    let weak = Rc::downgrade(state);
    glib::spawn_future_local(async move {
        while rx.recv().await.is_ok() {
            // Changes closer together than `settle` are read once.
            glib::timeout_future(settle).await;
            while rx.try_recv().is_ok() {}
            let Some(state) = weak.upgrade() else { break };
            changed(&state);
        }
    });
}

struct Window {
    ctx: Rc<Ctx>,
    split: adw::NavigationSplitView,
    list: gtk::ListBox,
    /// The panel list, or "No Results Found" for a search without matches.
    list_stack: gtk::Stack,
    search_entry: gtk::SearchEntry,
    rows: Vec<(PanelId, gtk::ListBoxRow)>,
    pages: RefCell<HashMap<PanelId, adw::NavigationPage>>,
    searching: RefCell<bool>,
    /// Panels not shown on this system: the Login Screen until the check
    /// says it is (the check runs when the window is made).
    hidden: RefCell<Vec<PanelId>>,
    /// Whether that check has answered.
    checked: Cell<bool>,
    /// A hidden panel asked for before the check answered.
    deferred: RefCell<Option<(PanelId, Option<&'static str>)>>,
}

impl Window {
    fn new(app: &adw::Application) -> Rc<Self> {
        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title(tr("Settings"))
            .default_width(980)
            .default_height(680)
            .width_request(360)
            .height_request(294)
            .build();
        helix_app_common::window_state::persist(
            &window,
            helix_app_common::window_state::Store::file("helix-settings"),
        );
        let toasts = adw::ToastOverlay::new();
        let split = adw::NavigationSplitView::builder()
            .min_sidebar_width(220.0)
            .max_sidebar_width(280.0)
            .build();

        // Sidebar.
        let search_entry = gtk::SearchEntry::builder()
            .placeholder_text(tr("Search settings"))
            .hexpand(true)
            .build()
            .labelled(tr("Search Settings"));
        let search_bar = gtk::SearchBar::builder()
            .child(&search_entry)
            .show_close_button(false)
            .build();
        search_bar.connect_entry(&search_entry);
        let search_button = gtk::ToggleButton::builder()
            .icon_name("system-search-symbolic")
            .tooltip_text(tr("Search"))
            .build()
            .labelled(tr("Search"));
        search_button
            .bind_property("active", &search_bar, "search-mode-enabled")
            .bidirectional()
            .build();

        let menu = gio::Menu::new();
        menu.append(Some(tr("_Keyboard Shortcuts")), Some("win.show-shortcuts"));
        menu.append(Some(tr("_About Settings")), Some("app.about"));
        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu)
            .primary(true)
            .tooltip_text(tr("Main Menu"))
            .build()
            .labelled(tr("Main Menu"));
        let header = adw::HeaderBar::new();
        header.pack_start(&search_button);
        header.pack_end(&menu_button);

        let list = gtk::ListBox::builder()
            .css_classes(["navigation-sidebar"])
            .build();
        let mut rows = Vec::new();
        let hidden = vec![PanelId::LoginScreen];
        for id in panels::sidebar_order("") {
            let row = Self::make_row(panels::info(id));
            if !hidden.contains(&id) {
                list.append(&row);
            }
            rows.push((id, row));
        }
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        let no_results = adw::StatusPage::builder()
            .icon_name("system-search-symbolic")
            .title(tr("No Results Found"))
            .description(tr("Try a different search"))
            .build();
        no_results.add_css_class("compact");
        let list_stack = gtk::Stack::new();
        list_stack.add_named(&scrolled, Some("list"));
        list_stack.add_named(&no_results, Some("no-results"));
        let sidebar_view = adw::ToolbarView::new();
        sidebar_view.add_top_bar(&header);
        sidebar_view.add_top_bar(&search_bar);
        sidebar_view.set_content(Some(&list_stack));
        split.set_sidebar(Some(&adw::NavigationPage::new(
            &sidebar_view,
            tr("Settings"),
        )));

        toasts.set_child(Some(&split));
        window.set_content(Some(&toasts));

        let bp = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
            adw::BreakpointConditionLengthType::MaxWidth,
            700.0,
            adw::LengthUnit::Sp,
        ));
        bp.add_setter(&split, "collapsed", Some(&true.to_value()));
        window.add_breakpoint(bp);

        let ctx = Rc::new(Ctx {
            window: window.clone(),
            toasts,
            prefs: RefCell::default(),
        });
        let this = Rc::new(Window {
            ctx,
            split,
            list: list.clone(),
            list_stack,
            search_entry: search_entry.clone(),
            rows,
            pages: RefCell::default(),
            searching: RefCell::new(false),
            hidden: RefCell::new(hidden),
            checked: Cell::new(false),
            deferred: RefCell::default(),
        });
        // Whether the Login Screen panel is shown: files, then systemd on
        // the system bus (only with helix-greeter installed).
        let weak = Rc::downgrade(&this);
        spawn_worker(crate::greeter::panel_shown_here, move |shown| {
            let Some(this) = weak.upgrade() else { return };
            this.checked.set(true);
            if shown {
                this.hidden.borrow_mut().clear();
                this.filter(&this.search_entry.text());
            }
            if let Some(target) = this.deferred.take() {
                if shown {
                    this.show_target(target);
                }
            }
        });

        list.set_header_func(glib::clone!(
            #[weak(rename_to = this)]
            this,
            move |row, before| this.update_header(row, before)
        ));
        list.connect_row_activated(glib::clone!(
            #[weak(rename_to = this)]
            this,
            move |_, row| {
                if let Some(id) = this.id_of(row) {
                    this.show_panel(id);
                }
            }
        ));
        search_entry.connect_search_changed(glib::clone!(
            #[weak(rename_to = this)]
            this,
            move |e| this.filter(&e.text())
        ));
        search_entry.connect_activate(glib::clone!(
            #[weak(rename_to = this)]
            this,
            move |_| {
                // Enter opens the first (best) match.
                let first = this.list.row_at_index(0).and_then(|r| this.id_of(&r));
                if let Some(id) = first {
                    this.show_panel(id);
                }
            }
        ));
        search_entry.connect_stop_search(glib::clone!(
            #[weak]
            search_bar,
            move |e| {
                e.set_text("");
                search_bar.set_search_mode(false);
            }
        ));
        search_bar.connect_search_mode_enabled_notify(glib::clone!(
            #[weak]
            search_entry,
            move |bar| {
                if bar.is_search_mode() {
                    search_entry.grab_focus();
                } else {
                    search_entry.set_text("");
                }
            }
        ));
        search_bar.set_key_capture_widget(Some(&window));

        let search_action = gio::ActionEntry::builder("search")
            .activate(move |_: &adw::ApplicationWindow, _, _| search_bar.set_search_mode(true))
            .build();
        let shortcuts_action = gio::ActionEntry::builder("show-shortcuts")
            .activate(|win: &adw::ApplicationWindow, _, _| shortcuts::show(win))
            .build();
        window.add_action_entries([search_action, shortcuts_action]);
        for (action, keys) in ACCELS {
            app.set_accels_for_action(action, keys);
        }
        this
    }

    fn make_row(p: &panels::PanelInfo) -> gtk::ListBoxRow {
        let bx = gtk::Box::builder()
            .spacing(12)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(6)
            .margin_end(6)
            .build();
        bx.append(&gtk::Image::from_icon_name(p.icon));
        let labels = gtk::Box::new(gtk::Orientation::Vertical, 0);
        labels.set_valign(gtk::Align::Center);
        labels.append(&gtk::Label::builder().label(tr(p.title)).xalign(0.0).build());
        if !p.available {
            labels.append(
                &gtk::Label::builder()
                    .label(tr("Not yet available"))
                    .xalign(0.0)
                    .css_classes(["caption"])
                    .build(),
            );
            bx.add_css_class("dim-label");
        }
        bx.append(&labels);
        let row = gtk::ListBoxRow::builder().child(&bx).build();
        row.set_widget_name(p.name);
        helix_app_common::set_accessible_label(&row, tr(p.title));
        if !p.available {
            // Read as "Wi-Fi, Not yet available".
            row.update_property(&[gtk::accessible::Property::Description(tr(
                "Not yet available",
            ))]);
        }
        row
    }

    fn id_of(&self, row: &gtk::ListBoxRow) -> Option<PanelId> {
        self.rows.iter().find(|(_, r)| r == row).map(|(id, _)| *id)
    }

    fn update_header(&self, row: &gtk::ListBoxRow, before: Option<&gtk::ListBoxRow>) {
        let info = |r: &gtk::ListBoxRow| self.id_of(r).map(panels::info);
        let available = |r: &gtk::ListBoxRow| info(r).is_some_and(|i| i.available);
        // Placeholders come last, under their own heading.
        if !available(row) && before.is_none_or(available) {
            let is_heading = row
                .header()
                .is_some_and(|h| h.widget_name() == "placeholders-heading");
            if !is_heading {
                let heading = gtk::Label::builder()
                    .label(tr("Not Yet Available"))
                    .xalign(0.0)
                    .margin_top(if before.is_some() { 12 } else { 6 })
                    .margin_bottom(6)
                    .margin_start(12)
                    .css_classes(["heading", "dim-label"])
                    .name("placeholders-heading")
                    .build();
                row.set_header(Some(&heading));
            }
            return;
        }
        let group = |r: &gtk::ListBoxRow| info(r).map(|i| i.group);
        let needs = !*self.searching.borrow() && before.is_some_and(|b| group(b) != group(row));
        if needs {
            if !row.header().is_some_and(|h| h.is::<gtk::Separator>()) {
                row.set_header(Some(&gtk::Separator::new(gtk::Orientation::Horizontal)));
            }
        } else {
            row.set_header(None::<&gtk::Widget>);
        }
    }

    fn filter(&self, query: &str) {
        *self.searching.borrow_mut() = !query.trim().is_empty();
        let order = panels::sidebar_order_shown(query, &self.hidden.borrow());
        for (_, row) in &self.rows {
            if row.parent().is_some() {
                self.list.remove(row);
            }
        }
        let found = order.len();
        for id in order {
            if let Some((_, row)) = self.rows.iter().find(|(i, _)| *i == id) {
                self.list.append(row);
            }
        }
        self.list.invalidate_headers();
        self.list_stack
            .set_visible_child_name(if found == 0 { "no-results" } else { "list" });
        if !query.trim().is_empty() {
            let message = match found {
                0 => tr("No results found").to_string(),
                n => trnf!("{n} panel found", "{n} panels found", n),
            };
            helix_app_common::announce_settled(&self.search_entry, &message);
        }
    }

    fn show_panel(&self, id: PanelId) {
        let page = self.pages.borrow().get(&id).cloned();
        let page = page.unwrap_or_else(|| {
            let page = self.build_page(id);
            self.pages.borrow_mut().insert(id, page.clone());
            page
        });
        self.split.set_content(Some(&page));
        self.split.set_show_content(true);
        if let Some((_, row)) = self.rows.iter().find(|(i, _)| *i == id) {
            if row.parent().is_some() {
                self.list.select_row(Some(row));
            }
        }
    }

    /// Shows panel `id`, on its sub-page `page` when given. A hidden
    /// panel is not shown (the default one is, when there is none yet);
    /// asked for before the check answered, it is shown if the check
    /// says so.
    fn show_target(&self, (id, page): (PanelId, Option<&'static str>)) {
        if self.hidden.borrow().contains(&id) {
            if !self.checked.get() {
                *self.deferred.borrow_mut() = Some((id, page));
            }
            if self.split.content().is_none() {
                self.show_panel(DEFAULT_PANEL);
            }
            return;
        }
        self.show_panel(id);
        let Some(page) = page else { return };
        let content = self.pages.borrow().get(&id).and_then(|p| p.child());
        if let (Some(panel), Some(content)) = (panel_impl(id), content) {
            panel.show_page(&self.ctx, &content, page);
        }
    }

    fn build_page(&self, id: PanelId) -> adw::NavigationPage {
        let info = panels::info(id);
        if let Some(panel) = panel_impl(id).filter(|p| p.own_header()) {
            return adw::NavigationPage::builder()
                .title(tr(info.title))
                .tag(info.name)
                .child(&panel.build(&self.ctx))
                .build();
        }
        let view = adw::ToolbarView::new();
        view.add_top_bar(&adw::HeaderBar::new());
        let content: gtk::Widget = match panel_impl(id) {
            Some(panel) => panel.build(&self.ctx),
            None => adw::StatusPage::builder()
                .icon_name(info.icon)
                .title(trf!("{} Is Not Yet Available", tr(info.title)))
                .description(tr(
                    "This panel will be added in a later version of Helix Settings.",
                ))
                .build()
                .upcast(),
        };
        view.set_content(Some(&content));
        adw::NavigationPage::builder()
            .title(tr(info.title))
            .tag(info.name)
            .child(&view)
            .build()
    }
}

/// Application accelerators: (action, keys). The Keyboard Shortcuts
/// window lists every one of them (checked by a test in `shortcuts`).
pub const ACCELS: &[(&str, &[&str])] = &[
    ("win.search", &["<Control>f"]),
    ("win.show-shortcuts", &["<Control>question"]),
    ("app.about", &["F1"]),
];

pub fn run(args: Vec<String>) -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_COMMAND_LINE)
        .build();
    helix_app_common::install_standard_actions(
        &app,
        tr("Settings"),
        tr("Configure the Helix desktop"),
    );
    let window: Rc<RefCell<Option<Rc<Window>>>> = Rc::default();
    app.connect_command_line(move |app, cmd| {
        let panel = cmd
            .arguments()
            .get(1)
            .and_then(|a| a.to_str().map(str::to_string))
            .and_then(|a| panels::lookup_target(&a));
        let existing = window.borrow().clone();
        let win = match existing {
            Some(w) => {
                if let Some(target) = panel {
                    w.show_target(target);
                }
                w
            }
            None => {
                let w = Window::new(app);
                w.show_target(panel.unwrap_or((DEFAULT_PANEL, None)));
                *window.borrow_mut() = Some(w.clone());
                w
            }
        };
        win.ctx.window.present();
        0
    });
    app.run_with_args(&args)
}
