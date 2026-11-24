//! NetworkManager (`org.freedesktop.NetworkManager` on the system bus),
//! for the Wi-Fi and Network panels. Blocking zbus; worker threads only.
//!
//! One reading takes NetworkManager's object manager (`/org/freedesktop`)
//! and every saved connection's settings.

use std::collections::HashMap;

use helix_app_common::i18n::tr;
use helix_app_common::nc_;
use zbus::blocking::Connection;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};

use crate::bus::{self, get, Obj, Props, Result};

pub const NM: &str = "org.freedesktop.NetworkManager";
const NM_PATH: &str = "/org/freedesktop/NetworkManager";
const ROOT: Obj<'static> = Obj::new(NM, NM_PATH, NM);
const SETTINGS: Obj<'static> = Obj::new(
    NM,
    "/org/freedesktop/NetworkManager/Settings",
    "org.freedesktop.NetworkManager.Settings",
);
const CONNECTION: &str = "org.freedesktop.NetworkManager.Settings.Connection";
const DEVICE: &str = "org.freedesktop.NetworkManager.Device";
const WIRELESS: &str = "org.freedesktop.NetworkManager.Device.Wireless";
const WIRED: &str = "org.freedesktop.NetworkManager.Device.Wired";
const AP: &str = "org.freedesktop.NetworkManager.AccessPoint";
const ACTIVE: &str = "org.freedesktop.NetworkManager.Connection.Active";
const IP4: &str = "org.freedesktop.NetworkManager.IP4Config";
const IP6: &str = "org.freedesktop.NetworkManager.IP6Config";

pub const TYPE_ETHERNET: u32 = 1;
pub const TYPE_WIFI: u32 = 2;
/// `NM_DEVICE_STATE_ACTIVATED`.
pub const STATE_ACTIVATED: u32 = 100;
/// `NM_DEVICE_STATE_UNAVAILABLE` (no cable, radio off…).
pub const STATE_UNAVAILABLE: u32 = 20;
#[cfg(test)]
/// `NM_DEVICE_STATE_DISCONNECTED`.
pub const STATE_DISCONNECTED: u32 = 30;

/// A connection's settings: groups (`connection`, `ipv4`…) of properties.
pub type Settings = HashMap<String, HashMap<String, OwnedValue>>;

/// An owned D-Bus value.
pub fn val<'a>(v: impl Into<Value<'a>>) -> OwnedValue {
    OwnedValue::try_from(v.into()).expect("no file descriptors")
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Device {
    pub path: String,
    pub interface: String,
    pub kind: u32,
    pub state: u32,
    pub managed: bool,
    /// Active connection object, `/` when none.
    pub active: String,
    pub ip4: String,
    pub ip6: String,
    pub hw_address: String,
    pub perm_hw_address: String,
    /// Wired: Mb/s; Wi-Fi: kb/s (`Bitrate`).
    pub speed: u32,
    pub carrier: bool,
    pub access_points: Vec<String>,
    pub active_ap: String,
}

impl Device {
    pub fn activated(&self) -> bool {
        self.state == STATE_ACTIVATED
    }

    /// Activating (between "prepare" and "secondaries").
    pub fn connecting(&self) -> bool {
        (40..STATE_ACTIVATED).contains(&self.state)
    }

    pub fn mac(&self) -> &str {
        if self.perm_hw_address.is_empty() {
            &self.hw_address
        } else {
            &self.perm_hw_address
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct AccessPoint {
    pub path: String,
    pub ssid: Vec<u8>,
    pub strength: u8,
    pub flags: u32,
    pub wpa_flags: u32,
    pub rsn_flags: u32,
    pub frequency: u32,
    pub bssid: String,
}

/// What a network asks of those who join it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Security {
    None,
    /// Opportunistic encryption, no password.
    Owe,
    Wep,
    /// WPA/WPA2 Personal (a password).
    Personal,
    /// WPA3 Personal (a password, SAE).
    Wpa3,
    /// WPA Enterprise (802.1X).
    Enterprise,
}

impl Security {
    pub fn label(self) -> &'static str {
        match self {
            Security::None => tr(nc_!("Wi-Fi security", "None")),
            Security::Owe => tr("Enhanced Open"),
            Security::Wep => tr("WEP"),
            Security::Personal => tr("WPA/WPA2 Personal"),
            Security::Wpa3 => tr("WPA3 Personal"),
            Security::Enterprise => tr("WPA/WPA2 Enterprise"),
        }
    }

    pub fn needs_password(self) -> bool {
        matches!(self, Security::Wep | Security::Personal | Security::Wpa3)
    }

    /// `802-11-wireless-security` `key-mgmt` for a new connection.
    pub fn key_mgmt(self) -> Option<&'static str> {
        match self {
            Security::None => None,
            Security::Owe => Some("owe"),
            Security::Wep => Some("none"),
            Security::Personal => Some("wpa-psk"),
            Security::Wpa3 => Some("sae"),
            Security::Enterprise => Some("wpa-eap"),
        }
    }
}

const KEY_PSK: u32 = 0x100;
const KEY_8021X: u32 = 0x200;
const KEY_SAE: u32 = 0x400;
const KEY_OWE: u32 = 0x800;

impl AccessPoint {
    pub fn security(&self) -> Security {
        let all = self.wpa_flags | self.rsn_flags;
        if all & KEY_8021X != 0 {
            Security::Enterprise
        } else if all & KEY_PSK != 0 {
            Security::Personal
        } else if all & KEY_SAE != 0 {
            Security::Wpa3
        } else if all & KEY_OWE != 0 {
            Security::Owe
        } else if self.flags & 1 != 0 {
            Security::Wep
        } else {
            Security::None
        }
    }

    pub fn name(&self) -> String {
        ssid_text(&self.ssid)
    }
}

/// A network name as text (names are bytes; most are UTF-8).
pub fn ssid_text(ssid: &[u8]) -> String {
    String::from_utf8_lossy(ssid).into_owned()
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Active {
    pub path: String,
    /// The saved connection.
    pub connection: String,
    pub id: String,
    pub kind: String,
    pub devices: Vec<String>,
    /// 1 activating, 2 activated, 3 deactivating.
    pub state: u32,
    pub vpn: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct IpConfig {
    /// `address/prefix`.
    pub addresses: Vec<String>,
    pub gateway: String,
    pub dns: Vec<String>,
}

/// A saved connection.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Saved {
    pub path: String,
    pub settings: Settings,
}

impl Saved {
    fn s(&self, group: &str, key: &str) -> Option<String> {
        self.settings.get(group).and_then(|g| get(g, key))
    }

    pub fn id(&self) -> String {
        self.s("connection", "id").unwrap_or_default()
    }

    /// `802-11-wireless`, `802-3-ethernet`, `vpn`, `wireguard`…
    pub fn kind(&self) -> String {
        self.s("connection", "type").unwrap_or_default()
    }

    pub fn ssid(&self) -> Vec<u8> {
        self.settings
            .get("802-11-wireless")
            .and_then(|g| get(g, "ssid"))
            .unwrap_or_default()
    }

    /// A Wi-Fi hotspot this computer serves.
    pub fn is_hotspot(&self) -> bool {
        self.s("802-11-wireless", "mode").as_deref() == Some("ap")
    }

    pub fn autoconnect(&self) -> bool {
        self.settings
            .get("connection")
            .and_then(|g| get(g, "autoconnect"))
            .unwrap_or(true)
    }

    /// `connection.metered`: 0 unknown, 1 yes, 2 no.
    pub fn metered(&self) -> bool {
        let m: i32 = self
            .settings
            .get("connection")
            .and_then(|g| get(g, "metered"))
            .unwrap_or(0);
        m == 1
    }

    /// The interface it is bound to, when it is.
    pub fn interface(&self) -> String {
        self.s("connection", "interface-name").unwrap_or_default()
    }

    /// VPN plugin (`org.freedesktop.NetworkManager.openvpn`…).
    pub fn vpn_service(&self) -> String {
        self.s("vpn", "service-type").unwrap_or_default()
    }

    pub fn is_vpn(&self) -> bool {
        matches!(self.kind().as_str(), "vpn" | "wireguard")
    }
}

/// Everything the panels show.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Nm {
    pub wireless_enabled: bool,
    pub wireless_hardware_enabled: bool,
    pub wwan_enabled: bool,
    pub devices: Vec<Device>,
    pub access_points: HashMap<String, AccessPoint>,
    pub active: Vec<Active>,
    pub ip4: HashMap<String, IpConfig>,
    pub ip6: HashMap<String, IpConfig>,
    pub saved: Vec<Saved>,
}

fn path(v: Option<OwnedObjectPath>) -> String {
    v.map(|p| p.to_string()).unwrap_or_else(|| "/".into())
}

fn paths(v: Option<Vec<OwnedObjectPath>>) -> Vec<String> {
    v.unwrap_or_default()
        .into_iter()
        .map(|p| p.to_string())
        .collect()
}

fn ip_config(p: &Props, v6: bool) -> IpConfig {
    let addresses = get::<Vec<HashMap<String, OwnedValue>>>(p, "AddressData")
        .unwrap_or_default()
        .iter()
        .filter_map(|a| {
            let address: String = get(a, "address")?;
            let prefix: u32 = get(a, "prefix").unwrap_or(0);
            Some(format!("{address}/{prefix}"))
        })
        .collect();
    let dns = if v6 {
        get::<Vec<Vec<u8>>>(p, "Nameservers")
            .unwrap_or_default()
            .iter()
            .filter_map(|b| <[u8; 16]>::try_from(b.as_slice()).ok())
            .map(|b| std::net::Ipv6Addr::from(b).to_string())
            .collect()
    } else {
        get::<Vec<HashMap<String, OwnedValue>>>(p, "NameserverData")
            .unwrap_or_default()
            .iter()
            .filter_map(|a| get::<String>(a, "address"))
            .collect()
    };
    IpConfig {
        addresses,
        gateway: get(p, "Gateway").unwrap_or_default(),
        dns,
    }
}

/// What NetworkManager has; [`bus::Error::Missing`] when it is not
/// running.
pub fn read(conn: &Connection) -> Result<Nm> {
    let root = ROOT.props(conn)?;
    let objects = Obj::new(NM, "/org/freedesktop", NM).managed_objects(conn)?;
    let mut nm = Nm {
        wireless_enabled: get(&root, "WirelessEnabled").unwrap_or(false),
        wireless_hardware_enabled: get(&root, "WirelessHardwareEnabled").unwrap_or(true),
        wwan_enabled: get(&root, "WwanEnabled").unwrap_or(false),
        ..Nm::default()
    };
    let device_order = paths(get(&root, "Devices"));
    let mut saved_paths = Vec::new();
    for (p, ifaces) in &objects {
        let p = p.to_string();
        if let Some(d) = ifaces.get(DEVICE) {
            let wifi = ifaces.get(WIRELESS);
            let wired = ifaces.get(WIRED);
            let extra = wifi.or(wired);
            nm.devices.push(Device {
                path: p.clone(),
                interface: get(d, "Interface").unwrap_or_default(),
                kind: get(d, "DeviceType").unwrap_or(0),
                state: get(d, "State").unwrap_or(0),
                managed: get(d, "Managed").unwrap_or(true),
                active: path(get(d, "ActiveConnection")),
                ip4: path(get(d, "Ip4Config")),
                ip6: path(get(d, "Ip6Config")),
                hw_address: get(d, "HwAddress").unwrap_or_default(),
                perm_hw_address: extra
                    .and_then(|x| get(x, "PermHwAddress"))
                    .unwrap_or_default(),
                speed: wired
                    .and_then(|x| get(x, "Speed"))
                    .or_else(|| wifi.and_then(|x| get(x, "Bitrate")))
                    .unwrap_or(0),
                carrier: wired.and_then(|x| get(x, "Carrier")).unwrap_or(false),
                access_points: paths(wifi.and_then(|x| get(x, "AccessPoints"))),
                active_ap: path(wifi.and_then(|x| get(x, "ActiveAccessPoint"))),
            });
        }
        if let Some(a) = ifaces.get(AP) {
            nm.access_points.insert(
                p.clone(),
                AccessPoint {
                    path: p.clone(),
                    ssid: get(a, "Ssid").unwrap_or_default(),
                    strength: get(a, "Strength").unwrap_or(0),
                    flags: get(a, "Flags").unwrap_or(0),
                    wpa_flags: get(a, "WpaFlags").unwrap_or(0),
                    rsn_flags: get(a, "RsnFlags").unwrap_or(0),
                    frequency: get(a, "Frequency").unwrap_or(0),
                    bssid: get(a, "HwAddress").unwrap_or_default(),
                },
            );
        }
        if let Some(a) = ifaces.get(ACTIVE) {
            nm.active.push(Active {
                path: p.clone(),
                connection: path(get(a, "Connection")),
                id: get(a, "Id").unwrap_or_default(),
                kind: get(a, "Type").unwrap_or_default(),
                devices: paths(get(a, "Devices")),
                state: get(a, "State").unwrap_or(0),
                vpn: get(a, "Vpn").unwrap_or(false),
            });
        }
        if let Some(c) = ifaces.get(IP4) {
            nm.ip4.insert(p.clone(), ip_config(c, false));
        }
        if let Some(c) = ifaces.get(IP6) {
            nm.ip6.insert(p.clone(), ip_config(c, true));
        }
        if ifaces.contains_key(CONNECTION) {
            saved_paths.push(p.clone());
        }
    }
    nm.devices.sort_by_key(|d| {
        (
            device_order
                .iter()
                .position(|p| *p == d.path)
                .unwrap_or(usize::MAX),
            d.path.clone(),
        )
    });
    nm.active.sort_by(|a, b| a.path.cmp(&b.path));
    saved_paths.sort();
    for p in saved_paths {
        // A connection deleted meanwhile is left out.
        if let Ok(settings) =
            Obj::new(NM, &p, CONNECTION).call::<_, Settings>(conn, "GetSettings", &())
        {
            nm.saved.push(Saved { path: p, settings });
        }
    }
    nm.saved
        .sort_by_key(|s| (s.id().to_lowercase(), s.path.clone()));
    Ok(nm)
}

impl Nm {
    pub fn wifi_devices(&self) -> Vec<&Device> {
        self.devices
            .iter()
            .filter(|d| d.kind == TYPE_WIFI && d.managed)
            .collect()
    }

    pub fn wired_devices(&self) -> Vec<&Device> {
        self.devices
            .iter()
            .filter(|d| d.kind == TYPE_ETHERNET && d.managed)
            .collect()
    }

    pub fn active(&self, path: &str) -> Option<&Active> {
        self.active.iter().find(|a| a.path == path)
    }

    pub fn saved(&self, path: &str) -> Option<&Saved> {
        self.saved.iter().find(|s| s.path == path)
    }

    /// The saved connection active on `device`.
    pub fn device_connection(&self, device: &Device) -> Option<&Saved> {
        self.active(&device.active)
            .and_then(|a| self.saved(&a.connection))
    }

    /// Saved Wi-Fi networks (not hotspots).
    pub fn saved_wifi(&self) -> Vec<&Saved> {
        self.saved
            .iter()
            .filter(|s| s.kind() == "802-11-wireless" && !s.is_hotspot())
            .collect()
    }

    /// The saved connection for a network name, usable on `device`.
    pub fn saved_for(&self, ssid: &[u8], device: &Device) -> Option<&Saved> {
        self.saved_wifi().into_iter().find(|s| {
            s.ssid() == ssid && (s.interface().is_empty() || s.interface() == device.interface)
        })
    }

    /// The hotspot connection Helix made, when there is one.
    pub fn hotspot(&self) -> Option<&Saved> {
        self.saved.iter().find(|s| s.is_hotspot())
    }

    /// Saved connections for wired devices (`device`'s, or unbound ones).
    pub fn wired_profiles(&self, device: &Device) -> Vec<&Saved> {
        self.saved
            .iter()
            .filter(|s| s.kind() == "802-3-ethernet")
            .filter(|s| s.interface().is_empty() || s.interface() == device.interface)
            .collect()
    }

    pub fn vpns(&self) -> Vec<&Saved> {
        self.saved.iter().filter(|s| s.is_vpn()).collect()
    }

    /// The active connection of a saved one.
    pub fn active_of(&self, saved: &str) -> Option<&Active> {
        self.active.iter().find(|a| a.connection == saved)
    }

    /// Networks `device` sees: one per name (hidden ones left out), the
    /// strongest access point of each, strongest first.
    pub fn networks(&self, device: &Device) -> Vec<Network> {
        let mut by_name: Vec<Network> = Vec::new();
        for ap in device
            .access_points
            .iter()
            .filter_map(|p| self.access_points.get(p))
        {
            if ap.ssid.is_empty() {
                continue;
            }
            let active = ap.path == device.active_ap;
            match by_name.iter_mut().find(|n| n.ssid == ap.ssid) {
                Some(n) => {
                    if active || (!n.active && ap.strength > n.strength) {
                        n.ap = ap.path.clone();
                        n.strength = ap.strength;
                    }
                    n.active |= active;
                }
                None => by_name.push(Network {
                    name: ap.name(),
                    ssid: ap.ssid.clone(),
                    ap: ap.path.clone(),
                    strength: ap.strength,
                    security: ap.security(),
                    active,
                    saved: self.saved_for(&ap.ssid, device).map(|s| s.path.clone()),
                }),
            }
        }
        by_name.sort_by_key(|n| {
            (
                !n.active,
                std::cmp::Reverse(n.strength),
                n.name.to_lowercase(),
            )
        });
        by_name
    }
}

/// One network name a device sees.
#[derive(Debug, Clone, PartialEq)]
pub struct Network {
    pub name: String,
    pub ssid: Vec<u8>,
    /// Its strongest access point (or the connected one).
    pub ap: String,
    pub strength: u8,
    pub security: Security,
    pub active: bool,
    /// The saved connection for it.
    pub saved: Option<String>,
}

/// The signal icon for a strength (0–100), as in the shell's status area.
pub fn signal_icon(strength: u8) -> &'static str {
    match strength {
        0..20 => "network-wireless-signal-none-symbolic",
        20..40 => "network-wireless-signal-weak-symbolic",
        40..50 => "network-wireless-signal-ok-symbolic",
        50..80 => "network-wireless-signal-good-symbolic",
        _ => "network-wireless-signal-excellent-symbolic",
    }
}

/// The strength in words, for screen readers.
pub fn signal_text(strength: u8) -> &'static str {
    match strength {
        0..20 => tr("No signal"),
        20..40 => tr("Weak signal"),
        40..50 => tr("OK signal"),
        50..80 => tr("Good signal"),
        _ => tr("Excellent signal"),
    }
}

fn op(p: &str) -> Result<ObjectPath<'_>> {
    ObjectPath::try_from(p).map_err(|e| bus::Error::Other(e.to_string()))
}

pub fn set_wireless(conn: &Connection, on: bool) -> Result<()> {
    ROOT.set_prop(conn, "WirelessEnabled", on)
}

pub fn set_wwan(conn: &Connection, on: bool) -> Result<()> {
    ROOT.set_prop(conn, "WwanEnabled", on)
}

/// Activates a saved connection (`specific`: an access point, or `/`).
pub fn activate(conn: &Connection, saved: &str, device: &str, specific: &str) -> Result<()> {
    ROOT.call_interactive::<_, OwnedObjectPath>(
        conn,
        "ActivateConnection",
        &(op(saved)?, op(device)?, op(specific)?),
    )
    .map(drop)
}

/// Saves and activates a new connection; returns the saved connection.
pub fn add_and_activate(
    conn: &Connection,
    settings: &Settings,
    device: &str,
    specific: &str,
) -> Result<String> {
    let (saved, _active): (OwnedObjectPath, OwnedObjectPath) = ROOT.call_interactive(
        conn,
        "AddAndActivateConnection",
        &(settings, op(device)?, op(specific)?),
    )?;
    Ok(saved.to_string())
}

pub fn deactivate(conn: &Connection, active: &str) -> Result<()> {
    ROOT.run_interactive(conn, "DeactivateConnection", &(op(active)?,))
}

/// Disconnects a device (and keeps it from connecting by itself until
/// asked).
pub fn disconnect(conn: &Connection, device: &str) -> Result<()> {
    Obj::new(NM, device, DEVICE).run_interactive(conn, "Disconnect", &())
}

pub fn request_scan(conn: &Connection, device: &str) -> Result<()> {
    Obj::new(NM, device, WIRELESS).run(
        conn,
        "RequestScan",
        &(HashMap::<String, OwnedValue>::new(),),
    )
}

/// Saves a new connection without activating it.
pub fn add(conn: &Connection, settings: &Settings) -> Result<String> {
    SETTINGS
        .call_interactive::<_, OwnedObjectPath>(conn, "AddConnection", &(settings,))
        .map(|p| p.to_string())
}

pub fn delete(conn: &Connection, saved: &str) -> Result<()> {
    Obj::new(NM, saved, CONNECTION).run_interactive(conn, "Delete", &())
}

/// The secrets of one settings group (`802-11-wireless-security`…).
pub fn secrets(conn: &Connection, saved: &str, group: &str) -> Result<Settings> {
    Obj::new(NM, saved, CONNECTION).call_interactive(conn, "GetSecrets", &(group,))
}

/// Replaces a saved connection's settings, keeping the secrets of
/// `secret_groups` it had (NetworkManager drops secrets an update leaves
/// out).
pub fn update(
    conn: &Connection,
    saved: &str,
    mut settings: Settings,
    secret_groups: &[&str],
) -> Result<()> {
    for group in secret_groups {
        if let Ok(s) = secrets(conn, saved, group) {
            for (g, values) in s {
                settings.entry(g).or_default().extend(values);
            }
        }
    }
    Obj::new(NM, saved, CONNECTION).run_interactive(conn, "Update", &(settings,))
}

/// The groups of `settings` that can hold secrets.
pub fn secret_groups(settings: &Settings) -> Vec<&'static str> {
    ["802-11-wireless-security", "802-1x", "vpn", "wireguard"]
        .into_iter()
        .filter(|g| settings.contains_key(*g))
        .collect()
}

/// Calls `changed` whenever NetworkManager changes (or comes or goes).
pub fn watch(conn: &Connection, changed: impl FnMut() -> bool) {
    bus::watch(conn, &[NM], changed)
}

fn group(pairs: Vec<(&str, OwnedValue)>) -> HashMap<String, OwnedValue> {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

/// A new connection's settings for a network that needs a password.
pub fn wifi_settings(
    name: &str,
    ssid: &[u8],
    security: Security,
    password: &str,
    hidden: bool,
) -> Settings {
    let mut s = Settings::new();
    s.insert(
        "connection".into(),
        group(vec![("id", val(name)), ("type", val("802-11-wireless"))]),
    );
    let mut wifi = group(vec![("ssid", val(ssid.to_vec()))]);
    if hidden {
        wifi.insert("hidden".into(), val(true));
    }
    s.insert("802-11-wireless".into(), wifi);
    if let Some(key_mgmt) = security.key_mgmt() {
        let mut sec = group(vec![("key-mgmt", val(key_mgmt))]);
        match security {
            Security::Wep => {
                sec.insert("wep-key0".into(), val(password));
                // A passphrase rather than a hex or ASCII key.
                let kind: u32 = if matches!(password.len(), 5 | 10 | 13 | 26) {
                    1
                } else {
                    2
                };
                sec.insert("wep-key-type".into(), val(kind));
            }
            Security::Personal | Security::Wpa3 => {
                sec.insert("psk".into(), val(password));
            }
            _ => {}
        }
        s.insert("802-11-wireless-security".into(), sec);
    }
    s
}

/// Settings of a hotspot served by this computer.
pub fn hotspot_settings(name: &str, password: &str) -> Settings {
    let mut s = Settings::new();
    s.insert(
        "connection".into(),
        group(vec![
            ("id", val(tr("Hotspot"))),
            ("type", val("802-11-wireless")),
            ("autoconnect", val(false)),
        ]),
    );
    s.insert(
        "802-11-wireless".into(),
        group(vec![
            ("ssid", val(name.as_bytes().to_vec())),
            ("mode", val("ap")),
        ]),
    );
    s.insert(
        "802-11-wireless-security".into(),
        group(vec![("key-mgmt", val("wpa-psk")), ("psk", val(password))]),
    );
    s.insert("ipv4".into(), group(vec![("method", val("shared"))]));
    s.insert("ipv6".into(), group(vec![("method", val("ignore"))]));
    s
}

/// Whether a password fits a network's security.
pub fn password_ok(security: Security, password: &str) -> bool {
    match security {
        Security::Personal => {
            (8..=63).contains(&password.len())
                || (password.len() == 64 && password.chars().all(|c| c.is_ascii_hexdigit()))
        }
        Security::Wpa3 => !password.is_empty(),
        Security::Wep => !password.is_empty(),
        _ => true,
    }
}

/// The text of a Wi-Fi QR code (the `WIFI:` scheme phones read).
pub fn qr_text(name: &str, security: Security, password: &str) -> String {
    fn esc(s: &str) -> String {
        let mut out = String::new();
        for c in s.chars() {
            if matches!(c, '\\' | ';' | ',' | ':' | '"') {
                out.push('\\');
            }
            out.push(c);
        }
        out
    }
    let kind = match security {
        Security::None | Security::Owe => "nopass",
        Security::Wep => "WEP",
        Security::Wpa3 => "SAE",
        _ => "WPA",
    };
    if kind == "nopass" {
        format!("WIFI:S:{};T:nopass;;", esc(name))
    } else {
        format!("WIFI:S:{};T:{kind};P:{};;", esc(name), esc(password))
    }
}

/// The password of a saved Wi-Fi network (empty for open ones).
pub fn wifi_password(conn: &Connection, saved: &Saved) -> Result<String> {
    if !saved.settings.contains_key("802-11-wireless-security") {
        return Ok(String::new());
    }
    let s = secrets(conn, &saved.path, "802-11-wireless-security")?;
    let sec = s.get("802-11-wireless-security");
    Ok(sec
        .and_then(|g| get::<String>(g, "psk").or_else(|| get(g, "wep-key0")))
        .unwrap_or_default())
}

/// The security of a saved Wi-Fi network.
pub fn saved_security(saved: &Saved) -> Security {
    let key: Option<String> = saved
        .settings
        .get("802-11-wireless-security")
        .and_then(|g| get(g, "key-mgmt"));
    match key.as_deref() {
        None => Security::None,
        Some("owe") => Security::Owe,
        Some("none") | Some("ieee8021x") => Security::Wep,
        Some("sae") => Security::Wpa3,
        Some("wpa-eap") | Some("wpa-eap-suite-b-192") => Security::Enterprise,
        Some(_) => Security::Personal,
    }
}

/// Characters of a hotspot password: lower-case letters and digits
/// without ones that look alike (GNOME Settings' set).
const PASSWORD_CHARS: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// Length of a generated hotspot password (about 50 bits).
pub const PASSWORD_LEN: usize = 10;

/// A random password for a hotspot, from the kernel's random number
/// generator; empty (the user types one) if that cannot be read.
pub fn random_password() -> String {
    use std::io::Read;
    let mut urandom = match std::fs::File::open("/dev/urandom") {
        Ok(f) => f,
        Err(_) => return String::new(),
    };
    password_from(|buf| urandom.read_exact(buf).is_ok()).unwrap_or_default()
}

/// [`PASSWORD_LEN`] characters of [`PASSWORD_CHARS`], each chosen
/// uniformly from bytes `fill` provides (rejection sampling: bytes past
/// the largest multiple of the alphabet's size are drawn again).
fn password_from(mut fill: impl FnMut(&mut [u8]) -> bool) -> Option<String> {
    let n = PASSWORD_CHARS.len();
    let limit = 256 - 256 % n;
    let mut out = String::with_capacity(PASSWORD_LEN);
    let mut buf = [0u8; 32];
    while out.len() < PASSWORD_LEN {
        if !fill(&mut buf) {
            return None;
        }
        for &b in buf.iter().filter(|&&b| (b as usize) < limit) {
            if out.len() == PASSWORD_LEN {
                break;
            }
            out.push(PASSWORD_CHARS[b as usize % n] as char);
        }
    }
    Some(out)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn password_sampling_is_uniform_and_rejects_high_bytes() {
        // Bytes at or above 248 (8 × 31) are skipped, so no character
        // is more likely than another.
        let mut calls = 0;
        let p = password_from(|buf| {
            calls += 1;
            for (i, b) in buf.iter_mut().enumerate() {
                *b = if i % 2 == 0 { 255 } else { 31 * (i % 8) as u8 };
            }
            true
        })
        .unwrap();
        assert_eq!(p, "a".repeat(PASSWORD_LEN));
        assert_eq!(calls, 1);
        // Every byte value below the limit maps to each character the
        // same number of times.
        let mut counts = [0u32; 31];
        for b in 0..248u32 {
            counts[(b % 31) as usize] += 1;
        }
        assert!(counts.iter().all(|&c| c == 8));
        assert!(password_from(|_| false).is_none());
    }
    use crate::bus::testbus;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use zbus::object_server::SignalEmitter;

    pub type Log = Arc<Mutex<Vec<String>>>;

    fn opath(p: &str) -> OwnedObjectPath {
        OwnedObjectPath::try_from(p).unwrap()
    }

    pub struct FakeNm {
        pub wireless: bool,
        pub hw: bool,
        pub wwan: bool,
        pub devices: Vec<String>,
        pub next: u32,
        pub log: Log,
    }

    #[zbus::interface(name = "org.freedesktop.NetworkManager")]
    impl FakeNm {
        fn get_devices(&self) -> Vec<OwnedObjectPath> {
            self.devices.iter().map(|p| opath(p)).collect()
        }
        async fn activate_connection(
            &mut self,
            connection: OwnedObjectPath,
            device: OwnedObjectPath,
            specific: OwnedObjectPath,
            #[zbus(object_server)] server: &zbus::ObjectServer,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            self.log.lock().unwrap().push(format!(
                "Activate {} {} {}",
                connection.as_str(),
                device.as_str(),
                specific.as_str()
            ));
            self.next += 1;
            let p = format!(
                "/org/freedesktop/NetworkManager/ActiveConnection/{}",
                self.next
            );
            server
                .at(
                    p.as_str(),
                    FakeActive {
                        connection: connection.to_string(),
                        id: "activated".into(),
                        kind: "802-11-wireless".into(),
                        devices: vec![device.to_string()],
                        vpn: false,
                    },
                )
                .await?;
            Ok(opath(&p))
        }
        async fn add_and_activate_connection(
            &mut self,
            settings: Settings,
            device: OwnedObjectPath,
            specific: OwnedObjectPath,
            #[zbus(object_server)] server: &zbus::ObjectServer,
        ) -> zbus::fdo::Result<(OwnedObjectPath, OwnedObjectPath)> {
            let mut keys: Vec<String> = settings
                .iter()
                .flat_map(|(g, v)| v.keys().map(move |k| format!("{g}.{k}")))
                .collect();
            keys.sort();
            self.log.lock().unwrap().push(format!(
                "AddAndActivate {} {} [{}]",
                device.as_str(),
                specific.as_str(),
                keys.join(" ")
            ));
            self.next += 1;
            let p = format!(
                "/org/freedesktop/NetworkManager/Settings/{}",
                100 + self.next
            );
            server
                .at(
                    p.as_str(),
                    FakeConnection {
                        settings,
                        secrets: Settings::new(),
                        log: self.log.clone(),
                    },
                )
                .await?;
            Ok((
                opath(&p),
                opath("/org/freedesktop/NetworkManager/ActiveConnection/99"),
            ))
        }
        fn deactivate_connection(&self, active: OwnedObjectPath) {
            self.log
                .lock()
                .unwrap()
                .push(format!("Deactivate {}", active.as_str()));
        }
        #[zbus(property)]
        fn devices(&self) -> Vec<OwnedObjectPath> {
            self.get_devices()
        }
        #[zbus(property)]
        fn wireless_enabled(&self) -> bool {
            self.wireless
        }
        #[zbus(property)]
        fn set_wireless_enabled(&mut self, v: bool) {
            self.log
                .lock()
                .unwrap()
                .push(format!("WirelessEnabled {v}"));
            self.wireless = v;
        }
        #[zbus(property)]
        fn wireless_hardware_enabled(&self) -> bool {
            self.hw
        }
        #[zbus(property)]
        fn wwan_enabled(&self) -> bool {
            self.wwan
        }
        #[zbus(property)]
        fn set_wwan_enabled(&mut self, v: bool) {
            self.log.lock().unwrap().push(format!("WwanEnabled {v}"));
            self.wwan = v;
        }
    }

    pub struct FakeDevice {
        pub interface: String,
        pub kind: u32,
        pub state: u32,
        pub active: String,
        pub ip4: String,
        pub hw: String,
        pub log: Log,
    }

    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device")]
    impl FakeDevice {
        async fn disconnect(
            &mut self,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("Disconnect {}", self.interface));
            self.state = STATE_DISCONNECTED;
            self.state_changed(&e).await?;
            Ok(())
        }
        #[zbus(property)]
        fn interface(&self) -> String {
            self.interface.clone()
        }
        #[zbus(property)]
        fn device_type(&self) -> u32 {
            self.kind
        }
        #[zbus(property)]
        fn state(&self) -> u32 {
            self.state
        }
        #[zbus(property)]
        fn managed(&self) -> bool {
            true
        }
        #[zbus(property)]
        fn active_connection(&self) -> OwnedObjectPath {
            opath(&self.active)
        }
        #[zbus(property)]
        fn ip4_config(&self) -> OwnedObjectPath {
            opath(&self.ip4)
        }
        #[zbus(property)]
        fn ip6_config(&self) -> OwnedObjectPath {
            opath("/")
        }
        #[zbus(property)]
        fn hw_address(&self) -> String {
            self.hw.clone()
        }
    }

    pub struct FakeWireless {
        pub aps: Vec<String>,
        pub active_ap: String,
        pub log: Log,
    }

    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wireless")]
    impl FakeWireless {
        fn request_scan(&self, _options: HashMap<String, OwnedValue>) {
            self.log.lock().unwrap().push("RequestScan".into());
        }
        #[zbus(property)]
        fn access_points(&self) -> Vec<OwnedObjectPath> {
            self.aps.iter().map(|p| opath(p)).collect()
        }
        #[zbus(property)]
        fn active_access_point(&self) -> OwnedObjectPath {
            opath(&self.active_ap)
        }
        #[zbus(property)]
        fn bitrate(&self) -> u32 {
            866_000
        }
        #[zbus(property)]
        fn perm_hw_address(&self) -> String {
            "52:54:00:AA:BB:01".into()
        }
    }

    pub struct FakeWired;

    #[zbus::interface(name = "org.freedesktop.NetworkManager.Device.Wired")]
    impl FakeWired {
        #[zbus(property)]
        fn speed(&self) -> u32 {
            1000
        }
        #[zbus(property)]
        fn carrier(&self) -> bool {
            true
        }
        #[zbus(property)]
        fn perm_hw_address(&self) -> String {
            "52:54:00:AA:BB:02".into()
        }
    }

    pub struct FakeAp {
        pub ssid: String,
        pub strength: u8,
        pub rsn: u32,
    }

    #[zbus::interface(name = "org.freedesktop.NetworkManager.AccessPoint")]
    impl FakeAp {
        #[zbus(property)]
        fn ssid(&self) -> Vec<u8> {
            self.ssid.as_bytes().to_vec()
        }
        #[zbus(property)]
        fn strength(&self) -> u8 {
            self.strength
        }
        #[zbus(property)]
        fn flags(&self) -> u32 {
            u32::from(self.rsn != 0)
        }
        #[zbus(property)]
        fn wpa_flags(&self) -> u32 {
            0
        }
        #[zbus(property)]
        fn rsn_flags(&self) -> u32 {
            self.rsn
        }
        #[zbus(property)]
        fn frequency(&self) -> u32 {
            5180
        }
        #[zbus(property)]
        fn hw_address(&self) -> String {
            "11:22:33:44:55:66".into()
        }
    }

    pub struct FakeActive {
        pub connection: String,
        pub id: String,
        pub kind: String,
        pub devices: Vec<String>,
        pub vpn: bool,
    }

    #[zbus::interface(name = "org.freedesktop.NetworkManager.Connection.Active")]
    impl FakeActive {
        #[zbus(property)]
        fn connection(&self) -> OwnedObjectPath {
            opath(&self.connection)
        }
        #[zbus(property)]
        fn id(&self) -> String {
            self.id.clone()
        }
        #[zbus(property, name = "Type")]
        fn kind(&self) -> String {
            self.kind.clone()
        }
        #[zbus(property)]
        fn devices(&self) -> Vec<OwnedObjectPath> {
            self.devices.iter().map(|p| opath(p)).collect()
        }
        #[zbus(property)]
        fn state(&self) -> u32 {
            2
        }
        #[zbus(property)]
        fn vpn(&self) -> bool {
            self.vpn
        }
    }

    pub struct FakeIp4;

    #[zbus::interface(name = "org.freedesktop.NetworkManager.IP4Config")]
    impl FakeIp4 {
        #[zbus(property)]
        fn address_data(&self) -> Vec<HashMap<String, OwnedValue>> {
            vec![group(vec![
                ("address", val("192.168.1.23")),
                ("prefix", val(24u32)),
            ])]
        }
        #[zbus(property)]
        fn gateway(&self) -> String {
            "192.168.1.1".into()
        }
        #[zbus(property)]
        fn nameserver_data(&self) -> Vec<HashMap<String, OwnedValue>> {
            vec![group(vec![("address", val("192.168.1.1"))])]
        }
    }

    pub struct FakeSettings {
        pub log: Log,
    }

    #[zbus::interface(name = "org.freedesktop.NetworkManager.Settings")]
    impl FakeSettings {
        async fn add_connection(
            &self,
            settings: Settings,
            #[zbus(object_server)] server: &zbus::ObjectServer,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            let id: String = settings
                .get("connection")
                .and_then(|g| get(g, "id"))
                .unwrap_or_default();
            self.log.lock().unwrap().push(format!("AddConnection {id}"));
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(50);
            let p = format!(
                "/org/freedesktop/NetworkManager/Settings/{}",
                NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            );
            let p = p.as_str();
            server
                .at(
                    p,
                    FakeConnection {
                        settings,
                        secrets: Settings::new(),
                        log: self.log.clone(),
                    },
                )
                .await?;
            Ok(opath(p))
        }
    }

    pub struct FakeConnection {
        pub settings: Settings,
        pub secrets: Settings,
        pub log: Log,
    }

    #[zbus::interface(name = "org.freedesktop.NetworkManager.Settings.Connection")]
    impl FakeConnection {
        fn get_settings(&self) -> Settings {
            self.settings.clone()
        }
        fn get_secrets(&self, group: String) -> Settings {
            self.secrets
                .iter()
                .filter(|(g, _)| **g == group)
                .map(|(g, v)| (g.clone(), v.clone()))
                .collect()
        }
        async fn update(
            &mut self,
            settings: Settings,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            let psk: Option<String> = settings
                .get("802-11-wireless-security")
                .and_then(|g| get(g, "psk"));
            self.log.lock().unwrap().push(format!("Update psk={psk:?}"));
            self.settings = settings;
            Self::updated(&e).await?;
            Ok(())
        }
        async fn delete(
            &self,
            #[zbus(header)] h: zbus::message::Header<'_>,
            #[zbus(object_server)] server: &zbus::ObjectServer,
        ) -> zbus::fdo::Result<()> {
            let p = h.path().unwrap().to_string();
            self.log.lock().unwrap().push(format!("Delete {p}"));
            server.remove::<FakeConnection, _>(p.as_str()).await?;
            Ok(())
        }
        #[zbus(signal)]
        async fn updated(e: &SignalEmitter<'_>) -> zbus::Result<()>;
    }

    pub fn wifi_conn(id: &str, ssid: &str, psk: Option<&str>) -> (Settings, Settings) {
        let mut s = wifi_settings(
            id,
            ssid.as_bytes(),
            if psk.is_some() {
                Security::Personal
            } else {
                Security::None
            },
            "",
            false,
        );
        s.get_mut("connection")
            .unwrap()
            .insert("uuid".into(), val(format!("uuid-{id}")));
        if let Some(sec) = s.get_mut("802-11-wireless-security") {
            sec.remove("psk");
        }
        let mut secrets = Settings::new();
        if let Some(p) = psk {
            secrets.insert(
                "802-11-wireless-security".into(),
                group(vec![("psk", val(p))]),
            );
        }
        (s, secrets)
    }

    fn conn_settings(
        id: &str,
        kind: &str,
        extra: Vec<(&str, Vec<(&str, OwnedValue)>)>,
    ) -> Settings {
        let mut s = Settings::new();
        s.insert(
            "connection".into(),
            group(vec![
                ("id", val(id)),
                ("type", val(kind)),
                ("uuid", val(format!("uuid-{id}"))),
            ]),
        );
        for (g, pairs) in extra {
            s.insert(g.into(), group(pairs));
        }
        s
    }

    pub const WIFI_DEV: &str = "/org/freedesktop/NetworkManager/Devices/2";
    pub const WIRED_DEV: &str = "/org/freedesktop/NetworkManager/Devices/1";
    pub const HOME: &str = "/org/freedesktop/NetworkManager/Settings/1";
    pub const OFFICE_VPN: &str = "/org/freedesktop/NetworkManager/Settings/3";

    /// A fake NetworkManager: a wired and a Wi-Fi device (connected to
    /// "Home"), four networks, saved connections for Home, the cable and
    /// a VPN.
    pub fn serve(conn: &Connection, log: &Log) {
        let s = conn.object_server();
        let nm = "/org/freedesktop/NetworkManager";
        s.at(
            nm,
            FakeNm {
                wireless: true,
                hw: true,
                wwan: false,
                devices: vec![WIRED_DEV.into(), WIFI_DEV.into()],
                next: 0,
                log: log.clone(),
            },
        )
        .unwrap();
        s.at(
            WIRED_DEV,
            FakeDevice {
                interface: "enp3s0".into(),
                kind: TYPE_ETHERNET,
                state: STATE_ACTIVATED,
                active: format!("{nm}/ActiveConnection/2"),
                ip4: format!("{nm}/IP4Config/1"),
                hw: "52:54:00:AA:BB:02".into(),
                log: log.clone(),
            },
        )
        .unwrap();
        s.at(WIRED_DEV, FakeWired).unwrap();
        s.at(
            WIFI_DEV,
            FakeDevice {
                interface: "wlp2s0".into(),
                kind: TYPE_WIFI,
                state: STATE_ACTIVATED,
                active: format!("{nm}/ActiveConnection/1"),
                ip4: format!("{nm}/IP4Config/1"),
                hw: "52:54:00:AA:BB:01".into(),
                log: log.clone(),
            },
        )
        .unwrap();
        let aps = [
            ("Home", 72, KEY_PSK),
            ("Home", 30, KEY_PSK),
            ("Café Libre", 55, 0),
            ("Neighbours 5G", 90, KEY_SAE),
            ("Campus", 41, KEY_8021X),
            ("", 80, KEY_PSK),
        ];
        let mut ap_paths = Vec::new();
        for (i, (ssid, strength, rsn)) in aps.into_iter().enumerate() {
            let p = format!("{nm}/AccessPoint/{i}");
            s.at(
                p.as_str(),
                FakeAp {
                    ssid: ssid.into(),
                    strength,
                    rsn,
                },
            )
            .unwrap();
            ap_paths.push(p);
        }
        s.at(
            WIFI_DEV,
            FakeWireless {
                aps: ap_paths,
                active_ap: format!("{nm}/AccessPoint/0"),
                log: log.clone(),
            },
        )
        .unwrap();
        s.at(
            format!("{nm}/ActiveConnection/1").as_str(),
            FakeActive {
                connection: HOME.into(),
                id: "Home".into(),
                kind: "802-11-wireless".into(),
                devices: vec![WIFI_DEV.into()],
                vpn: false,
            },
        )
        .unwrap();
        s.at(
            format!("{nm}/ActiveConnection/2").as_str(),
            FakeActive {
                connection: format!("{nm}/Settings/2"),
                id: "Wired".into(),
                kind: "802-3-ethernet".into(),
                devices: vec![WIRED_DEV.into()],
                vpn: false,
            },
        )
        .unwrap();
        s.at(format!("{nm}/IP4Config/1").as_str(), FakeIp4).unwrap();
        s.at(
            format!("{nm}/Settings").as_str(),
            FakeSettings { log: log.clone() },
        )
        .unwrap();
        let (home, home_secrets) = wifi_conn("Home", "Home", Some("correct horse"));
        s.at(
            HOME,
            FakeConnection {
                settings: home,
                secrets: home_secrets,
                log: log.clone(),
            },
        )
        .unwrap();
        let wired = conn_settings(
            "Wired",
            "802-3-ethernet",
            vec![
                ("ipv4", vec![("method", val("auto"))]),
                ("ipv6", vec![("method", val("auto"))]),
            ],
        );
        s.at(
            format!("{nm}/Settings/2").as_str(),
            FakeConnection {
                settings: wired,
                secrets: Settings::new(),
                log: log.clone(),
            },
        )
        .unwrap();
        let vpn = conn_settings(
            "Office",
            "vpn",
            vec![(
                "vpn",
                vec![(
                    "service-type",
                    val("org.freedesktop.NetworkManager.openvpn"),
                )],
            )],
        );
        s.at(
            OFFICE_VPN,
            FakeConnection {
                settings: vpn,
                secrets: Settings::new(),
                log: log.clone(),
            },
        )
        .unwrap();
        s.at("/org/freedesktop", zbus::fdo::ObjectManager).unwrap();
        conn.request_name(NM).unwrap();
    }

    #[test]
    fn against_a_fake_network_manager() {
        let Some(bus) = testbus::private_bus() else {
            return;
        };
        let app = testbus::connect(&bus);
        assert_eq!(read(&app), Err(bus::Error::Missing));
        let changes = testbus::count_changes(&bus, &[NM]);
        let log: Log = Arc::default();
        let service = testbus::service(&bus);
        serve(&service, &log);
        assert!(testbus::eventually(|| changes.load(Ordering::SeqCst) > 0));

        let nm = read(&app).unwrap();
        assert!(nm.wireless_enabled && nm.wireless_hardware_enabled);
        let wifi = nm.wifi_devices();
        assert_eq!(wifi.len(), 1);
        let dev = wifi[0];
        assert_eq!(
            (dev.interface.as_str(), dev.mac()),
            ("wlp2s0", "52:54:00:AA:BB:01")
        );
        let nets = nm.networks(dev);
        let names: Vec<&str> = nets.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(
            names,
            ["Home", "Neighbours 5G", "Café Libre", "Campus"],
            "connected first, one per name"
        );
        assert!(nets[0].active && nets[0].saved.as_deref() == Some(HOME));
        assert_eq!(nets[0].strength, 72, "the connected access point");
        assert_eq!(
            nets.iter().map(|n| n.security).collect::<Vec<_>>(),
            [
                Security::Personal,
                Security::Wpa3,
                Security::None,
                Security::Enterprise
            ]
        );
        assert_eq!(nm.device_connection(dev).unwrap().id(), "Home");
        let ip = &nm.ip4[&dev.ip4];
        assert_eq!(
            (ip.addresses[0].as_str(), ip.gateway.as_str()),
            ("192.168.1.23/24", "192.168.1.1")
        );
        assert_eq!(nm.wired_devices()[0].speed, 1000);
        assert_eq!(nm.vpns().len(), 1);
        assert_eq!(
            nm.vpns()[0].vpn_service(),
            "org.freedesktop.NetworkManager.openvpn"
        );

        // The password of a saved network, for its QR code.
        let home = nm.saved(HOME).unwrap();
        assert_eq!(wifi_password(&app, home).unwrap(), "correct horse");
        assert_eq!(saved_security(home), Security::Personal);

        // Joining: a saved network, a new open one, a new secured one.
        activate(&app, HOME, WIFI_DEV, &nets[0].ap).unwrap();
        add_and_activate(&app, &Settings::new(), WIFI_DEV, &nets[2].ap).unwrap();
        let s = wifi_settings(
            "Neighbours 5G",
            b"Neighbours 5G",
            Security::Wpa3,
            "secret",
            false,
        );
        let new = add_and_activate(&app, &s, WIFI_DEV, &nets[1].ap).unwrap();
        // Hidden network and hotspot.
        let s = wifi_settings("Attic", b"Attic", Security::Personal, "password1", true);
        add_and_activate(&app, &s, WIFI_DEV, "/").unwrap();
        add_and_activate(&app, &hotspot_settings("helix", "abcdefgh"), WIFI_DEV, "/").unwrap();

        // Changing a saved network keeps its password.
        let mut settings = home.settings.clone();
        settings
            .get_mut("connection")
            .unwrap()
            .insert("autoconnect".into(), val(false));
        let groups = secret_groups(&settings);
        update(&app, HOME, settings, &groups).unwrap();
        let nm = read(&app).unwrap();
        assert!(!nm.saved(HOME).unwrap().autoconnect());
        assert!(nm.saved(&new).is_some(), "joined networks are saved");
        assert!(nm.hotspot().is_some());

        delete(&app, &new).unwrap();
        set_wireless(&app, false).unwrap();
        set_wwan(&app, false).unwrap();
        request_scan(&app, WIFI_DEV).unwrap();
        disconnect(&app, WIRED_DEV).unwrap();
        deactivate(&app, "/org/freedesktop/NetworkManager/ActiveConnection/2").unwrap();
        let log = log.lock().unwrap().clone();
        let ap = |i: u32| format!("/org/freedesktop/NetworkManager/AccessPoint/{i}");
        assert_eq!(
            log,
            [
                format!("Activate {HOME} {WIFI_DEV} {}", ap(0)),
                format!("AddAndActivate {WIFI_DEV} {} []", ap(2)),
                format!(
                    "AddAndActivate {WIFI_DEV} {} [802-11-wireless-security.key-mgmt 802-11-wireless-security.psk 802-11-wireless.ssid connection.id connection.type]",
                    ap(3)
                ),
                format!(
                    "AddAndActivate {WIFI_DEV} / [802-11-wireless-security.key-mgmt 802-11-wireless-security.psk 802-11-wireless.hidden 802-11-wireless.ssid connection.id connection.type]"
                ),
                format!(
                    "AddAndActivate {WIFI_DEV} / [802-11-wireless-security.key-mgmt 802-11-wireless-security.psk 802-11-wireless.mode 802-11-wireless.ssid connection.autoconnect connection.id connection.type ipv4.method ipv6.method]"
                ),
                "Update psk=Some(\"correct horse\")".into(),
                format!("Delete {new}"),
                "WirelessEnabled false".into(),
                "WwanEnabled false".into(),
                "RequestScan".into(),
                "Disconnect enp3s0".into(),
                "Deactivate /org/freedesktop/NetworkManager/ActiveConnection/2".into(),
            ]
        );
        assert!(!read(&app).unwrap().wireless_enabled);
    }

    #[test]
    fn texts_and_checks() {
        assert_eq!(
            qr_text("My;Net", Security::Personal, "pa:ss"),
            "WIFI:S:My\\;Net;T:WPA;P:pa\\:ss;;"
        );
        assert_eq!(
            qr_text("Open", Security::None, ""),
            "WIFI:S:Open;T:nopass;;"
        );
        assert!(!password_ok(Security::Personal, "short"));
        assert!(password_ok(Security::Personal, "long enough"));
        assert!(password_ok(Security::Personal, &"a".repeat(64)));
        assert!(!password_ok(Security::Personal, &"g".repeat(64)));
        assert_eq!(signal_icon(10), "network-wireless-signal-none-symbolic");
        assert_eq!(
            signal_icon(85),
            "network-wireless-signal-excellent-symbolic"
        );
        let p = random_password();
        assert_eq!(p.len(), PASSWORD_LEN);
        assert!(password_ok(Security::Personal, &p));
        assert!(p.bytes().all(|b| PASSWORD_CHARS.contains(&b)));
        assert_ne!(p, random_password());
        let s = wifi_settings("W", b"W", Security::Wep, "12345", false);
        let sec = &s["802-11-wireless-security"];
        assert_eq!(get::<u32>(sec, "wep-key-type"), Some(1));
    }
}
