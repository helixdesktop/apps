//! Printers panel: CUPS's printers, their jobs and options; changes go
//! through cups-pk-helper, so polkit asks for them.

use adw::prelude::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use super::widgets::{self, InfoRow};
use super::{follow, spawn_worker, Ctx, Panel};
use crate::bus;
use crate::cups::{self, Choices, Job, Printer, Server};
use helix_app_common::i18n::tr;
use helix_app_common::trf;

pub struct PrintersPanel;

/// How often the page reads CUPS again while it is on screen.
const POLL: Duration = Duration::from_secs(3);
/// How long CUPS looks for printers when adding one.
const SEARCH_SECONDS: i32 = 10;

/// The open details dialog of one printer.
struct Details {
    dialog: adw::Dialog,
    name: String,
    status: InfoRow,
    default: gtk::Button,
    paused: adw::SwitchRow,
    jobs: adw::PreferencesGroup,
    job_rows: RefCell<Vec<gtk::Widget>>,
    shown_jobs: RefCell<Option<Vec<Job>>>,
}

struct State {
    ctx: Rc<Ctx>,
    stack: gtk::Stack,
    list: adw::PreferencesGroup,
    rows: RefCell<Vec<gtk::Widget>>,
    current: RefCell<cups::State>,
    shown: RefCell<Option<cups::State>>,
    quiet: Cell<bool>,
    details: RefCell<Option<Details>>,
}

impl Panel for PrintersPanel {
    fn build(&self, ctx: &Rc<Ctx>) -> gtk::Widget {
        let stack = gtk::Stack::new();
        stack.add_named(&widgets::loading(tr("Loading Printers…")), Some("loading"));
        stack.add_named(
            &widgets::status(
                "printer-error-symbolic",
                tr("Printing Is Not Available"),
                tr("The printing service (CUPS) is not running."),
            ),
            Some("missing"),
        );
        stack.add_named(
            &widgets::status(
                "printer-error-symbolic",
                tr("Printing Is Not Installed"),
                tr("Install CUPS to set up printers."),
            ),
            Some("not-installed"),
        );
        let page = widgets::page();
        let list = widgets::group(tr("Printers"));
        let add = gtk::Button::builder()
            .label(tr("Add Printer…"))
            .valign(gtk::Align::Center)
            .build();
        list.set_header_suffix(Some(&add));
        page.add(&list);
        stack.add_named(&page, Some("list"));
        stack.set_visible_child_name("loading");

        let state = Rc::new(State {
            ctx: ctx.clone(),
            stack: stack.clone(),
            list,
            rows: RefCell::default(),
            current: RefCell::default(),
            shown: RefCell::default(),
            quiet: Cell::new(false),
            details: RefCell::default(),
        });
        let weak = Rc::downgrade(&state);
        add.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                open_add(&state);
            }
        });
        let weak = Rc::downgrade(&state);
        stack.connect_map(move |_| {
            if let Some(state) = weak.upgrade() {
                refresh(&state);
            }
        });
        refresh(&state);
        // CUPS sends no change signals to plain clients: read it again
        // while the page is on screen.
        follow(
            &state,
            Duration::from_millis(50),
            |tell| loop {
                std::thread::sleep(POLL);
                if !tell() {
                    break;
                }
            },
            |state| {
                if state.stack.is_mapped() || state.details.borrow().is_some() {
                    refresh(state);
                }
            },
        );
        stack.connect_destroy(move |_| {
            let _ = &state;
        });
        stack.upcast()
    }
}

fn refresh(state: &Rc<State>) {
    let weak = Rc::downgrade(state);
    spawn_worker(
        || cups::read(&Server::local()),
        move |r| {
            if let Some(state) = weak.upgrade() {
                show(&state, r);
            }
        },
    );
}

/// A change's error as a toast (made by `failed`), then a fresh reading.
/// Refusals (polkit, or the user cancelling) are not reported.
fn done(weak: &std::rc::Weak<State>, r: bus::Result<()>, failed: impl Fn(&str) -> String) {
    let Some(state) = weak.upgrade() else { return };
    match r {
        Err(bus::Error::Denied) => {}
        Err(bus::Error::Missing) => state.ctx.toast(tr(
            "Changing printers needs cups-pk-helper, which is not installed",
        )),
        Err(e) => state.ctx.toast(&failed(&e.to_string())),
        Ok(()) => {}
    }
    refresh(&state);
}

/// Runs a cups-pk-helper change on a worker thread, then [`done`].
fn change(
    state: &Rc<State>,
    work: impl FnOnce(&zbus::blocking::Connection) -> bus::Result<()> + Send + 'static,
    failed: impl Fn(&str) -> String + 'static,
) {
    let weak = Rc::downgrade(state);
    spawn_worker(
        move || bus::system().and_then(|c| work(&c)),
        move |r| done(&weak, r, failed),
    );
}

fn show(state: &Rc<State>, r: bus::Result<cups::State>) {
    let s = match r {
        Ok(s) => s,
        Err(e) => {
            if e != bus::Error::Missing {
                eprintln!("helix-settings-app: cups: {e}");
            }
            let page = if cups::installed() {
                "missing"
            } else {
                "not-installed"
            };
            state.stack.set_visible_child_name(page);
            close_details(state);
            return;
        }
    };
    state.stack.set_visible_child_name("list");
    if state.shown.borrow().as_ref() != Some(&s) {
        let rows: Vec<gtk::Widget> = s
            .printers
            .iter()
            .map(|p| printer_row(state, p, s.default.as_deref() == Some(&p.name)).upcast())
            .collect();
        state.list.set_description(if rows.is_empty() {
            Some(tr("No printers are set up yet."))
        } else {
            None
        });
        widgets::replace_rows(&state.list, &state.rows, rows);
        *state.shown.borrow_mut() = Some(s.clone());
    }
    *state.current.borrow_mut() = s;
    update_details(state);
}

fn printer(state: &State, name: &str) -> Option<Printer> {
    state
        .current
        .borrow()
        .printers
        .iter()
        .find(|p| p.name == name)
        .cloned()
}

fn subtitle(p: &Printer) -> String {
    if p.location.is_empty() {
        p.status()
    } else {
        trf!(
            "{status} · {location}",
            status = p.status(),
            location = p.location
        )
    }
}

fn printer_row(state: &Rc<State>, p: &Printer, default: bool) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title(p.title())
        .subtitle(subtitle(p))
        .activatable(true)
        .build();
    row.add_prefix(
        &gtk::Image::builder()
            .icon_name("printer-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build(),
    );
    if default {
        let label = gtk::Label::builder()
            .label(tr("Default"))
            .css_classes(["dim-label"])
            .valign(gtk::Align::Center)
            .build();
        row.add_suffix(&label);
    }
    row.add_suffix(
        &gtk::Image::builder()
            .icon_name("go-next-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build(),
    );
    let name = p.name.clone();
    let weak = Rc::downgrade(state);
    row.connect_activated(move |_| {
        if let Some(state) = weak.upgrade() {
            open_details(&state, &name);
        }
    });
    row
}

fn open_details(state: &Rc<State>, name: &str) {
    let Some(p) = printer(state, name) else {
        return;
    };
    let page = widgets::page();

    let info = widgets::group("");
    let status = InfoRow::new(tr("Status"), &p.status());
    info.add(&status.row);
    info.add(&InfoRow::new(tr("Model"), &p.model).row);
    info.add(&InfoRow::new(tr("Location"), &p.location).row);
    info.add(&InfoRow::new(tr("Address"), &p.device_uri).row);
    page.add(&info);

    let use_group = widgets::group("");
    let (default_row, default) = widgets::action_row(
        tr("Default Printer"),
        tr("Apps print here unless another printer is chosen"),
        tr("Make Default"),
        None,
    );
    use_group.add(&default_row);
    let paused = adw::SwitchRow::builder()
        .use_markup(false)
        .title(tr("Paused"))
        .subtitle(tr("Jobs wait until printing is resumed"))
        .build();
    use_group.add(&paused);
    page.add(&use_group);

    let options = widgets::group(tr("Default Options"));
    options.set_visible(false);
    page.add(&options);

    let jobs = widgets::group(tr("Jobs"));
    page.add(&jobs);

    let actions = widgets::group("");
    let (test_row, test) = widgets::action_row(
        tr("Test Page"),
        tr("Print a page to check that the printer works"),
        tr("Print"),
        None,
    );
    actions.add(&test_row);
    let (remove_row, remove) = widgets::action_row(
        tr("Remove Printer"),
        tr("Apps will no longer be able to print to it"),
        tr("Remove…"),
        Some("destructive-action"),
    );
    actions.add(&remove_row);
    page.add(&actions);

    let dialog = widgets::dialog(p.title(), &page);
    dialog.set_content_height(560);

    let weak = Rc::downgrade(state);
    let n = p.name.clone();
    default.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else { return };
        let (name, title) = (n.clone(), title_of(&state, &n));
        change(
            &state,
            move |c| cups::set_default(c, &name),
            move |e| {
                trf!(
                    "Could not make {name} the default printer: {e}",
                    name = title,
                    e = e
                )
            },
        );
    });
    let weak = Rc::downgrade(state);
    let n = p.name.clone();
    paused.connect_active_notify(move |sw| {
        let Some(state) = weak.upgrade() else { return };
        if state.quiet.get() {
            return;
        }
        let (name, on, title) = (n.clone(), !sw.is_active(), title_of(&state, &n));
        change(
            &state,
            move |c| cups::set_enabled(c, &name, on),
            move |e| {
                if on {
                    trf!("Could not resume {name}: {e}", name = title, e = e)
                } else {
                    trf!("Could not pause {name}: {e}", name = title, e = e)
                }
            },
        );
    });
    let weak = Rc::downgrade(state);
    let n = p.name.clone();
    test.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else { return };
        let (name, title) = (n.clone(), title_of(&state, &n));
        let weak = Rc::downgrade(&state);
        spawn_worker(
            move || {
                let page =
                    std::fs::read(cups::TEST_PAGE).map_err(|e| bus::Error::Other(e.to_string()))?;
                cups::print_test_page(&Server::local(), &name, &page)
            },
            move |r| {
                let Some(state) = weak.upgrade() else { return };
                match r {
                    Ok(_) => state
                        .ctx
                        .toast(&trf!("Test page sent to {name}", name = title)),
                    Err(e) => state.ctx.toast(&trf!(
                        "Could not print a test page on {name}: {e}",
                        name = title,
                        e = e
                    )),
                }
                refresh(&state);
            },
        );
    });
    let weak = Rc::downgrade(state);
    let n = p.name.clone();
    remove.connect_clicked(move |_| {
        if let Some(state) = weak.upgrade() {
            confirm_remove(&state, &n);
        }
    });
    let weak = Rc::downgrade(state);
    dialog.connect_closed(move |_| {
        if let Some(state) = weak.upgrade() {
            state.details.borrow_mut().take();
        }
    });

    *state.details.borrow_mut() = Some(Details {
        dialog: dialog.clone(),
        name: p.name.clone(),
        status,
        default,
        paused,
        jobs,
        job_rows: RefCell::default(),
        shown_jobs: RefCell::default(),
    });
    update_details(state);
    load_options(state, &p.name, &options);
    dialog.present(Some(&state.ctx.window));
}

fn title_of(state: &State, name: &str) -> String {
    printer(state, name).map_or_else(|| name.to_owned(), |p| p.title().to_owned())
}

fn close_details(state: &State) {
    let d = state.details.borrow_mut().take();
    if let Some(d) = d {
        d.dialog.close();
    }
}

/// Brings the open dialog up to date, and reads its printer's jobs.
fn update_details(state: &Rc<State>) {
    let (name, gone) = {
        let details = state.details.borrow();
        let Some(d) = details.as_ref() else { return };
        let current = state.current.borrow();
        match current.printers.iter().find(|p| p.name == d.name) {
            Some(p) => {
                d.status.set(&p.status());
                let is_default = current.default.as_deref() == Some(&p.name);
                d.default.set_sensitive(!is_default);
                d.default.set_label(if is_default {
                    tr("Default")
                } else {
                    tr("Make Default")
                });
                state.quiet.set(true);
                d.paused.set_active(p.state == cups::PrinterState::Stopped);
                state.quiet.set(false);
                (d.name.clone(), false)
            }
            None => (d.name.clone(), true),
        }
    };
    if gone {
        close_details(state);
        return;
    }
    let weak = Rc::downgrade(state);
    spawn_worker(
        move || cups::jobs(&Server::local(), &name),
        move |r| {
            let Some(state) = weak.upgrade() else { return };
            show_jobs(&state, r.unwrap_or_default());
        },
    );
}

fn show_jobs(state: &Rc<State>, jobs: Vec<Job>) {
    let details = state.details.borrow();
    let Some(d) = details.as_ref() else { return };
    if d.shown_jobs.borrow().as_ref() == Some(&jobs) {
        return;
    }
    d.jobs.set_description(if jobs.is_empty() {
        Some(tr("No jobs are waiting."))
    } else {
        None
    });
    let rows: Vec<gtk::Widget> = jobs.iter().map(|j| job_row(state, j).upcast()).collect();
    widgets::replace_rows(&d.jobs, &d.job_rows, rows);
    *d.shown_jobs.borrow_mut() = Some(jobs);
}

fn job_row(state: &Rc<State>, j: &Job) -> adw::ActionRow {
    let title = if j.name.is_empty() {
        trf!("Job {id}", id = j.id)
    } else {
        j.name.clone()
    };
    let subtitle = if j.user.is_empty() {
        j.status().to_owned()
    } else {
        trf!("{status} · {user}", status = j.status(), user = j.user)
    };
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title(&title)
        .subtitle(subtitle)
        .build();
    let held = j.state == cups::JobState::Held;
    let hold = gtk::Button::builder()
        .icon_name(if held {
            "media-playback-start-symbolic"
        } else {
            "media-playback-pause-symbolic"
        })
        .tooltip_text(if held { tr("Resume") } else { tr("Hold") })
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    helix_app_common::set_accessible_label(
        &hold,
        &if held {
            trf!("Resume {job}", job = title)
        } else {
            trf!("Hold {job}", job = title)
        },
    );
    let cancel = gtk::Button::builder()
        .icon_name("process-stop-symbolic")
        .tooltip_text(tr("Cancel Job"))
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .build();
    helix_app_common::set_accessible_label(&cancel, &trf!("Cancel {job}", job = title));
    row.add_suffix(&hold);
    row.add_suffix(&cancel);
    let (id, weak) = (j.id, Rc::downgrade(state));
    let t = title.clone();
    hold.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else { return };
        let t = t.clone();
        change(
            &state,
            move |c| cups::hold_job(c, id, !held),
            move |e| trf!("Could not change {job}: {e}", job = t, e = e),
        );
    });
    let weak = Rc::downgrade(state);
    cancel.connect_clicked(move |_| {
        let Some(state) = weak.upgrade() else { return };
        let t = title.clone();
        change(
            &state,
            move |c| cups::cancel_job(c, id),
            move |e| trf!("Could not cancel {job}: {e}", job = t, e = e),
        );
    });
    row
}

/// Reads the printer's options and shows a row for each.
fn load_options(state: &Rc<State>, name: &str, group: &adw::PreferencesGroup) {
    let (weak, group, n) = (Rc::downgrade(state), group.clone(), name.to_owned());
    spawn_worker(
        move || cups::options(&Server::local(), &n),
        move |r| {
            let Some(state) = weak.upgrade() else { return };
            let Some(name) = state.details.borrow().as_ref().map(|d| d.name.clone()) else {
                return;
            };
            let choices = r.unwrap_or_default();
            group.set_visible(!choices.is_empty());
            for c in choices {
                group.add(&option_row(&state, &name, c));
            }
        },
    );
}

fn option_row(state: &Rc<State>, printer: &str, c: Choices) -> adw::ComboRow {
    let labels: Vec<String> = c
        .values
        .iter()
        .map(|v| cups::choice_label(c.kind, v))
        .collect();
    let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
    let row = adw::ComboRow::builder()
        .use_subtitle(false)
        .title(c.kind.title())
        .model(&gtk::StringList::new(&refs))
        .build();
    if let Some(i) = c
        .default
        .as_ref()
        .and_then(|d| c.values.iter().position(|v| v == d))
    {
        row.set_selected(i as u32);
    }
    let (weak, printer) = (Rc::downgrade(state), printer.to_owned());
    let values = c.values.clone();
    let option = c.kind.attribute();
    let title = c.kind.title();
    row.connect_selected_notify(move |row| {
        let Some(state) = weak.upgrade() else { return };
        let Some(value) = values.get(row.selected() as usize).cloned() else {
            return;
        };
        let printer = printer.clone();
        change(
            &state,
            move |c| cups::set_option_default(c, &printer, option, &value),
            move |e| trf!("Could not change {option}: {e}", option = title, e = e),
        );
    });
    row
}

fn confirm_remove(state: &Rc<State>, name: &str) {
    let title = title_of(state, name);
    let alert = adw::AlertDialog::new(
        Some(&trf!("Remove {name}?", name = title)),
        Some(tr("It will have to be added again before it can be used.")),
    );
    alert.add_response("cancel", tr("_Cancel"));
    alert.add_response("remove", tr("_Remove"));
    alert.set_response_appearance("remove", adw::ResponseAppearance::Destructive);
    alert.set_default_response(Some("cancel"));
    alert.set_close_response("cancel");
    let (weak, name) = (Rc::downgrade(state), name.to_owned());
    alert.connect_response(None, move |_, response| {
        if response != "remove" {
            return;
        }
        let Some(state) = weak.upgrade() else { return };
        close_details(&state);
        let (name, title) = (name.clone(), title.clone());
        change(
            &state,
            move |c| cups::remove(c, &name),
            move |e| trf!("Could not remove {name}: {e}", name = title, e = e),
        );
    });
    alert.present(Some(&state.ctx.window));
}

/// The Add Printer dialog: printers CUPS finds, or one at an address.
fn open_add(state: &Rc<State>) {
    let page = widgets::page();
    let found = widgets::group(tr("Printers Found"));
    let spinner = gtk::Spinner::builder().spinning(true).build();
    helix_app_common::set_accessible_label(&spinner, tr("Searching for Printers"));
    found.set_header_suffix(Some(&spinner));
    found.set_description(Some(tr("Searching for printers on the network…")));
    page.add(&found);
    let manual = widgets::group(tr("Add by Address"));
    manual.set_description(Some(tr(
        "The printer's host name or IP address, for printers that support driverless printing (IPP Everywhere)",
    )));
    let address = adw::EntryRow::builder()
        .use_markup(false)
        .title(tr("Address"))
        .show_apply_button(true)
        .build();
    manual.add(&address);
    page.add(&manual);
    let dialog = widgets::dialog(tr("Add Printer"), &page);
    dialog.set_content_height(480);

    let weak = Rc::downgrade(state);
    let d = dialog.clone();
    address.connect_apply(move |e| {
        let Some(state) = weak.upgrade() else { return };
        let text = e.text().to_string();
        let Some(uri) = cups::uri_for_address(&text) else {
            state
                .ctx
                .toast(tr("Enter a host name, an IP address, or an ipp:// address"));
            return;
        };
        d.close();
        add_printer(&state, text.trim(), &uri, "");
    });

    let (weak, group, d) = (Rc::downgrade(state), found.clone(), dialog.clone());
    spawn_worker(
        || bus::system().and_then(|c| cups::devices(&c, SEARCH_SECONDS)),
        move |r| {
            let Some(state) = weak.upgrade() else { return };
            spinner.set_visible(false);
            let configured: Vec<String> = state
                .current
                .borrow()
                .printers
                .iter()
                .map(|p| p.device_uri.clone())
                .collect();
            let devices: Vec<cups::Device> = match r {
                Ok(v) => v
                    .into_iter()
                    .filter(|d| !configured.contains(&d.uri))
                    .collect(),
                Err(bus::Error::Denied) => Vec::new(),
                Err(bus::Error::Missing) => {
                    group.set_description(Some(tr(
                        "Finding printers needs cups-pk-helper, which is not installed.",
                    )));
                    return;
                }
                Err(e) => {
                    group.set_description(Some(&trf!("Could not search for printers: {e}", e = e)));
                    return;
                }
            };
            group.set_description(if devices.is_empty() {
                Some(tr(
                    "No other printers were found. Make sure the printer is on and connected.",
                ))
            } else {
                None
            });
            for dev in devices {
                let row = adw::ActionRow::builder()
                    .use_markup(false)
                    .title(dev.title())
                    .subtitle(if dev.model.is_empty() || dev.model == dev.title() {
                        dev.location.clone()
                    } else {
                        dev.model.clone()
                    })
                    .activatable(true)
                    .build();
                row.add_prefix(
                    &gtk::Image::builder()
                        .icon_name("printer-network-symbolic")
                        .accessible_role(gtk::AccessibleRole::Presentation)
                        .build(),
                );
                let (weak, d) = (Rc::downgrade(&state), d.clone());
                row.connect_activated(move |_| {
                    let Some(state) = weak.upgrade() else { return };
                    d.close();
                    add_printer(&state, dev.title(), &dev.uri, &dev.location);
                });
                group.add(&row);
            }
        },
    );
    dialog.present(Some(&state.ctx.window));
}

fn add_printer(state: &Rc<State>, title: &str, uri: &str, location: &str) {
    let taken: Vec<String> = state
        .current
        .borrow()
        .printers
        .iter()
        .map(|p| p.name.clone())
        .collect();
    let name = cups::queue_name(title, &taken);
    let (uri, info, location) = (uri.to_owned(), title.to_owned(), location.to_owned());
    let weak = Rc::downgrade(state);
    let shown = title.to_owned();
    state.ctx.toast(&trf!("Adding {name}…", name = shown));
    spawn_worker(
        move || bus::system().and_then(|c| cups::add(&c, &name, &uri, &info, &location)),
        move |r| {
            let Some(state) = weak.upgrade() else { return };
            if r.is_ok() {
                state.ctx.toast(&trf!("Added {name}", name = shown));
            }
            done(&Rc::downgrade(&state), r, |e| {
                trf!("Could not add {name}: {e}", name = shown, e = e)
            });
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subtitles() {
        let p = Printer {
            name: "a".into(),
            info: "A".into(),
            location: "Hall".into(),
            model: String::new(),
            device_uri: String::new(),
            state: cups::PrinterState::Idle,
            reasons: vec!["media-jam-error".into()],
            message: String::new(),
            accepting: true,
            jobs: 0,
        };
        assert_eq!(subtitle(&p), "Paper jam · Hall");
        let q = Printer {
            location: String::new(),
            reasons: Vec::new(),
            ..p
        };
        assert_eq!(subtitle(&q), "Ready");
    }
}
