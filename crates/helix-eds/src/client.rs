//! D-Bus client for the EDS registry and factories.
//!
//! All calls are blocking; run them from worker threads. Opened books and
//! calendars stay alive while the [`Eds`] connection does (EDS ties backend
//! objects to the opener's bus connection).

use crate::ical::{parse_events, Event};
use crate::source::Source;
use crate::vcard::Contact;
use crate::{n_, tr};
use futures_lite::StreamExt;
use std::collections::HashMap;
use std::time::Duration;
use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

pub const SOURCES_BUS: &str = "org.gnome.evolution.dataserver.Sources5";
pub const SOURCES_PATH: &str = "/org/gnome/evolution/dataserver/SourceManager";
pub const SOURCE_MANAGER_IFACE: &str = "org.gnome.evolution.dataserver.SourceManager";
pub const SOURCE_IFACE: &str = "org.gnome.evolution.dataserver.Source";
pub const SOURCE_WRITABLE_IFACE: &str = "org.gnome.evolution.dataserver.Source.Writable";
pub const SOURCE_REMOVABLE_IFACE: &str = "org.gnome.evolution.dataserver.Source.Removable";
pub const BOOK_BUS: &str = "org.gnome.evolution.dataserver.AddressBook10";
pub const BOOK_FACTORY_PATH: &str = "/org/gnome/evolution/dataserver/AddressBookFactory";
pub const BOOK_FACTORY_IFACE: &str = "org.gnome.evolution.dataserver.AddressBookFactory";
pub const BOOK_IFACE: &str = "org.gnome.evolution.dataserver.AddressBook";
pub const BOOK_VIEW_IFACE: &str = "org.gnome.evolution.dataserver.AddressBookView";
pub const CAL_BUS: &str = "org.gnome.evolution.dataserver.Calendar8";
pub const CAL_FACTORY_PATH: &str = "/org/gnome/evolution/dataserver/CalendarFactory";
pub const CAL_FACTORY_IFACE: &str = "org.gnome.evolution.dataserver.CalendarFactory";
pub const CAL_IFACE: &str = "org.gnome.evolution.dataserver.Calendar";
pub const CAL_VIEW_IFACE: &str = "org.gnome.evolution.dataserver.CalendarView";

const PROPERTIES_IFACE: &str = "org.freedesktop.DBus.Properties";

/// The longest [`Eds::wait_backend_settled`] waits.
pub const SETTLE_LIMIT: Duration = Duration::from_secs(5);
/// How long a connection status other than "disconnected" must stay
/// unchanged for [`Eds::wait_backend_settled`] to return.
pub const SETTLE_QUIET: Duration = Duration::from_millis(1500);

/// View flag: deliver existing matches as `ObjectsAdded` after `Start`.
const VIEW_FLAGS_NOTIFY_INITIAL: u32 = 1;

#[derive(Debug, Clone)]
pub enum Error {
    /// From the bus or an EDS service (which translates its own messages).
    Bus(String),
    Invalid(String),
    /// The source, by its display name, cannot be changed.
    ReadOnly(String),
    /// The source, by its display name, cannot be removed.
    NotRemovable(String),
    /// A contact's vCard could not be read.
    BadContact,
}

impl std::fmt::Display for Error {
    /// Messages of this crate's own are translated (see
    /// [`crate::set_translator`]).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let named = |template: &'static str, name: &str| tr(template).replace("{name}", name);
        match self {
            Error::Bus(e) => write!(f, "{}", clean_dbus_error(e)),
            Error::Invalid(e) => write!(f, "{e}"),
            Error::ReadOnly(name) => f.write_str(&named(n_("“{name}” is read-only"), name)),
            Error::NotRemovable(name) => {
                f.write_str(&named(n_("“{name}” cannot be removed"), name))
            }
            Error::BadContact => f.write_str(tr(n_("The contact could not be read"))),
        }
    }
}

impl std::error::Error for Error {}

impl Error {
    /// The bus's or service's own text, with its D-Bus error name, when
    /// the error came from the bus: English for bus-level errors (no
    /// reply, service not running, access denied), so apps can show a
    /// translated explanation instead (`helix_app_common::errors`).
    pub fn bus_text(&self) -> Option<&str> {
        match self {
            Error::Bus(e) => Some(e),
            _ => None,
        }
    }
}

impl From<zbus::Error> for Error {
    fn from(e: zbus::Error) -> Self {
        Error::Bus(e.to_string())
    }
}

impl From<zbus::fdo::Error> for Error {
    fn from(e: zbus::fdo::Error) -> Self {
        Error::Bus(e.to_string())
    }
}

impl From<zbus::zvariant::Error> for Error {
    fn from(e: zbus::zvariant::Error) -> Self {
        Error::Bus(e.to_string())
    }
}

fn clean_dbus_error(e: &str) -> String {
    // "org.gnome.evolution.dataserver.Client.NotOpened: msg" → "msg"
    match e.split_once(": ") {
        Some((head, rest)) if head.contains("org.") || head.contains("Method") => rest.to_string(),
        _ => e.to_string(),
    }
}

pub type Result<T> = std::result::Result<T, Error>;

/// How a change to a recurring event applies (EDS `ECalObjModType` nicks).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModType {
    This,
    ThisAndFuture,
    All,
    OnlyThis,
}

impl ModType {
    pub fn nick(self) -> &'static str {
        match self {
            ModType::This => "this",
            ModType::ThisAndFuture => "this-and-future",
            ModType::All => "all",
            ModType::OnlyThis => "only-this",
        }
    }
}

/// Live view notifications.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ViewEvent {
    /// vCards / iCalendar components.
    Added(Vec<String>),
    Modified(Vec<String>),
    /// Contact UIDs, or calendar ids (`uid` or `uid\nrid`).
    Removed(Vec<String>),
    Progress(u32, String),
    /// Initial notification finished; `Some(message)` on error.
    Complete(Option<String>),
}

/// Splits a calendar view removal id into (uid, rid).
pub fn split_calendar_id(id: &str) -> (String, String) {
    match id.split_once('\n') {
        Some((u, r)) => (u.to_string(), r.to_string()),
        None => (id.to_string(), String::new()),
    }
}

fn proxy(
    conn: &Connection,
    dest: &str,
    path: &str,
    iface: &str,
) -> Result<zbus::blocking::Proxy<'static>> {
    Ok(
        zbus::blocking::proxy::Builder::<zbus::blocking::Proxy<'static>>::new(conn)
            .destination(dest.to_string())?
            .path(path.to_string())?
            .interface(iface.to_string())?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()?,
    )
}

/// A running signal subscription; dropping it stops the listener thread.
pub struct Watch {
    stop: async_channel::Sender<()>,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.close();
    }
}

fn watch_signals(
    conn: &Connection,
    path: Option<&str>,
    iface: Option<&str>,
    mut f: impl FnMut(&zbus::Message) + Send + 'static,
) -> Result<Watch> {
    let mut rule = zbus::MatchRule::builder().msg_type(zbus::message::Type::Signal);
    if let Some(p) = path {
        rule = rule.path(p.to_string())?;
    }
    if let Some(i) = iface {
        rule = rule.interface(i.to_string())?;
    }
    let rule = rule.build();
    let inner = conn.inner().clone();
    // Register the match before returning so no signal is missed.
    let mut stream = zbus::block_on(zbus::MessageStream::for_match_rule(rule, &inner, Some(256)))?;
    let (tx, rx) = async_channel::bounded::<()>(1);
    std::thread::Builder::new()
        .name("helix-eds-watch".into())
        .spawn(move || {
            zbus::block_on(async move {
                loop {
                    let next =
                        futures_lite::future::or(async { stream.next().await.map(Some) }, async {
                            let _ = rx.recv().await;
                            None
                        })
                        .await;
                    match next {
                        Some(Some(Ok(msg))) => f(&msg),
                        Some(Some(Err(_))) => continue,
                        _ => break,
                    }
                }
            })
        })
        .map_err(|e| Error::Invalid(e.to_string()))?;
    Ok(Watch { stop: tx })
}

/// Connection to the EDS services on a bus.
#[derive(Clone)]
pub struct Eds {
    conn: Connection,
}

impl Eds {
    /// Connects to the session bus.
    pub fn session() -> Result<Eds> {
        let conn = zbus::blocking::connection::Builder::session()?
            .method_timeout(Duration::from_secs(120))
            .build()?;
        Ok(Eds { conn })
    }

    /// Connects to a specific bus address (used by tests on private buses).
    pub fn with_address(address: &str) -> Result<Eds> {
        let conn = zbus::blocking::connection::Builder::address(address)?
            .method_timeout(Duration::from_secs(120))
            .build()?;
        Ok(Eds { conn })
    }

    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// All sources known to the registry.
    pub fn sources(&self) -> Result<Vec<Source>> {
        let om = zbus::blocking::fdo::ObjectManagerProxy::builder(&self.conn)
            .destination(SOURCES_BUS)?
            .path(SOURCES_PATH)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()?;
        let objects = om.get_managed_objects()?;
        let mut out = Vec::new();
        for (path, ifaces) in objects {
            if let Some(s) = source_from_ifaces(&path, &ifaces) {
                out.push(s);
            }
        }
        out.sort_by(|a, b| a.uid.cmp(&b.uid));
        Ok(out)
    }

    /// Asks the registry to create a source from key-file `data`.
    pub fn create_source(&self, uid: &str, data: &str) -> Result<()> {
        let p = proxy(&self.conn, SOURCES_BUS, SOURCES_PATH, SOURCE_MANAGER_IFACE)?;
        let mut map = HashMap::new();
        map.insert(uid.to_string(), data.to_string());
        p.call::<_, _, ()>("CreateSources", &(map,))?;
        Ok(())
    }

    /// Saves a source's edited data.
    pub fn write_source(&self, source: &Source) -> Result<()> {
        if !source.writable {
            return Err(Error::ReadOnly(source.display_name()));
        }
        let p = proxy(
            &self.conn,
            SOURCES_BUS,
            &source.object_path,
            SOURCE_WRITABLE_IFACE,
        )?;
        p.call::<_, _, ()>("Write", &(source.data_string(),))?;
        Ok(())
    }

    /// Deletes a source (and its data, for local sources).
    ///
    /// Waits first, up to a few seconds, for a book or calendar backend that
    /// was just closed to finish with the source
    /// ([`Eds::wait_backend_settled`]).
    pub fn remove_source(&self, source: &Source) -> Result<()> {
        if !source.removable {
            return Err(Error::NotRemovable(source.display_name()));
        }
        self.wait_backend_settled(source);
        let p = proxy(
            &self.conn,
            SOURCES_BUS,
            &source.object_path,
            SOURCE_REMOVABLE_IFACE,
        )?;
        p.call::<_, _, ()>("Remove", &())?;
        Ok(())
    }

    /// Waits until no backend is about to set `source`'s connection status,
    /// so that the source can be removed safely. Blocks for up to
    /// [`SETTLE_LIMIT`]; returns at once when the source is disconnected.
    ///
    /// A backend that is closed (its last client closed the book or
    /// calendar) sets the source's `ConnectionStatus` to "disconnected" with
    /// a `Properties.Set` on the registry's object, just after replying to
    /// `Close`. If `Remove` unexports that object before the registry has
    /// handled the Set, `evolution-source-registry` crashes: GLib runs the
    /// queued Set on the freed object (GNOME/glib#2400, fixed on GLib's main
    /// branch after 2.90.1; helixdesktop/apps#205).
    ///
    /// The registry handles calls to the object in order, so once it reports
    /// "disconnected", in a reply to `Get` or in `PropertiesChanged`, the Set
    /// has run. (The backend's other calls on close are method calls, which
    /// GLib drops safely for an unexported object.) A source that stays
    /// "connected" or "connecting" may be held open by another client (the
    /// shell, say), or be a web calendar closed while still connecting, which
    /// never reports "disconnected": then this waits until the status has
    /// not changed for [`SETTLE_QUIET`].
    pub fn wait_backend_settled(&self, source: &Source) {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        // Subscribe before reading, so that no change is missed.
        let watch = watch_signals(
            &self.conn,
            Some(&source.object_path),
            Some(PROPERTIES_IFACE),
            move |msg| {
                if let Some(status) = changed_connection_status(msg) {
                    let _ = tx.send(status);
                }
            },
        );
        let Ok(_watch) = watch else { return };
        let Ok(p) = proxy(&self.conn, SOURCES_BUS, &source.object_path, SOURCE_IFACE) else {
            return;
        };
        // No such property (or source): nothing will set it.
        let Ok(mut status) = p.get_property::<String>("ConnectionStatus") else {
            return;
        };
        let start = std::time::Instant::now();
        let mut since = start;
        while status != "disconnected" {
            let left = SETTLE_QUIET
                .saturating_sub(since.elapsed())
                .min(SETTLE_LIMIT.saturating_sub(start.elapsed()));
            if left.is_zero() {
                return;
            }
            match rx.recv_timeout(left) {
                Ok(s) => {
                    status = s;
                    since = std::time::Instant::now();
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// Calls `f` whenever sources are added, removed or changed.
    pub fn watch_sources(&self, mut f: impl FnMut() + Send + 'static) -> Result<Watch> {
        watch_signals(&self.conn, None, None, move |msg| {
            let h = msg.header();
            let path = h.path().map(|p| p.as_str().to_string()).unwrap_or_default();
            let member = h
                .member()
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            let relevant = path.starts_with(SOURCES_PATH)
                && matches!(
                    member.as_str(),
                    "InterfacesAdded" | "InterfacesRemoved" | "PropertiesChanged"
                );
            if relevant {
                f();
            }
        })
    }

    pub fn open_book(&self, source_uid: &str) -> Result<Book> {
        let factory = proxy(&self.conn, BOOK_BUS, BOOK_FACTORY_PATH, BOOK_FACTORY_IFACE)?;
        let (path, bus): (String, String) = factory.call("OpenAddressBook", &(source_uid,))?;
        let p = proxy(&self.conn, &bus, &path, BOOK_IFACE)?;
        let _: Vec<String> = p.call("Open", &())?;
        let writable = p.get_property::<bool>("Writable").unwrap_or(false);
        Ok(Book {
            conn: self.conn.clone(),
            proxy: p,
            bus,
            source_uid: source_uid.to_string(),
            writable,
        })
    }

    pub fn open_calendar(&self, source_uid: &str) -> Result<Calendar> {
        let factory = proxy(&self.conn, CAL_BUS, CAL_FACTORY_PATH, CAL_FACTORY_IFACE)?;
        let (path, bus): (String, String) = factory.call("OpenCalendar", &(source_uid,))?;
        let p = proxy(&self.conn, &bus, &path, CAL_IFACE)?;
        let _: Vec<String> = p.call("Open", &())?;
        let writable = p.get_property::<bool>("Writable").unwrap_or(false);
        Ok(Calendar {
            conn: self.conn.clone(),
            proxy: p,
            bus,
            source_uid: source_uid.to_string(),
            writable,
        })
    }
}

fn source_from_ifaces(
    path: &OwnedObjectPath,
    ifaces: &HashMap<zbus::names::OwnedInterfaceName, HashMap<String, OwnedValue>>,
) -> Option<Source> {
    let mut props = None;
    let mut writable = false;
    let mut removable = false;
    for (name, p) in ifaces {
        match name.as_str() {
            SOURCE_IFACE => props = Some(p),
            SOURCE_WRITABLE_IFACE => writable = true,
            SOURCE_REMOVABLE_IFACE => removable = true,
            _ => {}
        }
    }
    let props = props?;
    let get = |k: &str| {
        props
            .get(k)
            .and_then(|v| String::try_from(v.try_clone().ok()?).ok())
            .unwrap_or_default()
    };
    let mut s = Source::from_data(&get("UID"), path.as_str(), &get("Data"));
    s.writable = writable;
    s.removable = removable;
    Some(s)
}

/// The new `ConnectionStatus` in a source's `PropertiesChanged` signal.
fn changed_connection_status(msg: &zbus::Message) -> Option<String> {
    if msg.header().member()?.as_str() != "PropertiesChanged" {
        return None;
    }
    let (iface, changed, _): (String, HashMap<String, OwnedValue>, Vec<String>) =
        msg.body().deserialize().ok()?;
    if iface != SOURCE_IFACE {
        return None;
    }
    String::try_from(changed.get("ConnectionStatus")?.try_clone().ok()?).ok()
}

/// A live query on a book or calendar. Dropping it stops and disposes the
/// server-side view.
pub struct View {
    proxy: zbus::blocking::Proxy<'static>,
    _watch: Watch,
}

impl Drop for View {
    fn drop(&mut self) {
        let _ = self.proxy.call::<_, _, ()>("Stop", &());
        let _ = self.proxy.call::<_, _, ()>("Dispose", &());
    }
}

fn start_view(
    conn: &Connection,
    bus: &str,
    path: &str,
    iface: &str,
    mut f: impl FnMut(ViewEvent) + Send + 'static,
) -> Result<View> {
    let view = proxy(conn, bus, path, iface)?;
    let watch = watch_signals(conn, Some(path), Some(iface), move |msg| {
        let h = msg.header();
        let Some(member) = h.member().map(|m| m.as_str().to_string()) else {
            return;
        };
        let body = msg.body();
        let ev = match member.as_str() {
            "ObjectsAdded" => body
                .deserialize::<(Vec<String>,)>()
                .ok()
                .map(|(v,)| ViewEvent::Added(v)),
            "ObjectsModified" => body
                .deserialize::<(Vec<String>,)>()
                .ok()
                .map(|(v,)| ViewEvent::Modified(v)),
            "ObjectsRemoved" => body
                .deserialize::<(Vec<String>,)>()
                .ok()
                .map(|(v,)| ViewEvent::Removed(v)),
            "Progress" => body
                .deserialize::<(u32, String)>()
                .ok()
                .map(|(p, m)| ViewEvent::Progress(p, m)),
            "Complete" => body
                .deserialize::<(String, String)>()
                .ok()
                .map(|(name, m)| ViewEvent::Complete(if name.is_empty() { None } else { Some(m) })),
            _ => None,
        };
        if let Some(ev) = ev {
            f(ev);
        }
    })?;
    view.call::<_, _, ()>("SetFlags", &(VIEW_FLAGS_NOTIFY_INITIAL,))?;
    view.call::<_, _, ()>("Start", &())?;
    Ok(View {
        proxy: view,
        _watch: watch,
    })
}

/// An opened address book.
pub struct Book {
    conn: Connection,
    proxy: zbus::blocking::Proxy<'static>,
    bus: String,
    pub source_uid: String,
    pub writable: bool,
}

impl Book {
    pub fn vcards(&self, query: &str) -> Result<Vec<String>> {
        Ok(self.proxy.call("GetContactList", &(query,))?)
    }

    pub fn contacts(&self, query: &str) -> Result<Vec<Contact>> {
        Ok(self
            .vcards(query)?
            .iter()
            .filter_map(|v| Contact::parse(v))
            .collect())
    }

    pub fn contact(&self, uid: &str) -> Result<Contact> {
        let v: String = self.proxy.call("GetContact", &(uid,))?;
        Contact::parse(&v).ok_or(Error::BadContact)
    }

    /// Creates contacts and returns their new UIDs.
    pub fn create(&self, contacts: &[Contact]) -> Result<Vec<String>> {
        let cards: Vec<String> = contacts.iter().map(Contact::to_vcard_string).collect();
        Ok(self.proxy.call("CreateContacts", &(cards, 0u32))?)
    }

    pub fn modify(&self, contacts: &[Contact]) -> Result<()> {
        let cards: Vec<String> = contacts.iter().map(Contact::to_vcard_string).collect();
        self.proxy
            .call::<_, _, ()>("ModifyContacts", &(cards, 0u32))?;
        Ok(())
    }

    pub fn remove(&self, uids: &[String]) -> Result<()> {
        self.proxy
            .call::<_, _, ()>("RemoveContacts", &(uids.to_vec(), 0u32))?;
        Ok(())
    }

    pub fn view(&self, query: &str, f: impl FnMut(ViewEvent) + Send + 'static) -> Result<View> {
        let path: OwnedObjectPath = self.proxy.call("GetView", &(query,))?;
        start_view(&self.conn, &self.bus, path.as_str(), BOOK_VIEW_IFACE, f)
    }
}

impl Drop for Book {
    fn drop(&mut self) {
        let _ = self.proxy.call::<_, _, ()>("Close", &());
    }
}

/// An opened calendar.
pub struct Calendar {
    conn: Connection,
    proxy: zbus::blocking::Proxy<'static>,
    bus: String,
    pub source_uid: String,
    pub writable: bool,
}

impl Calendar {
    /// Raw iCalendar components matching an S-expression query.
    pub fn objects(&self, query: &str) -> Result<Vec<String>> {
        Ok(self.proxy.call("GetObjectList", &(query,))?)
    }

    pub fn events(&self, query: &str) -> Result<Vec<Event>> {
        Ok(self
            .objects(query)?
            .iter()
            .flat_map(|o| parse_events(o))
            .filter_map(|c| Event::from_component(&c))
            .collect())
    }

    /// One component (`rid` empty for the master / non-recurring event).
    pub fn object(&self, uid: &str, rid: &str) -> Result<String> {
        Ok(self.proxy.call("GetObject", &(uid, rid))?)
    }

    /// Creates components (bare VEVENT text) and returns their UIDs.
    pub fn create(&self, ics: &[String]) -> Result<Vec<String>> {
        Ok(self.proxy.call("CreateObjects", &(ics.to_vec(), 0u32))?)
    }

    pub fn modify(&self, ics: &[String], mod_type: ModType) -> Result<()> {
        self.proxy
            .call::<_, _, ()>("ModifyObjects", &(ics.to_vec(), mod_type.nick(), 0u32))?;
        Ok(())
    }

    /// Removes (uid, rid) pairs; rid empty means the whole series with
    /// [`ModType::All`].
    pub fn remove(&self, ids: &[(String, String)], mod_type: ModType) -> Result<()> {
        self.proxy
            .call::<_, _, ()>("RemoveObjects", &(ids.to_vec(), mod_type.nick(), 0u32))?;
        Ok(())
    }

    /// VTIMEZONE text for a TZID known to the calendar.
    pub fn timezone(&self, tzid: &str) -> Result<String> {
        Ok(self.proxy.call("GetTimezone", &(tzid,))?)
    }

    /// Registers a VTIMEZONE so events can refer to its TZID.
    pub fn add_timezone(&self, vtimezone: &str) -> Result<()> {
        self.proxy.call::<_, _, ()>("AddTimezone", &(vtimezone,))?;
        Ok(())
    }

    pub fn view(&self, query: &str, f: impl FnMut(ViewEvent) + Send + 'static) -> Result<View> {
        let path: OwnedObjectPath = self.proxy.call("GetView", &(query,))?;
        start_view(&self.conn, &self.bus, path.as_str(), CAL_VIEW_IFACE, f)
    }
}

impl Drop for Calendar {
    fn drop(&mut self) {
        let _ = self.proxy.call::<_, _, ()>("Close", &());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calendar_ids() {
        assert_eq!(
            split_calendar_id("a\n20260101T000000Z"),
            ("a".into(), "20260101T000000Z".into())
        );
        assert_eq!(split_calendar_id("a"), ("a".into(), String::new()));
    }

    #[test]
    fn error_cleanup() {
        let e = Error::Bus("org.gnome.evolution.dataserver.Client.NotOpened: Not opened".into());
        assert_eq!(e.to_string(), "Not opened");
        assert_eq!(ModType::ThisAndFuture.nick(), "this-and-future");
    }

    /// This crate's own errors are shown through the app's translator.
    /// The only test that sets it (it is set once per process).
    #[test]
    fn own_errors_are_translated() {
        fn pseudo(msgid: &'static str) -> &'static str {
            match msgid {
                "“{name}” is read-only" => "⟦«{name}» ŕéàð-öñļý⟧",
                "“{name}” cannot be removed" => "⟦«{name}» çàññöţ ƀé ŕéṁöṽéð⟧",
                "The contact could not be read" => "⟦Ţĥé çöñţàçţ çöûļð ñöţ ƀé ŕéàð⟧",
                other => other,
            }
        }
        crate::set_translator(pseudo);
        assert_eq!(
            Error::ReadOnly("Birthdays".into()).to_string(),
            "⟦«Birthdays» ŕéàð-öñļý⟧"
        );
        assert_eq!(
            Error::NotRemovable("Personal".into()).to_string(),
            "⟦«Personal» çàññöţ ƀé ŕéṁöṽéð⟧"
        );
        assert_eq!(
            Error::BadContact.to_string(),
            "⟦Ţĥé çöñţàçţ çöûļð ñöţ ƀé ŕéàð⟧"
        );
    }
}
