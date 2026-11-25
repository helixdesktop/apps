//! Dialogs: confirmation, naming, conflicts, Open With, preferences.

use adw::prelude::*;
use gtk::{gio, glib};
use std::rc::Rc;

use super::Window;
use crate::archive;
use crate::naming;
use crate::ops::{Choice, Conflict, Resolution};
use helix_app_common::i18n::{n_, tr};
use helix_app_common::trf;
use helix_app_common::trnf;
use helix_app_common::Labelled;

/// Destructive confirmation; `on_confirm` runs if accepted.
pub fn confirm(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    body: &str,
    accept: &str,
    on_confirm: impl Fn() + 'static,
) {
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .default_response("cancel")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", tr("_Cancel")), ("accept", accept)]);
    dialog.set_response_appearance("accept", adw::ResponseAppearance::Destructive);
    dialog.connect_response(Some("accept"), move |_, _| on_confirm());
    dialog.present(Some(parent));
}

/// Asks for a file name. `dir` (if known) is used to reject existing names.
pub fn name_dialog(
    parent: &impl IsA<gtk::Widget>,
    heading: &str,
    accept: &str,
    initial: &str,
    is_dir: bool,
    dir: Option<gio::File>,
    on_accept: impl Fn(String) + 'static,
) {
    let entry = gtk::Entry::builder()
        .text(initial)
        .activates_default(true)
        .width_chars(30)
        .build();
    let field = gtk::Label::builder()
        .label(if is_dir {
            tr("Folder _Name")
        } else {
            tr("File _Name")
        })
        .use_underline(true)
        .mnemonic_widget(&entry)
        .xalign(0.0)
        .build();
    field.add_css_class("heading");
    helix_app_common::a11y::label_widget(&entry, &field);
    let error = gtk::Label::builder().xalign(0.0).wrap(true).build();
    error.add_css_class("error");
    error.add_css_class("caption");
    let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
    b.append(&field);
    b.append(&entry);
    b.append(&error);
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .extra_child(&b)
        .default_response("accept")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", tr("_Cancel")), ("accept", accept)]);
    dialog.set_response_appearance("accept", adw::ResponseAppearance::Suggested);

    let initial = initial.to_string();
    let validate = {
        let dialog = dialog.clone();
        let entry = entry.clone();
        let error = error.clone();
        let initial = initial.clone();
        move |text: &str| {
            let msg = match naming::validate_name(text) {
                Err(_) if text.is_empty() => Some(String::new()),
                Err(e) => Some(e.to_string()),
                Ok(()) if text == initial => Some(String::new()),
                Ok(()) => match &dir {
                    Some(d) if d.child(text).query_exists(None::<&gio::Cancellable>) => Some(
                        if is_dir {
                            tr("A folder with that name already exists.")
                        } else {
                            tr("A file with that name already exists.")
                        }
                        .to_string(),
                    ),
                    _ => None,
                },
            };
            dialog.set_response_enabled("accept", msg.is_none());
            error.set_text(msg.as_deref().unwrap_or(""));
            error.set_visible(msg.as_ref().is_some_and(|m| !m.is_empty()));
            // Empty and unchanged names only disable the button; real
            // errors mark the entry invalid and are spoken.
            helix_app_common::a11y::set_input_error(
                &entry,
                Some(&error),
                msg.as_deref().filter(|m| !m.is_empty()),
            );
            if text.starts_with('.') && msg.is_none() {
                const HIDDEN: &str = n_("Names starting with “.” are hidden.");
                error.set_text(tr(HIDDEN));
                error.remove_css_class("error");
                error.set_visible(true);
                entry.update_property(&[gtk::accessible::Property::Description(tr(HIDDEN))]);
                helix_app_common::a11y::announce_change(
                    &error,
                    tr(HIDDEN),
                    helix_app_common::a11y::Urgency::Polite,
                );
            } else {
                error.add_css_class("error");
                entry.reset_property(gtk::AccessibleProperty::Description);
                helix_app_common::a11y::announce_change(
                    &error,
                    "",
                    helix_app_common::a11y::Urgency::Polite,
                );
            }
        }
    };
    validate(&initial);
    let v = validate.clone();
    entry.connect_changed(move |e| v(&e.text()));
    let e = entry.clone();
    dialog.connect_response(Some("accept"), move |_, _| {
        on_accept(e.text().trim_end().to_string())
    });
    dialog.present(Some(parent));
    // Select the stem (not the extension) for renames.
    let (start, end) = naming::rename_selection(&initial, is_dir);
    glib::idle_add_local_once(move || {
        entry.grab_focus();
        entry.select_region(start, end);
    });
}

/// The Create Archive dialog: a name (without extension) and a format,
/// checked against the names taken in `dir`. `on_accept` gets the file
/// name (with the format's extension) and the format.
pub fn archive_dialog(
    parent: &impl IsA<gtk::Widget>,
    initial: &str,
    format: archive::Format,
    dir: gio::File,
    on_accept: impl Fn(String, archive::Format) + 'static,
) -> adw::AlertDialog {
    let entry = gtk::Entry::builder()
        .text(initial)
        .activates_default(true)
        .hexpand(true)
        .width_chars(24)
        .build();
    let field = gtk::Label::builder()
        .label(tr("Archive _Name"))
        .use_underline(true)
        .mnemonic_widget(&entry)
        .xalign(0.0)
        .build();
    field.add_css_class("heading");
    helix_app_common::a11y::label_widget(&entry, &field);
    let extensions: Vec<&str> = archive::Format::ALL.iter().map(|f| f.extension()).collect();
    let formats = gtk::DropDown::from_strings(&extensions).labelled(tr("Archive Format"));
    formats.set_tooltip_text(Some(tr("Archive Format")));
    let selected = archive::Format::ALL
        .iter()
        .position(|f| *f == format)
        .unwrap_or(0);
    formats.set_selected(selected as u32);
    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&entry);
    row.append(&formats);
    let error = gtk::Label::builder().xalign(0.0).wrap(true).build();
    error.add_css_class("error");
    error.add_css_class("caption");
    error.set_visible(false);
    let b = gtk::Box::new(gtk::Orientation::Vertical, 6);
    b.append(&field);
    b.append(&row);
    b.append(&error);
    let dialog = adw::AlertDialog::builder()
        .heading(tr("Create Archive"))
        .extra_child(&b)
        .default_response("accept")
        .close_response("cancel")
        .build();
    dialog.add_responses(&[("cancel", tr("_Cancel")), ("accept", tr("_Create"))]);
    dialog.set_response_appearance("accept", adw::ResponseAppearance::Suggested);

    let chosen = {
        let entry = entry.clone();
        let formats = formats.clone();
        move || {
            let format = archive::Format::ALL
                .get(formats.selected() as usize)
                .copied()
                .unwrap_or(archive::Format::Zip);
            (
                format!("{}{}", entry.text().trim_end(), format.extension()),
                format,
                entry.text().trim_end().is_empty(),
            )
        }
    };
    let validate = {
        let dialog = dialog.clone();
        let entry = entry.clone();
        let error = error.clone();
        let chosen = chosen.clone();
        move || {
            let (name, _, empty) = chosen();
            let msg = if empty {
                Some(String::new())
            } else {
                match naming::validate_name(&name) {
                    Err(e) => Some(e.to_string()),
                    Ok(()) if dir.child(&name).query_exists(None::<&gio::Cancellable>) => {
                        Some(tr("A file with that name already exists.").to_string())
                    }
                    Ok(()) => None,
                }
            };
            dialog.set_response_enabled("accept", msg.is_none());
            let shown = msg.as_deref().filter(|m| !m.is_empty());
            error.set_text(shown.unwrap_or(""));
            error.set_visible(shown.is_some());
            helix_app_common::a11y::set_input_error(&entry, Some(&error), shown);
        }
    };
    validate();
    let v = validate.clone();
    entry.connect_changed(move |_| v());
    formats.connect_selected_notify(move |_| validate());
    dialog.connect_response(Some("accept"), move |_, _| {
        let (name, format, _) = chosen();
        on_accept(name, format)
    });
    dialog.present(Some(parent));
    glib::idle_add_local_once(move || {
        entry.grab_focus();
        entry.select_region(0, -1);
    });
    dialog
}

fn describe(info: Option<&gio::FileInfo>) -> String {
    let Some(i) = info else { return String::new() };
    let size = if i.file_type() == gio::FileType::Directory {
        String::new()
    } else {
        crate::fmt::size(i.size().max(0) as u64)
    };
    let date = i
        .modification_date_time()
        .and_then(|d| {
            glib::DateTime::now_local()
                .ok()
                .map(|now| crate::fmt::date(&d, &now, true))
        })
        .unwrap_or_default();
    match (size.is_empty(), date.is_empty()) {
        (false, false) => trf!("{size}, last modified {date}", size = size, date = date),
        (true, false) => trf!("Last modified {date}", date = date),
        (false, true) => size,
        _ => String::new(),
    }
}

/// Asks how to resolve a name conflict.
pub async fn conflict(parent: &adw::ApplicationWindow, c: &Conflict) -> Resolution {
    let attrs = "standard::type,standard::size,time::modified";
    let flags = gio::FileQueryInfoFlags::NOFOLLOW_SYMLINKS;
    let src = c
        .source
        .query_info_future(attrs, flags, glib::Priority::DEFAULT)
        .await
        .ok();
    let dst = c
        .dest
        .query_info_future(attrs, flags, glib::Priority::DEFAULT)
        .await
        .ok();
    let name = crate::ops::display_name(&c.dest);
    let folder = c
        .dest
        .parent()
        .map(|p| crate::ops::display_name(&p))
        .unwrap_or_default();
    let merge = c.source_is_dir && c.dest_is_dir;
    let heading = if merge {
        trf!("Merge Folder “{name}”?", name = name)
    } else {
        trf!("Replace “{name}”?", name = name)
    };
    let mut body = if merge {
        trf!("A folder with the same name already exists in “{folder}”. Merging will ask before replacing any files in the folder that conflict with the files being copied.", folder = folder)
    } else {
        trf!("Another file with the same name already exists in “{folder}”. Replacing it will overwrite its content.", folder = folder)
    };
    let (o, n) = (describe(dst.as_ref()), describe(src.as_ref()));
    if !o.is_empty() {
        body.push_str("\n\n");
        body.push_str(&trf!("Original: {description}", description = o));
    }
    if !n.is_empty() {
        body.push('\n');
        body.push_str(&trf!("Replace with: {description}", description = n));
    }
    let all = gtk::CheckButton::with_label(tr("Apply this action to all conflicts"));
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .extra_child(&all)
        .default_response(conflict_default_response(merge))
        .close_response("cancel")
        .build();
    dialog.add_responses(&[
        ("cancel", tr("_Cancel")),
        ("skip", tr("_Skip")),
        ("rename", tr("Keep _Both")),
        ("replace", if merge { tr("_Merge") } else { tr("_Replace") }),
    ]);
    dialog.set_response_appearance(
        "replace",
        if merge {
            adw::ResponseAppearance::Suggested
        } else {
            adw::ResponseAppearance::Destructive
        },
    );
    let response = dialog.choose_future(parent).await;
    let choice = match response.as_str() {
        "skip" => Choice::Skip,
        "rename" => Choice::KeepBoth,
        "replace" => Choice::Replace,
        _ => Choice::Cancel,
    };
    Resolution {
        choice,
        apply_to_all: all.is_active(),
    }
}

/// The response Enter picks in the conflict dialog. Replacing a file is
/// destructive, so files default to Keep Both; merging two folders asks
/// again for each conflicting file inside, so it may stay the default.
fn conflict_default_response(merge: bool) -> &'static str {
    if merge {
        "replace"
    } else {
        "rename"
    }
}

/// "Open With" chooser for the selected files.
/// Which heading an application is listed under in Open With.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AppGroup {
    Recommended,
    Other,
    All,
}

impl AppGroup {
    fn title(self) -> &'static str {
        match self {
            AppGroup::Recommended => tr("Recommended Applications"),
            AppGroup::Other => tr("Other Applications"),
            AppGroup::All => tr("All Applications"),
        }
    }
}

/// Groups application ids for Open With: the recommended ones first, then
/// the others registered for the type. Only when the type has no
/// applications at all is every installed application offered. Duplicates
/// are dropped.
fn group_apps(
    recommended: &[String],
    for_type: &[String],
    all: &[String],
) -> Vec<(AppGroup, String)> {
    let mut out: Vec<(AppGroup, String)> = Vec::new();
    let push = |group, id: &String, out: &mut Vec<(AppGroup, String)>| {
        if !out.iter().any(|(_, x)| x == id) {
            out.push((group, id.clone()));
        }
    };
    for id in recommended {
        push(AppGroup::Recommended, id, &mut out);
    }
    for id in for_type {
        push(AppGroup::Other, id, &mut out);
    }
    if out.is_empty() {
        for id in all {
            push(AppGroup::All, id, &mut out);
        }
    }
    out
}

/// Whether an application named `name` matches the Open With search.
fn app_matches(name: &str, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty() || name.to_lowercase().contains(&query)
}

pub fn open_with(win: &Rc<Window>, infos: Vec<gio::FileInfo>) {
    let files: Vec<gio::File> = infos.iter().filter_map(super::tab::info_target).collect();
    let Some(first) = infos.first() else { return };
    let ct = first
        .content_type()
        .or_else(|| first.attribute_string("standard::fast-content-type"))
        .map(|s| s.to_string())
        .unwrap_or_else(|| "application/octet-stream".into());
    let desc = gio::content_type_get_description(&ct);

    let shown = |v: Vec<gio::AppInfo>| -> Vec<gio::AppInfo> {
        v.into_iter().filter(|a| a.should_show()).collect()
    };
    let recommended = shown(gio::AppInfo::recommended_for_type(&ct));
    let for_type = shown(gio::AppInfo::all_for_type(&ct));
    let all = if recommended.is_empty() && for_type.is_empty() {
        shown(gio::AppInfo::all())
    } else {
        Vec::new()
    };
    let id_of = |a: &gio::AppInfo| a.id().map(|s| s.to_string()).unwrap_or_default();
    let ids = |v: &[gio::AppInfo]| v.iter().map(id_of).collect::<Vec<_>>();
    let pool: Vec<gio::AppInfo> = recommended
        .iter()
        .chain(&for_type)
        .chain(&all)
        .cloned()
        .collect();
    let apps: Vec<(AppGroup, gio::AppInfo)> =
        group_apps(&ids(&recommended), &ids(&for_type), &ids(&all))
            .into_iter()
            .filter_map(|(g, id)| pool.iter().find(|a| id_of(a) == id).map(|a| (g, a.clone())))
            .collect();
    let default = gio::AppInfo::default_for_type(&ct, false);

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.update_property(&[gtk::accessible::Property::Label(tr("Applications"))]);
    for (_, app) in &apps {
        let row = adw::ActionRow::builder()
            .title(app.display_name().as_str())
            .activatable(true)
            .build();
        if let Some(icon) = app.icon() {
            let img = gtk::Image::builder()
                .gicon(&icon)
                .pixel_size(32)
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build();
            row.add_prefix(&img);
        }
        if default.as_ref().is_some_and(|d| d.id() == app.id()) {
            row.set_subtitle(tr("Default"));
        }
        list.append(&row);
    }
    // A heading above the first row of each group.
    let groups: Vec<AppGroup> = apps.iter().map(|(g, _)| *g).collect();
    list.set_header_func(move |row, before| {
        let group = groups.get(row.index() as usize).copied();
        let prev = before.and_then(|b| groups.get(b.index() as usize).copied());
        match group {
            Some(g) if prev != Some(g) => {
                let label = gtk::Label::builder()
                    .label(g.title())
                    .xalign(0.0)
                    .margin_top(if before.is_some() { 18 } else { 6 })
                    .margin_bottom(6)
                    .margin_start(6)
                    .build();
                label.add_css_class("heading");
                row.set_header(Some(&label));
            }
            _ => row.set_header(None::<&gtk::Widget>),
        }
    });
    let names: Rc<Vec<String>> = Rc::new(
        apps.iter()
            .map(|(_, a)| a.display_name().to_string())
            .collect(),
    );
    let matching = {
        let names = names.clone();
        move |query: &str| -> Vec<usize> {
            (0..names.len())
                .filter(|&i| app_matches(&names[i], query))
                .collect()
        }
    };
    let search = gtk::SearchEntry::builder()
        .placeholder_text(tr("Search applications"))
        .build();
    search.update_property(&[gtk::accessible::Property::Label(tr("Search Applications"))]);
    {
        let names = names.clone();
        let search = search.clone();
        list.set_filter_func(move |row| {
            names
                .get(row.index() as usize)
                .is_some_and(|n| app_matches(n, &search.text()))
        });
    }
    if let Some(r) = list.row_at_index(0) {
        list.select_row(Some(&r));
    }

    let scroll = gtk::ScrolledWindow::builder()
        .child(&list)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(240)
        .build();
    let empty = adw::StatusPage::builder()
        .icon_name("system-search-symbolic")
        .title(if apps.is_empty() {
            tr("No Applications Found")
        } else {
            tr("No Results Found")
        })
        .description(if apps.is_empty() {
            tr("No installed application can open this file")
        } else {
            tr("Try a different search")
        })
        .vexpand(true)
        .build();
    empty.add_css_class("compact");
    let stack = gtk::Stack::new();
    stack.add_named(&scroll, Some("list"));
    stack.add_named(&empty, Some("empty"));
    stack.set_visible_child_name(if apps.is_empty() { "empty" } else { "list" });

    let always = gtk::CheckButton::with_label(&trf!("Always use for “{desc}” files", desc = desc));
    let content = gtk::Box::new(gtk::Orientation::Vertical, 12);
    content.set_margin_top(12);
    content.set_margin_bottom(12);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.append(&search);
    content.append(&stack);
    content.append(&always);

    let header = adw::HeaderBar::new();
    let open_btn = gtk::Button::with_mnemonic(tr("_Open"));
    open_btn.add_css_class("suggested-action");
    open_btn.set_sensitive(!apps.is_empty());
    header.pack_end(&open_btn);
    let tv = adw::ToolbarView::new();
    tv.add_top_bar(&header);
    tv.set_content(Some(&content));
    let dialog = adw::Dialog::builder()
        .title(tr("Open With"))
        .content_width(420)
        .content_height(560)
        .child(&tv)
        .default_widget(&open_btn)
        .focus_widget(&search)
        .build();

    {
        let list = list.clone();
        let stack = stack.clone();
        let open_btn = open_btn.clone();
        search.connect_search_changed(move |e| {
            list.invalidate_filter();
            let shown = matching(&e.text());
            let keep = list
                .selected_row()
                .is_some_and(|r| shown.contains(&(r.index() as usize)));
            if !keep {
                let first = shown.first().and_then(|&i| list.row_at_index(i as i32));
                list.select_row(first.as_ref());
            }
            stack.set_visible_child_name(if shown.is_empty() { "empty" } else { "list" });
            open_btn.set_sensitive(!shown.is_empty());
            let msg = match shown.len() {
                0 => tr("No applications found").to_string(),
                n => trnf!("{n} application", "{n} applications", n),
            };
            helix_app_common::announce_settled(e, &msg);
        });
    }

    let apps = Rc::new(apps);
    let launch = {
        let dialog = dialog.clone();
        let list = list.clone();
        let win = Rc::downgrade(win);
        let apps = apps.clone();
        move || {
            let Some(row) = list.selected_row() else {
                return;
            };
            let Some((_, app)) = apps.get(row.index() as usize) else {
                return;
            };
            if always.is_active() {
                let _ = app.set_as_default_for_type(&ct);
            }
            if let Some(w) = win.upgrade() {
                let ctx = gtk::prelude::WidgetExt::display(&w.window).app_launch_context();
                if let Err(e) = app.launch(&files, Some(&ctx)) {
                    w.toast(&trf!(
                        "Could not open: {}",
                        helix_app_common::errors::glib_detail(&e)
                    ));
                } else {
                    for f in &files {
                        gtk::RecentManager::default().add_item(&f.uri());
                    }
                }
            }
            dialog.close();
        }
    };
    let l = Rc::new(launch);
    let l2 = l.clone();
    open_btn.connect_clicked(move |_| l2());
    // Enter in the search entry opens the selected (first matching) app.
    let l3 = l.clone();
    search.connect_activate(move |_| l3());
    list.connect_row_activated(move |list, row| {
        list.select_row(Some(row));
        l()
    });
    // Typing anywhere in the dialog goes to the search; Down from the search moves
    // into the list.
    search.set_key_capture_widget(Some(&dialog));
    let key = gtk::EventControllerKey::new();
    {
        let list = list.clone();
        key.connect_key_pressed(move |_, keyval, _, _| {
            if keyval == gtk::gdk::Key::Down {
                if let Some(r) = list.selected_row() {
                    r.grab_focus();
                    return glib::Propagation::Stop;
                }
            }
            glib::Propagation::Proceed
        });
    }
    search.add_controller(key);
    dialog.present(Some(&win.window));
    search.grab_focus();
}

/// Preferences dialog bound to the Nautilus GSettings keys.
pub fn preferences(win: &Rc<Window>) {
    let shared = win.shared.clone();
    let dialog = adw::PreferencesDialog::new();
    let page = adw::PreferencesPage::builder()
        .title(tr("General"))
        .icon_name("preferences-system-symbolic")
        .build();

    let combo = |title: &str,
                 subtitle: &str,
                 options: &[(&'static str, &'static str)],
                 current: &str,
                 apply: Box<dyn Fn(&str)>| {
        let labels: Vec<&str> = options.iter().map(|(_, l)| *l).collect();
        let row = adw::ComboRow::builder()
            .title(title)
            .model(&gtk::StringList::new(&labels))
            .build();
        if !subtitle.is_empty() {
            row.set_subtitle(subtitle);
        }
        row.set_selected(options.iter().position(|(n, _)| *n == current).unwrap_or(0) as u32);
        let opts: Vec<&'static str> = options.iter().map(|(n, _)| *n).collect();
        row.connect_selected_notify(move |r| {
            if let Some(n) = opts.get(r.selected() as usize) {
                apply(n);
            }
        });
        row
    };
    let switch = |title: &str, subtitle: &str, active: bool, apply: Box<dyn Fn(bool)>| {
        let row = adw::SwitchRow::builder()
            .title(title)
            .active(active)
            .build();
        if !subtitle.is_empty() {
            row.set_subtitle(subtitle);
        }
        row.connect_active_notify(move |r| apply(r.is_active()));
        row
    };
    let enum_setter = |key: &'static str| -> Box<dyn Fn(&str)> {
        let s = shared.clone();
        Box::new(move |nick: &str| {
            s.prefs.set_enum_nick(key, nick);
            s.settings_changed();
        })
    };
    let bool_setter = |prefs: helix_app_common::Prefs, key: &'static str| -> Box<dyn Fn(bool)> {
        let s = shared.clone();
        Box::new(move |v: bool| {
            prefs.set(key, v);
            s.settings_changed();
        })
    };
    let p = &shared.prefs;

    let general = adw::PreferencesGroup::builder()
        .title(tr("General"))
        .build();
    general.add(&switch(
        tr("Sort Folders Before Files"),
        "",
        shared.chooser_prefs.get("sort-directories-first", true),
        bool_setter(shared.chooser_prefs.clone(), "sort-directories-first"),
    ));
    general.add(&combo(
        tr("Open Action"),
        "",
        &[
            ("double", tr("Double Click")),
            ("single", tr("Single Click")),
        ],
        &p.get_enum_nick("click-policy", "double"),
        enum_setter("click-policy"),
    ));
    general.add(&switch(
        tr("Show Hidden Files"),
        "",
        p.get("show-hidden-files", false),
        bool_setter(p.clone(), "show-hidden-files"),
    ));
    general.add(&switch(
        tr("Always Show the Location Entry"),
        tr("Instead of the path bar"),
        p.get("always-use-location-entry", false),
        bool_setter(p.clone(), "always-use-location-entry"),
    ));
    page.add(&general);

    let views = adw::PreferencesGroup::builder().title(tr("Views")).build();
    views.add(&combo(
        tr("Default View"),
        "",
        &[("icon-view", tr("Grid")), ("list-view", tr("List"))],
        &p.get_enum_nick("default-folder-viewer", "icon-view"),
        enum_setter("default-folder-viewer"),
    ));
    views.add(&combo(
        tr("Show Thumbnails"),
        "",
        &[
            ("local-only", tr("On This Computer Only")),
            ("always", tr("All Files")),
            ("never", tr("Never")),
        ],
        &p.get_enum_nick("show-image-thumbnails", "local-only"),
        enum_setter("show-image-thumbnails"),
    ));
    views.add(&combo(
        tr("Date and Time Format"),
        "",
        &[("simple", tr("Simple")), ("detailed", tr("Detailed"))],
        &p.get_enum_nick("date-time-format", "simple"),
        enum_setter("date-time-format"),
    ));
    page.add(&views);

    let menu = adw::PreferencesGroup::builder()
        .title(tr("Context Menu"))
        .build();
    menu.add(&switch(
        tr("Create Link"),
        tr("Show an action to create symbolic links"),
        p.get("show-create-link", false),
        bool_setter(p.clone(), "show-create-link"),
    ));
    menu.add(&switch(
        tr("Delete Permanently"),
        tr("Show an action to bypass the Trash"),
        p.get("show-delete-permanently", false),
        bool_setter(p.clone(), "show-delete-permanently"),
    ));
    page.add(&menu);

    let search = adw::PreferencesGroup::builder().title(tr("Search")).build();
    search.add(&combo(
        tr("Search in Subfolders"),
        "",
        &[
            ("local-only", tr("On This Computer Only")),
            ("always", tr("All Locations")),
            ("never", tr("Never")),
        ],
        &p.get_enum_nick("recursive-search", "local-only"),
        enum_setter("recursive-search"),
    ));
    page.add(&search);

    dialog.add(&page);
    dialog.present(Some(&win.window));
}

#[cfg(test)]
mod tests {
    use super::{app_matches, conflict_default_response, group_apps, AppGroup};

    fn v(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn open_with_groups_relevant_apps_only() {
        let g = group_apps(&v(&["a", "b"]), &v(&["b", "c"]), &v(&["x"]));
        assert_eq!(
            g,
            vec![
                (AppGroup::Recommended, "a".into()),
                (AppGroup::Recommended, "b".into()),
                (AppGroup::Other, "c".into()),
            ]
        );
        // Every app is offered only when none handles the type.
        let g = group_apps(&[], &[], &v(&["x", "y"]));
        assert!(g.iter().all(|(grp, _)| *grp == AppGroup::All));
        assert_eq!(g.len(), 2);
    }

    #[test]
    fn open_with_search_matches_names() {
        assert!(app_matches("Text Editor", ""));
        assert!(app_matches("Text Editor", " edit "));
        assert!(!app_matches("Text Editor", "image"));
    }

    #[test]
    fn enter_never_replaces_a_file() {
        assert_eq!(conflict_default_response(false), "rename");
        // Folder merge stays the default; it asks again per file.
        assert_eq!(conflict_default_response(true), "replace");
    }
}
