//! Network panel: wired connections and VPNs (NetworkManager) and the
//! network proxy (`org.gnome.system.proxy`).

use adw::prelude::*;
use gtk::gio;
use helix_app_common::Prefs;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use super::netedit::{self, Editor};
use super::widgets::{self, watch};
use super::wifi::push_ip_info;
use super::{follow, spawn_worker, Ctx, Panel};
use crate::bus;
use crate::netconf;
use crate::nm::{self, Device, Nm, Saved};
use helix_app_common::i18n::{n_, tr};
use helix_app_common::nc_;
use helix_app_common::trf;

pub struct NetworkPanel;

const PROXY: &str = "org.gnome.system.proxy";
const PROXY_MODES: &[(&str, &str)] = &[
    ("none", n_("Off")),
    ("manual", n_("Manual")),
    ("auto", n_("Automatic")),
];

struct State {
    ctx: Rc<Ctx>,
    stack: gtk::Stack,
    page: adw::PreferencesPage,
    /// The wired and VPN groups, rebuilt when what they show changes.
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    proxy: adw::PreferencesGroup,
    nm: RefCell<Nm>,
    shown: RefCell<String>,
    quiet: Cell<bool>,
}

impl Panel for NetworkPanel {
    fn build(&self, ctx: &Rc<Ctx>) -> gtk::Widget {
        let stack = gtk::Stack::new();
        stack.add_named(&widgets::loading(tr("Loading Network…")), Some("loading"));
        let page = widgets::page();
        let proxy = proxy_group(&ctx.prefs(PROXY), ctx);
        page.add(&proxy);
        stack.add_named(&page, Some("list"));
        stack.set_visible_child_name("loading");
        let state = Rc::new(State {
            ctx: ctx.clone(),
            stack: stack.clone(),
            page,
            groups: RefCell::default(),
            proxy,
            nm: RefCell::default(),
            shown: RefCell::default(),
            quiet: Cell::new(false),
        });
        refresh(&state);
        follow(
            &state,
            Duration::from_millis(500),
            |tell| {
                if let Ok(conn) = bus::system() {
                    nm::watch(&conn, tell);
                }
            },
            refresh,
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
        || bus::system().and_then(|c| nm::read(&c)),
        move |r| {
            if let Some(state) = weak.upgrade() {
                show(&state, r);
            }
        },
    );
}

fn report(weak: &std::rc::Weak<State>, r: bus::Result<impl Sized>, what: &str) {
    let Some(state) = weak.upgrade() else { return };
    if let Err(e) = r {
        if e != bus::Error::Denied {
            state.ctx.toast(&format!("{what}: {e}"));
        }
    }
    refresh(&state);
}

/// What the groups show, to rebuild them only when it changes.
fn shape(nm: &Nm) -> String {
    let mut s = String::new();
    for d in nm.wired_devices() {
        s += &format!(
            "{}:{}:{}:{}:{};",
            d.path, d.state, d.active, d.carrier, d.speed
        );
        for p in nm.wired_profiles(d) {
            s += &format!("{}={};", p.path, p.id());
        }
    }
    for v in nm.vpns() {
        let state = nm.active_of(&v.path).map_or(0, |a| a.state);
        s += &format!("{}={}:{state};", v.path, v.id());
    }
    s
}

fn show(state: &Rc<State>, r: bus::Result<Nm>) {
    state.stack.set_visible_child_name("list");
    let nm = match r {
        Ok(nm) => nm,
        Err(e) => {
            if e != bus::Error::Missing {
                eprintln!("helix-settings-app: NetworkManager: {e}");
            }
            // The proxy works without NetworkManager.
            let missing = widgets::group(tr("Connections"));
            missing.set_description(Some(tr(
                "NetworkManager is not running, so wired connections and VPNs cannot be shown.",
            )));
            rebuild(state, vec![missing]);
            *state.shown.borrow_mut() = String::from("missing");
            return;
        }
    };
    let shape = shape(&nm);
    *state.nm.borrow_mut() = nm.clone();
    if *state.shown.borrow() == shape {
        return;
    }
    let mut groups = Vec::new();
    let wired = nm.wired_devices();
    let several = wired.len() > 1;
    for d in &wired {
        groups.push(wired_group(state, &nm, d, several));
    }
    groups.push(vpn_group(state, &nm));
    rebuild(state, groups);
    *state.shown.borrow_mut() = shape;
}

fn rebuild(state: &Rc<State>, groups: Vec<adw::PreferencesGroup>) {
    // Keep the keyboard focus in the same group across the rebuild.
    let focused = state
        .groups
        .borrow()
        .iter()
        .position(|g| g.focus_child().is_some());
    for g in state.groups.borrow_mut().drain(..) {
        state.page.remove(&g);
    }
    state.page.remove(&state.proxy);
    for g in &groups {
        state.page.add(g);
    }
    state.page.add(&state.proxy);
    if let Some(g) = focused.and_then(|i| groups.get(i)) {
        g.child_focus(gtk::DirectionType::TabForward);
    }
    *state.groups.borrow_mut() = groups;
}

fn status(d: &Device) -> String {
    if d.activated() {
        if d.speed > 0 {
            trf!("Connected — {speed} Mb/s", speed = d.speed)
        } else {
            tr("Connected").to_string()
        }
    } else if d.connecting() {
        tr("Connecting…").to_string()
    } else if d.state <= nm::STATE_UNAVAILABLE || !d.carrier {
        tr("Cable unplugged").to_string()
    } else {
        tr("Off").to_string()
    }
}

fn gear(name: &str) -> gtk::Button {
    let b = gtk::Button::builder()
        .icon_name("emblem-system-symbolic")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .tooltip_text(tr("Details"))
        .build();
    helix_app_common::set_accessible_label(&b, &trf!("Details of {name}", name = name));
    b
}

fn wired_group(state: &Rc<State>, nm: &Nm, d: &Device, several: bool) -> adw::PreferencesGroup {
    let title = if several {
        trf!("Wired ({interface})", interface = d.interface)
    } else {
        tr("Wired").to_string()
    };
    let group = widgets::group(&title);
    let profiles: Vec<Saved> = nm.wired_profiles(d).into_iter().cloned().collect();
    let current = nm.device_connection(d).cloned();
    let row = adw::ActionRow::builder()
        .use_markup(false)
        .title(status(d))
        .build();
    row.add_prefix(
        &gtk::Image::builder()
            .icon_name("network-wired-symbolic")
            .accessible_role(gtk::AccessibleRole::Presentation)
            .build(),
    );
    if let Some(p) = current.clone().or_else(|| profiles.first().cloned()) {
        let b = gear(&title);
        let (weak, path, dev) = (Rc::downgrade(state), p.path.clone(), d.path.clone());
        b.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                details(&state, &path, &dev);
            }
        });
        row.add_suffix(&b);
    }
    let sw = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(d.activated() || d.connecting())
        .sensitive(d.carrier && d.state > nm::STATE_UNAVAILABLE)
        .build();
    helix_app_common::set_accessible_label(&sw, &title);
    row.add_suffix(&sw);
    row.set_activatable_widget(Some(&sw));
    {
        let (weak, dev) = (Rc::downgrade(state), d.path.clone());
        let first = profiles.first().map(|p| p.path.clone());
        sw.connect_active_notify(move |sw| {
            let Some(state) = weak.upgrade() else { return };
            if state.quiet.get() {
                return;
            }
            let (on, dev, first, weak) = (
                sw.is_active(),
                dev.clone(),
                first.clone(),
                Rc::downgrade(&state),
            );
            spawn_worker(
                move || {
                    let c = bus::system()?;
                    match (on, first) {
                        (false, _) => nm::disconnect(&c, &dev),
                        (true, Some(p)) => nm::activate(&c, &p, &dev, "/"),
                        // No profile yet: NetworkManager makes one.
                        (true, None) => {
                            nm::add_and_activate(&c, &nm::Settings::new(), &dev, "/").map(drop)
                        }
                    }
                },
                move |r| report(&weak, r, tr("Could not change the wired connection")),
            );
        });
    }
    group.add(&row);

    // Several profiles: choose which one the cable uses.
    if profiles.len() > 1 {
        for p in &profiles {
            let active = current.as_ref().is_some_and(|c| c.path == p.path);
            let r = adw::ActionRow::builder()
                .use_markup(false)
                .title(p.id())
                .subtitle(if active {
                    tr("In use")
                } else {
                    tr("Select to use this profile")
                })
                .activatable(!active)
                .build();
            if active {
                r.add_prefix(
                    &gtk::Image::builder()
                        .icon_name("object-select-symbolic")
                        .accessible_role(gtk::AccessibleRole::Presentation)
                        .build(),
                );
            }
            let b = gear(&p.id());
            let (weak, path, dev) = (Rc::downgrade(state), p.path.clone(), d.path.clone());
            b.connect_clicked(move |_| {
                if let Some(state) = weak.upgrade() {
                    details(&state, &path, &dev);
                }
            });
            r.add_suffix(&b);
            let (weak, path, dev, name) =
                (Rc::downgrade(state), p.path.clone(), d.path.clone(), p.id());
            r.connect_activated(move |_| {
                let (path, dev, weak) = (path.clone(), dev.clone(), weak.clone());
                let failed = trf!("Could not use {name}", name = name);
                spawn_worker(
                    move || bus::system().and_then(|c| nm::activate(&c, &path, &dev, "/")),
                    move |r| report(&weak, r, &failed),
                );
            });
            group.add(&r);
        }
    }
    let (add_row, add) = widgets::action_row(
        tr("Add Profile"),
        tr("Another set of settings for this connection"),
        tr("Add…"),
        None,
    );
    helix_app_common::set_accessible_label(&add, &trf!("Add Profile for {name}", name = title));
    {
        let (weak, iface, dev) = (Rc::downgrade(state), d.interface.clone(), d.path.clone());
        let n = profiles.len() + 1;
        add.connect_clicked(move |_| {
            let Some(state) = weak.upgrade() else { return };
            let settings = wired_profile(&trf!("Wired Profile {n}", n = n), &iface);
            let (weak, dev) = (Rc::downgrade(&state), dev.clone());
            spawn_worker(
                move || bus::system().and_then(|c| nm::add(&c, &settings)),
                move |r| {
                    let Some(state) = weak.upgrade() else { return };
                    match r {
                        Ok(path) => {
                            // Read it, then open its settings.
                            let weak = Rc::downgrade(&state);
                            spawn_worker(
                                || bus::system().and_then(|c| nm::read(&c)),
                                move |r| {
                                    let Some(state) = weak.upgrade() else { return };
                                    if let Ok(nm) = r {
                                        *state.nm.borrow_mut() = nm;
                                        details(&state, &path, &dev);
                                    }
                                    refresh(&state);
                                },
                            );
                        }
                        Err(bus::Error::Denied) => {}
                        Err(e) => state
                            .ctx
                            .toast(&trf!("Could not add a profile: {e}", e = e)),
                    }
                },
            );
        });
    }
    group.add(&add_row);
    group
}

/// A new wired profile bound to `interface`, not connecting by itself.
fn wired_profile(name: &str, interface: &str) -> nm::Settings {
    let mut s = nm::Settings::new();
    s.insert(
        "connection".into(),
        [
            ("id", nm::val(name)),
            ("type", nm::val("802-3-ethernet")),
            ("interface-name", nm::val(interface)),
            ("autoconnect", nm::val(false)),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v))
        .collect(),
    );
    s.insert(
        "ipv4".into(),
        [("method".to_owned(), nm::val("auto"))]
            .into_iter()
            .collect(),
    );
    s.insert(
        "ipv6".into(),
        [("method".to_owned(), nm::val("auto"))]
            .into_iter()
            .collect(),
    );
    s
}

fn details(state: &Rc<State>, saved: &str, device: &str) {
    let nm = state.nm.borrow().clone();
    let Some(saved) = nm.saved(saved).cloned() else {
        return;
    };
    let dev = nm.devices.iter().find(|d| d.path == device);
    let active = dev.filter(|d| {
        d.activated()
            && nm
                .device_connection(d)
                .is_some_and(|c| c.path == saved.path)
    });
    let mut info = Vec::new();
    if let Some(d) = active {
        if d.speed > 0 {
            info.push((tr("Link Speed").to_string(), trf!("{n} Mb/s", n = d.speed)));
        }
        push_ip_info(&mut info, &nm, d);
    } else if let Some(d) = dev {
        if !d.mac().is_empty() {
            info.push((tr("Hardware Address").to_string(), d.mac().to_string()));
        }
    }
    let weak = Rc::downgrade(state);
    netedit::open(
        &state.ctx,
        Editor {
            saved,
            info,
            extra: Vec::new(),
            reactivate: active.map(|d| d.path.clone()),
            rename: true,
            remove: n_("Remove Profile"),
        },
        move || {
            if let Some(state) = weak.upgrade() {
                refresh(&state);
            }
        },
    );
}

fn vpn_group(state: &Rc<State>, nm: &Nm) -> adw::PreferencesGroup {
    let group = widgets::group(tr("VPN"));
    let add = gtk::Button::builder()
        .icon_name("list-add-symbolic")
        .valign(gtk::Align::Center)
        .css_classes(["flat"])
        .tooltip_text(tr("Import VPN From File…"))
        .build();
    helix_app_common::set_accessible_label(&add, tr("Import VPN From File…"));
    {
        let weak = Rc::downgrade(state);
        add.connect_clicked(move |_| {
            if let Some(state) = weak.upgrade() {
                import(&state);
            }
        });
    }
    group.set_header_suffix(Some(&add));
    let vpns = nm.vpns();
    if vpns.is_empty() {
        group.set_description(Some(tr(
            "No VPNs are set up. Import a WireGuard or OpenVPN configuration file with the + button.",
        )));
    }
    for v in vpns {
        let active = nm.active_of(&v.path).cloned();
        let row = adw::ActionRow::builder()
            .use_markup(false)
            .title(v.id())
            .subtitle(match active.as_ref().map(|a| a.state) {
                Some(2) => tr("Connected"),
                Some(1) => tr("Connecting…"),
                Some(_) => tr("Disconnecting…"),
                None => tr("Off"),
            })
            .build();
        row.add_prefix(
            &gtk::Image::builder()
                .icon_name("network-vpn-symbolic")
                .accessible_role(gtk::AccessibleRole::Presentation)
                .build(),
        );
        let b = gear(&v.id());
        {
            let (weak, path) = (Rc::downgrade(state), v.path.clone());
            b.connect_clicked(move |_| {
                if let Some(state) = weak.upgrade() {
                    vpn_details(&state, &path);
                }
            });
        }
        row.add_suffix(&b);
        let sw = gtk::Switch::builder()
            .valign(gtk::Align::Center)
            .active(active.is_some())
            .build();
        helix_app_common::set_accessible_label(&sw, &v.id());
        row.add_suffix(&sw);
        row.set_activatable_widget(Some(&sw));
        let (weak, path, name) = (Rc::downgrade(state), v.path.clone(), v.id());
        let active_path = active.map(|a| a.path);
        sw.connect_active_notify(move |sw| {
            let Some(state) = weak.upgrade() else { return };
            if state.quiet.get() {
                return;
            }
            let (on, path, active, weak) = (
                sw.is_active(),
                path.clone(),
                active_path.clone(),
                Rc::downgrade(&state),
            );
            let failed = trf!("Could not change {name}", name = name);
            spawn_worker(
                move || {
                    let c = bus::system()?;
                    match (on, active) {
                        (true, _) => nm::activate(&c, &path, "/", "/"),
                        (false, Some(a)) => nm::deactivate(&c, &a),
                        (false, None) => Ok(()),
                    }
                },
                move |r| report(&weak, r, &failed),
            );
        });
        group.add(&row);
    }
    group
}

fn vpn_details(state: &Rc<State>, path: &str) {
    let nm = state.nm.borrow().clone();
    let Some(saved) = nm.saved(path).cloned() else {
        return;
    };
    let mut info = vec![(tr("Type").to_string(), vpn_kind(&saved))];
    if let Some(a) = nm.active_of(path) {
        if let Some(d) = a
            .devices
            .first()
            .and_then(|p| nm.devices.iter().find(|d| d.path == *p))
        {
            push_ip_info(&mut info, &nm, d);
        }
    }
    let weak = Rc::downgrade(state);
    netedit::open(
        &state.ctx,
        Editor {
            saved,
            info,
            extra: Vec::new(),
            reactivate: None,
            rename: true,
            remove: n_("Remove VPN"),
        },
        move || {
            if let Some(state) = weak.upgrade() {
                refresh(&state);
            }
        },
    );
}

/// The kind of VPN, for its details.
fn vpn_kind(saved: &Saved) -> String {
    if saved.kind() == "wireguard" {
        return "WireGuard".into();
    }
    let service = saved.vpn_service();
    match service.rsplit('.').next().unwrap_or("") {
        "openvpn" => "OpenVPN".into(),
        "openconnect" => "OpenConnect".into(),
        "pptp" => "PPTP".into(),
        "l2tp" => "L2TP".into(),
        "vpnc" => "Cisco (vpnc)".into(),
        "strongswan" | "libreswan" => "IPsec".into(),
        "" => tr(nc_!("VPN type", "Other")).into(),
        other => other.to_string(),
    }
}

/// Imports a VPN configuration file: WireGuard files are read here; other
/// files go through `nmcli connection import` and the matching
/// NetworkManager VPN plugin (OpenVPN for `.ovpn` and other `.conf`).
fn import(state: &Rc<State>) {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some(tr("VPN Configuration Files")));
    for p in ["*.conf", "*.ovpn"] {
        filter.add_pattern(p);
    }
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    let chooser = gtk::FileDialog::builder()
        .title(tr("Import VPN"))
        .modal(true)
        .filters(&filters)
        .build();
    let weak = Rc::downgrade(state);
    chooser.open(Some(&state.ctx.window), gio::Cancellable::NONE, move |r| {
        let (Ok(file), Some(state)) = (r, weak.upgrade()) else {
            return;
        };
        let Some(path) = file.path() else { return };
        let weak = Rc::downgrade(&state);
        spawn_worker(
            move || import_file(&path),
            move |r| {
                if let Some(state) = weak.upgrade() {
                    match r {
                        Ok(()) => {}
                        Err(e) => state
                            .ctx
                            .toast(&trf!("Could not import the VPN: {e}", e = e)),
                    }
                    refresh(&state);
                }
            },
        );
    });
}

fn import_file(path: &std::path::Path) -> Result<(), String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "VPN".into());
    if let Some(settings) = netconf::wireguard(&text, &name) {
        let c = bus::system().map_err(|e| e.to_string())?;
        return nm::add(&c, &settings).map(drop).map_err(|e| e.to_string());
    }
    let out = std::process::Command::new("nmcli")
        .args(["connection", "import", "type", "openvpn", "file"])
        .arg(path)
        .env("LC_ALL", "C")
        .output()
        .map_err(|_| tr("nmcli is not installed").to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

/// An entry row for a string key, applied with its apply button.
fn text_row(prefs: &Prefs, key: &'static str, title: &str) -> adw::EntryRow {
    let row = adw::EntryRow::builder()
        .use_markup(false)
        .title(title)
        .show_apply_button(true)
        .text(prefs.get::<String>(key, String::new()))
        .build();
    let p = prefs.clone();
    row.connect_apply(move |r| p.set(key, r.text().trim().to_string()));
    let (weak, p) = (row.downgrade(), prefs.clone());
    watch(prefs, key, move || {
        if let Some(r) = weak.upgrade() {
            if !r.has_focus() {
                r.set_text(&p.get::<String>(key, String::new()));
            }
        }
    });
    row
}

/// An entry row for a string-list key, comma separated.
fn list_row(prefs: &Prefs, key: &'static str, title: &str, default: &[&str]) -> adw::EntryRow {
    let default: Vec<String> = default.iter().map(|s| s.to_string()).collect();
    let get = {
        let (p, d) = (prefs.clone(), default.clone());
        move || p.get::<Vec<String>>(key, d.clone()).join(", ")
    };
    let row = adw::EntryRow::builder()
        .use_markup(false)
        .title(title)
        .show_apply_button(true)
        .text(get())
        .build();
    let p = prefs.clone();
    row.connect_apply(move |r| p.set(key, crate::extensions::split_list(&r.text())));
    let weak = row.downgrade();
    watch(prefs, key, move || {
        if let Some(r) = weak.upgrade() {
            if !r.has_focus() {
                r.set_text(&get());
            }
        }
    });
    row
}

/// A spin row for a port (`i` key).
fn port_row(prefs: &Prefs, key: &'static str, title: &str, name: &str) -> adw::SpinRow {
    let row = adw::SpinRow::with_range(0.0, 65535.0, 1.0);
    row.set_use_markup(false);
    row.set_title(title);
    helix_app_common::set_accessible_label(&row, name);
    row.set_value(prefs.get::<i32>(key, 0) as f64);
    let p = prefs.clone();
    row.connect_value_notify(move |r| {
        let v = r.value() as i32;
        if p.get::<i32>(key, 0) != v {
            p.set(key, v);
        }
    });
    let (weak, p) = (row.downgrade(), prefs.clone());
    watch(prefs, key, move || {
        if let Some(r) = weak.upgrade() {
            r.set_value(p.get::<i32>(key, 0) as f64);
        }
    });
    row
}

fn proxy_group(prefs: &Prefs, ctx: &Rc<Ctx>) -> adw::PreferencesGroup {
    let group = widgets::group(tr("Network Proxy"));
    let mode = widgets::nick_combo(prefs, "mode", "none", PROXY_MODES, tr("Mode"));
    helix_app_common::set_accessible_label(&mode, tr("Network Proxy Mode"));
    group.add(&mode);
    let url = text_row(prefs, "autoconfig-url", tr("Configuration URL"));
    group.add(&url);
    let mut manual: Vec<gtk::Widget> = Vec::new();
    for (kind, label) in [
        ("http", tr("HTTP Proxy")),
        ("https", tr("HTTPS Proxy")),
        ("ftp", tr("FTP Proxy")),
        ("socks", tr("SOCKS Host")),
    ] {
        let schema: &'static str = match kind {
            "http" => "org.gnome.system.proxy.http",
            "https" => "org.gnome.system.proxy.https",
            "ftp" => "org.gnome.system.proxy.ftp",
            _ => "org.gnome.system.proxy.socks",
        };
        let p = ctx.prefs(schema);
        let host = text_row(&p, "host", label);
        let port = port_row(&p, "port", tr("Port"), &trf!("{proxy} Port", proxy = label));
        group.add(&host);
        group.add(&port);
        manual.push(host.upcast());
        manual.push(port.upcast());
    }
    let ignore = list_row(
        prefs,
        "ignore-hosts",
        tr("Ignore Hosts"),
        &["localhost", "127.0.0.0/8", "::1"],
    );
    ignore.set_tooltip_text(Some(tr(
        "Hosts and networks reached without the proxy, separated by commas",
    )));
    group.add(&ignore);
    manual.push(ignore.upcast());
    let sync = {
        let (p, url) = (prefs.clone(), url.clone());
        move || {
            let m = p.get_enum_nick("mode", "none");
            url.set_visible(m == "auto");
            for w in &manual {
                w.set_visible(m == "manual");
            }
        }
    };
    sync();
    let s = sync.clone();
    mode.connect_selected_notify(move |_| s());
    watch(prefs, "mode", sync);
    group
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::testbus;
    use crate::nm::tests::{serve, Log, OFFICE_VPN, WIRED_DEV};

    #[test]
    fn wired_and_vpn_against_the_fake() {
        let Some(bus) = testbus::private_bus() else {
            return;
        };
        let log: Log = Default::default();
        let service = testbus::service(&bus);
        serve(&service, &log);
        let app = testbus::connect(&bus);
        let nm = nm::read(&app).unwrap();
        let wired = nm.wired_devices();
        assert_eq!(wired.len(), 1);
        assert_eq!(status(wired[0]), "Connected — 1000 Mb/s");
        assert_eq!(nm.wired_profiles(wired[0]).len(), 1);
        let vpn = nm.saved(OFFICE_VPN).unwrap();
        assert_eq!(vpn_kind(vpn), "OpenVPN");
        assert!(shape(&nm).contains(OFFICE_VPN));

        // Wired off and a new profile; a VPN on; a WireGuard file.
        nm::disconnect(&app, WIRED_DEV).unwrap();
        nm::add(&app, &wired_profile("Wired Profile 2", "enp3s0")).unwrap();
        nm::activate(&app, OFFICE_VPN, "/", "/").unwrap();
        let wg = netconf::wireguard("[Interface]\nPrivateKey = a2V5\nAddress = 10.0.0.2/32\n[Peer]\nPublicKey = cHVi\nAllowedIPs = 0.0.0.0/0\n", "home").unwrap();
        nm::add(&app, &wg).unwrap();
        let log = log.lock().unwrap().clone();
        assert_eq!(
            log,
            [
                "Disconnect enp3s0".to_string(),
                "AddConnection Wired Profile 2".into(),
                format!("Activate {OFFICE_VPN} / /"),
                "AddConnection home".into(),
            ]
        );
        let s = Saved {
            path: String::new(),
            settings: wired_profile("P", "eth0"),
        };
        assert_eq!((s.interface().as_str(), s.autoconnect()), ("eth0", false));
    }
}
