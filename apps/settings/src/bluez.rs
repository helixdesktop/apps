//! Bluetooth through BlueZ (`org.bluez` on the system bus) and file
//! sending through the shell (`org.helixdesktop.BluetoothSendTo`, helixdesktop/spec
//! `contracts/dbus/org.bluez.obex.md`). Blocking zbus; worker threads only.

use helix_app_common::i18n::{n_, tr};
use zbus::blocking::Connection;
use zbus::zvariant::ObjectPath;

use crate::bus::{self, get, Obj, Result};

pub const BLUEZ: &str = "org.bluez";
const ADAPTER: &str = "org.bluez.Adapter1";
const DEVICE: &str = "org.bluez.Device1";
const BATTERY: &str = "org.bluez.Battery1";
const ROOT: Obj<'static> = Obj::new(BLUEZ, "/", ADAPTER);

const SHELL: &str = "org.gnome.Shell";
const SEND_TO: Obj<'static> = Obj::new(
    SHELL,
    "/org/helixdesktop/BluetoothSendTo",
    "org.helixdesktop.BluetoothSendTo",
);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Adapter {
    pub path: String,
    pub address: String,
    /// The name other devices see (`Alias`).
    pub name: String,
    pub powered: bool,
    pub discoverable: bool,
    pub discovering: bool,
    /// `PowerState` is `off-blocked` (rfkill).
    pub blocked: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Device {
    pub path: String,
    pub adapter: String,
    pub address: String,
    /// `Alias`, else `Name` (empty when the device has no name).
    pub name: String,
    /// BlueZ's icon name (`audio-headset`, `input-keyboard`…).
    pub icon: String,
    pub paired: bool,
    pub connected: bool,
    pub trusted: bool,
    /// `Battery1` `Percentage`, when the device reports it.
    pub battery: Option<u8>,
}

impl Device {
    /// What to call it: its name, else its address.
    pub fn title(&self) -> &str {
        if self.name.is_empty() {
            &self.address
        } else {
            &self.name
        }
    }

    /// The status line under its name.
    pub fn status(&self) -> String {
        let state = if self.connected {
            tr("Connected")
        } else if self.paired {
            tr("Not connected")
        } else {
            tr("Not paired")
        };
        match self.battery {
            Some(b) if self.connected => {
                helix_app_common::trf!("{state} · Battery {percent} %", state = state, percent = b)
            }
            _ => state.to_owned(),
        }
    }
}

/// Everything the panel shows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub adapters: Vec<Adapter>,
    /// Paired devices first, then by name.
    pub devices: Vec<Device>,
}

impl State {
    /// The adapter the panel works with (the first one, `hci0` usually).
    pub fn adapter(&self) -> Option<&Adapter> {
        self.adapters.first()
    }

    /// Devices of the panel's adapter.
    pub fn devices(&self) -> impl Iterator<Item = &Device> {
        let adapter = self.adapter().map(|a| a.path.clone()).unwrap_or_default();
        self.devices.iter().filter(move |d| d.adapter == adapter)
    }
}

/// What BlueZ has; [`bus::Error::Missing`] when it is not running.
pub fn read(conn: &Connection) -> Result<State> {
    let objects = ROOT.managed_objects(conn)?;
    let mut state = State::default();
    for (path, ifaces) in &objects {
        if let Some(a) = ifaces.get(ADAPTER) {
            state.adapters.push(Adapter {
                path: path.to_string(),
                address: get(a, "Address").unwrap_or_default(),
                name: get::<String>(a, "Alias")
                    .or_else(|| get(a, "Name"))
                    .unwrap_or_default(),
                powered: get(a, "Powered").unwrap_or(false),
                discoverable: get(a, "Discoverable").unwrap_or(false),
                discovering: get(a, "Discovering").unwrap_or(false),
                blocked: get::<String>(a, "PowerState").as_deref() == Some("off-blocked"),
            });
        }
        if let Some(d) = ifaces.get(DEVICE) {
            let address: String = get(d, "Address").unwrap_or_default();
            // BlueZ makes the alias up from the address for nameless
            // devices; those count as nameless.
            let name = get::<String>(d, "Name")
                .or_else(|| get(d, "Alias"))
                .filter(|n| n.replace('-', ":") != address)
                .unwrap_or_default();
            let alias = get::<String>(d, "Alias")
                .filter(|n| !n.is_empty() && n.replace('-', ":") != address)
                .unwrap_or(name);
            state.devices.push(Device {
                path: path.to_string(),
                adapter: get::<zbus::zvariant::OwnedObjectPath>(d, "Adapter")
                    .map(|p| p.to_string())
                    .unwrap_or_default(),
                address,
                name: alias,
                icon: get(d, "Icon").unwrap_or_default(),
                paired: get(d, "Paired").unwrap_or(false),
                connected: get(d, "Connected").unwrap_or(false),
                trusted: get(d, "Trusted").unwrap_or(false),
                battery: ifaces.get(BATTERY).and_then(|b| get(b, "Percentage")),
            });
        }
    }
    state.adapters.sort_by(|a, b| a.path.cmp(&b.path));
    state
        .devices
        .sort_by_key(|d| (!d.paired, d.title().to_lowercase(), d.path.clone()));
    Ok(state)
}

fn adapter(path: &str) -> Obj<'_> {
    Obj::new(BLUEZ, path, ADAPTER)
}

fn device(path: &str) -> Obj<'_> {
    Obj::new(BLUEZ, path, DEVICE)
}

pub fn set_powered(conn: &Connection, adapter_path: &str, on: bool) -> Result<()> {
    adapter(adapter_path).set_prop(conn, "Powered", on)
}

pub fn set_discoverable(conn: &Connection, adapter_path: &str, on: bool) -> Result<()> {
    adapter(adapter_path).set_prop(conn, "Discoverable", on)
}

pub fn set_name(conn: &Connection, adapter_path: &str, name: &str) -> Result<()> {
    adapter(adapter_path).set_prop(conn, "Alias", name)
}

pub fn discover(conn: &Connection, adapter_path: &str, on: bool) -> Result<()> {
    adapter(adapter_path).run(
        conn,
        if on {
            "StartDiscovery"
        } else {
            "StopDiscovery"
        },
        &(),
    )
}

/// Pairs (BlueZ asks the shell's agent for codes), trusts and connects.
/// Pairing can take as long as the user needs.
pub fn pair(conn: &Connection, device_path: &str) -> Result<()> {
    let d = device(device_path);
    d.run(conn, "Pair", &())?;
    d.set_prop(conn, "Trusted", true)?;
    // Some devices pair but do not take connections (keyboards that are
    // not in use, phones): they stay paired.
    let _ = d.run(conn, "Connect", &());
    Ok(())
}

pub fn connect(conn: &Connection, device_path: &str, on: bool) -> Result<()> {
    device(device_path).run(conn, if on { "Connect" } else { "Disconnect" }, &())
}

/// Forgets a device: BlueZ drops its pairing.
pub fn forget(conn: &Connection, adapter_path: &str, device_path: &str) -> Result<()> {
    let path = ObjectPath::try_from(device_path).map_err(|e| bus::Error::Other(e.to_string()))?;
    adapter(adapter_path).run(conn, "RemoveDevice", &(path,))
}

/// Asks the shell to send files to `address`: it asks for the files and
/// shows the progress.
pub fn send_files(conn: &Connection, address: &str) -> Result<()> {
    SEND_TO.run(conn, "SendFiles", &(address, Vec::<String>::new()))
}

/// Calls `changed` whenever BlueZ changes (or comes or goes).
pub fn watch(conn: &Connection, changed: impl FnMut() -> bool) {
    bus::watch(conn, &[BLUEZ], changed)
}

/// What kind of device BlueZ's icon name says it is.
pub fn kind_label(icon: &str) -> &'static str {
    const KINDS: &[(&str, &str)] = &[
        ("audio-headset", n_("Headset")),
        ("audio-headphones", n_("Headphones")),
        ("audio-card", n_("Speaker")),
        ("input-keyboard", n_("Keyboard")),
        ("input-mouse", n_("Mouse")),
        ("input-tablet", n_("Tablet")),
        ("input-gaming", n_("Game Controller")),
        ("phone", n_("Phone")),
        ("computer", n_("Computer")),
        ("camera-photo", n_("Camera")),
        ("camera-video", n_("Video Camera")),
        ("printer", n_("Printer")),
        ("scanner", n_("Scanner")),
        ("modem", n_("Modem")),
        ("network-wireless", n_("Network")),
        ("video-display", n_("Display")),
        ("multimedia-player", n_("Media Player")),
    ];
    KINDS
        .iter()
        .find(|(i, _)| *i == icon)
        .map(|(_, l)| tr(l))
        .unwrap_or_else(|| tr("Other Device"))
}

/// A symbolic icon for BlueZ's icon name.
pub fn icon_name(icon: &str) -> String {
    match icon {
        "" => "bluetooth-symbolic".into(),
        "audio-card" => "audio-speakers-symbolic".into(),
        "phone" => "phone-symbolic".into(),
        "computer" => "computer-symbolic".into(),
        i => format!("{i}-symbolic"),
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::bus::testbus;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use zbus::object_server::SignalEmitter;
    use zbus::zvariant::OwnedObjectPath;

    /// What the fake was asked to do.
    pub type Log = Arc<Mutex<Vec<String>>>;

    pub struct FakeAdapter {
        pub alias: String,
        pub powered: bool,
        pub discoverable: bool,
        pub discovering: bool,
        pub log: Log,
    }

    #[zbus::interface(name = "org.bluez.Adapter1")]
    impl FakeAdapter {
        async fn start_discovery(
            &mut self,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            self.log.lock().unwrap().push("StartDiscovery".into());
            self.discovering = true;
            self.discovering_changed(&e).await?;
            Ok(())
        }
        async fn stop_discovery(
            &mut self,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            self.log.lock().unwrap().push("StopDiscovery".into());
            self.discovering = false;
            self.discovering_changed(&e).await?;
            Ok(())
        }
        async fn remove_device(
            &self,
            device: OwnedObjectPath,
            #[zbus(object_server)] server: &zbus::ObjectServer,
        ) -> zbus::fdo::Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("RemoveDevice {}", device.as_str()));
            server.remove::<FakeDevice, _>(device.as_str()).await?;
            let _ = server.remove::<FakeBattery, _>(device.as_str()).await;
            Ok(())
        }
        #[zbus(property)]
        fn address(&self) -> String {
            "00:11:22:33:44:55".into()
        }
        #[zbus(property)]
        fn alias(&self) -> String {
            self.alias.clone()
        }
        #[zbus(property)]
        fn set_alias(&mut self, v: String) {
            self.log.lock().unwrap().push(format!("Alias {v}"));
            self.alias = v;
        }
        #[zbus(property)]
        fn powered(&self) -> bool {
            self.powered
        }
        #[zbus(property)]
        fn set_powered(&mut self, v: bool) {
            self.log.lock().unwrap().push(format!("Powered {v}"));
            self.powered = v;
        }
        #[zbus(property)]
        fn power_state(&self) -> String {
            if self.powered { "on" } else { "off" }.into()
        }
        #[zbus(property)]
        fn discoverable(&self) -> bool {
            self.discoverable
        }
        #[zbus(property)]
        fn set_discoverable(&mut self, v: bool) {
            self.log.lock().unwrap().push(format!("Discoverable {v}"));
            self.discoverable = v;
        }
        #[zbus(property)]
        fn discovering(&self) -> bool {
            self.discovering
        }
    }

    pub struct FakeDevice {
        pub address: String,
        pub alias: String,
        pub icon: String,
        pub paired: bool,
        pub connected: bool,
        pub trusted: bool,
        pub refuse_pairing: bool,
        pub log: Log,
    }

    #[zbus::interface(name = "org.bluez.Device1")]
    impl FakeDevice {
        async fn pair(
            &mut self,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("Pair {}", self.alias));
            if self.refuse_pairing {
                return Err(zbus::fdo::Error::Failed("Authentication Rejected".into()));
            }
            self.paired = true;
            self.paired_changed(&e).await?;
            Ok(())
        }
        async fn connect(
            &mut self,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("Connect {}", self.alias));
            self.connected = true;
            self.connected_changed(&e).await?;
            Ok(())
        }
        async fn disconnect(
            &mut self,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("Disconnect {}", self.alias));
            self.connected = false;
            self.connected_changed(&e).await?;
            Ok(())
        }
        #[zbus(property)]
        fn address(&self) -> String {
            self.address.clone()
        }
        #[zbus(property)]
        fn alias(&self) -> String {
            self.alias.clone()
        }
        #[zbus(property)]
        fn icon(&self) -> String {
            self.icon.clone()
        }
        #[zbus(property)]
        fn paired(&self) -> bool {
            self.paired
        }
        #[zbus(property)]
        fn connected(&self) -> bool {
            self.connected
        }
        #[zbus(property)]
        fn trusted(&self) -> bool {
            self.trusted
        }
        #[zbus(property)]
        fn set_trusted(&mut self, v: bool) {
            self.log
                .lock()
                .unwrap()
                .push(format!("Trusted {} {v}", self.alias));
            self.trusted = v;
        }
        #[zbus(property)]
        fn adapter(&self) -> OwnedObjectPath {
            OwnedObjectPath::try_from("/org/bluez/hci0").unwrap()
        }
    }

    pub struct FakeBattery(pub u8);

    #[zbus::interface(name = "org.bluez.Battery1")]
    impl FakeBattery {
        #[zbus(property)]
        fn percentage(&self) -> u8 {
            self.0
        }
    }

    pub struct FakeSendTo(pub Log);

    #[zbus::interface(name = "org.helixdesktop.BluetoothSendTo")]
    impl FakeSendTo {
        fn send_files(&self, device: String, files: Vec<String>) {
            self.0
                .lock()
                .unwrap()
                .push(format!("SendFiles {device} {files:?}"));
        }
    }

    fn dev(
        log: &Log,
        address: &str,
        alias: &str,
        icon: &str,
        paired: bool,
        connected: bool,
    ) -> FakeDevice {
        FakeDevice {
            address: address.into(),
            alias: alias.into(),
            icon: icon.into(),
            paired,
            connected,
            trusted: paired,
            refuse_pairing: alias == "Stubborn Speaker",
            log: log.clone(),
        }
    }

    /// A fake BlueZ with one adapter, two paired devices and two nearby
    /// ones, on `conn` (which then owns `org.bluez`).
    pub fn serve(conn: &Connection, log: &Log) {
        let server = conn.object_server();
        server
            .at(
                "/org/bluez/hci0",
                FakeAdapter {
                    alias: "helix-laptop".into(),
                    powered: true,
                    discoverable: false,
                    discovering: false,
                    log: log.clone(),
                },
            )
            .unwrap();
        let devices = [
            (
                "AA:BB:CC:00:00:01",
                "Studio Headphones",
                "audio-headphones",
                true,
                true,
            ),
            (
                "AA:BB:CC:00:00:02",
                "Travel Keyboard",
                "input-keyboard",
                true,
                false,
            ),
            (
                "AA:BB:CC:00:00:03",
                "Stubborn Speaker",
                "audio-card",
                false,
                false,
            ),
            ("AA:BB:CC:00:00:04", "Pixel Phone", "phone", false, false),
        ];
        for (address, alias, icon, paired, connected) in devices {
            let path = format!("/org/bluez/hci0/dev_{}", address.replace(':', "_"));
            server
                .at(
                    path.as_str(),
                    dev(log, address, alias, icon, paired, connected),
                )
                .unwrap();
            if alias == "Studio Headphones" {
                server.at(path.as_str(), FakeBattery(80)).unwrap();
            }
        }
        server.at("/", zbus::fdo::ObjectManager).unwrap();
        conn.request_name(BLUEZ).unwrap();
    }

    /// A fake shell serving only `org.helixdesktop.BluetoothSendTo`.
    pub fn serve_send_to(conn: &Connection, log: &Log) {
        conn.object_server()
            .at("/org/helixdesktop/BluetoothSendTo", FakeSendTo(log.clone()))
            .unwrap();
        conn.request_name(SHELL).unwrap();
    }

    const HEADPHONES: &str = "/org/bluez/hci0/dev_AA_BB_CC_00_00_01";
    const SPEAKER: &str = "/org/bluez/hci0/dev_AA_BB_CC_00_00_03";
    const PHONE: &str = "/org/bluez/hci0/dev_AA_BB_CC_00_00_04";

    #[test]
    fn against_a_fake_bluez() {
        let Some(bus) = testbus::private_bus() else {
            return;
        };
        let app = testbus::connect(&bus);
        // Not running yet.
        assert_eq!(read(&app), Err(bus::Error::Missing));

        let changes = testbus::count_changes(&bus, &[BLUEZ]);
        let log: Log = Arc::default();
        let service = testbus::service(&bus);
        serve(&service, &log);
        assert!(
            testbus::eventually(|| changes.load(Ordering::SeqCst) > 0),
            "appearing is a change"
        );

        let state = read(&app).unwrap();
        let a = state.adapter().unwrap();
        assert_eq!(
            (a.name.as_str(), a.powered, a.blocked),
            ("helix-laptop", true, false)
        );
        let names: Vec<&str> = state.devices().map(|d| d.title()).collect();
        assert_eq!(
            names,
            [
                "Studio Headphones",
                "Travel Keyboard",
                "Pixel Phone",
                "Stubborn Speaker"
            ]
        );
        let headphones = &state.devices[0];
        assert_eq!(headphones.battery, Some(80));
        assert_eq!(headphones.status(), "Connected · Battery 80 %");
        assert_eq!(state.devices[1].status(), "Not connected");
        assert_eq!(state.devices[2].status(), "Not paired");

        // Changes are followed.
        let before = changes.load(Ordering::SeqCst);
        set_powered(&app, &a.path, false).unwrap();
        set_name(&app, &a.path, "Desk").unwrap();
        set_discoverable(&app, &a.path, true).unwrap();
        discover(&app, &a.path, true).unwrap();
        assert!(testbus::eventually(
            || changes.load(Ordering::SeqCst) > before
        ));
        let state = read(&app).unwrap();
        let a = state.adapter().unwrap();
        assert_eq!(
            (a.name.as_str(), a.powered, a.discoverable, a.discovering),
            ("Desk", false, true, true)
        );

        connect(&app, HEADPHONES, false).unwrap();
        pair(&app, PHONE).unwrap();
        let state = read(&app).unwrap();
        assert!(!state
            .devices
            .iter()
            .any(|d| d.path == HEADPHONES && d.connected));
        let phone = state.devices.iter().find(|d| d.path == PHONE).unwrap();
        assert!(phone.paired && phone.trusted && phone.connected);

        // A refused pairing is an error with BlueZ's reason.
        assert_eq!(
            pair(&app, SPEAKER),
            Err(bus::Error::Other("Authentication Rejected".into()))
        );

        forget(&app, &a.path, HEADPHONES).unwrap();
        assert!(read(&app)
            .unwrap()
            .devices
            .iter()
            .all(|d| d.path != HEADPHONES));

        assert_eq!(
            *log.lock().unwrap(),
            [
                "Powered false",
                "Alias Desk",
                "Discoverable true",
                "StartDiscovery",
                "Disconnect Studio Headphones",
                "Pair Pixel Phone",
                "Trusted Pixel Phone true",
                "Connect Pixel Phone",
                "Pair Stubborn Speaker",
                format!("RemoveDevice {HEADPHONES}").as_str(),
            ]
        );

        // Sending goes through the shell, when it is there.
        assert_eq!(
            send_files(&app, "AA:BB:CC:00:00:02"),
            Err(bus::Error::Missing)
        );
        let shell = testbus::service(&bus);
        serve_send_to(&shell, &log);
        send_files(&app, "AA:BB:CC:00:00:02").unwrap();
        assert_eq!(
            log.lock().unwrap().last().unwrap(),
            "SendFiles AA:BB:CC:00:00:02 []"
        );

        // BlueZ going away is a change too.
        let before = changes.load(Ordering::SeqCst);
        service.close().unwrap();
        assert!(testbus::eventually(
            || changes.load(Ordering::SeqCst) > before
        ));
    }

    #[test]
    fn labels() {
        assert_eq!(kind_label("input-keyboard"), "Keyboard");
        assert_eq!(kind_label("something"), "Other Device");
        assert_eq!(icon_name(""), "bluetooth-symbolic");
        assert_eq!(icon_name("input-mouse"), "input-mouse-symbolic");
        let d = Device {
            address: "AA:BB".into(),
            ..Default::default()
        };
        assert_eq!(d.title(), "AA:BB");
    }
}
