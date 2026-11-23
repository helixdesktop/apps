//! D-Bus plumbing shared by the panels that talk to system and session
//! services (BlueZ, NetworkManager, AccountsService, DisplayConfig, the
//! permission store). Blocking zbus; call only from worker threads.
//!
//! Every function takes the connection, so tests run them against fake
//! services on a private `dbus-daemon`; the panels pass
//! [`system`] or [`session`].

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::zvariant::{DynamicDeserialize, DynamicType, OwnedObjectPath, OwnedValue, Value};

pub type Result<T> = std::result::Result<T, Error>;

/// Properties of one interface, by name.
pub type Props = HashMap<String, OwnedValue>;
/// `GetManagedObjects`: interfaces and their properties, by object.
pub type Objects = HashMap<OwnedObjectPath, HashMap<String, Props>>;

/// How a call failed.
#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    /// The service is not on the bus (or the bus is not there).
    Missing,
    /// The user or polkit said no, or the user cancelled.
    Denied,
    /// Anything else, with the service's message.
    Other(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Missing => f.write_str("service not available"),
            Error::Denied => f.write_str("not allowed"),
            Error::Other(m) => f.write_str(m),
        }
    }
}

impl From<zbus::Error> for Error {
    fn from(e: zbus::Error) -> Self {
        match e {
            zbus::Error::MethodError(name, msg, _) => {
                let name = name.as_str();
                if matches!(
                    name,
                    "org.freedesktop.DBus.Error.ServiceUnknown"
                        | "org.freedesktop.DBus.Error.NameHasNoOwner"
                ) {
                    Error::Missing
                } else if name == "org.freedesktop.DBus.Error.AccessDenied"
                    || name.ends_with(".PermissionDenied")
                    || name.ends_with(".NotAuthorized")
                    || name.ends_with(".NotPrivileged")
                    || name.ends_with(".AuthenticationCanceled")
                    || name.ends_with(".Error.Canceled")
                {
                    Error::Denied
                } else {
                    Error::Other(msg.unwrap_or_else(|| name.to_owned()))
                }
            }
            zbus::Error::FDO(e) => match *e {
                zbus::fdo::Error::ServiceUnknown(_) | zbus::fdo::Error::NameHasNoOwner(_) => {
                    Error::Missing
                }
                zbus::fdo::Error::AccessDenied(_) => Error::Denied,
                other => Error::Other(other.to_string()),
            },
            zbus::Error::InputOutput(_) | zbus::Error::Address(_) | zbus::Error::Handshake(_) => {
                Error::Missing
            }
            other => Error::Other(other.to_string()),
        }
    }
}

impl From<zbus::fdo::Error> for Error {
    fn from(e: zbus::fdo::Error) -> Self {
        Error::from(zbus::Error::FDO(Box::new(e)))
    }
}

impl From<zbus::zvariant::Error> for Error {
    fn from(e: zbus::zvariant::Error) -> Self {
        Error::Other(e.to_string())
    }
}

pub fn system() -> Result<Connection> {
    Connection::system().map_err(|_| Error::Missing)
}

pub fn session() -> Result<Connection> {
    Connection::session().map_err(|_| Error::Missing)
}

/// One service object: where calls go.
#[derive(Debug, Clone, Copy)]
pub struct Obj<'a> {
    pub dest: &'a str,
    pub path: &'a str,
    pub iface: &'a str,
}

impl<'a> Obj<'a> {
    pub const fn new(dest: &'a str, path: &'a str, iface: &'a str) -> Self {
        Obj { dest, path, iface }
    }

    /// Calls `method`; the reply's body is deserialized to `R`.
    pub fn call<B, R>(&self, conn: &Connection, method: &str, body: &B) -> Result<R>
    where
        B: serde::ser::Serialize + DynamicType,
        R: for<'d> DynamicDeserialize<'d>,
    {
        let reply = conn.call_method(Some(self.dest), self.path, Some(self.iface), method, body)?;
        Ok(reply.body().deserialize()?)
    }

    /// Calls a method that returns nothing.
    pub fn run<B>(&self, conn: &Connection, method: &str, body: &B) -> Result<()>
    where
        B: serde::ser::Serialize + DynamicType,
    {
        conn.call_method(Some(self.dest), self.path, Some(self.iface), method, body)?;
        Ok(())
    }

    /// Calls `method`, letting polkit ask the user (the call may then
    /// take a while).
    pub fn call_interactive<B, R>(&self, conn: &Connection, method: &str, body: &B) -> Result<R>
    where
        B: serde::ser::Serialize + DynamicType,
        R: for<'d> DynamicDeserialize<'d>,
    {
        let proxy = zbus::blocking::proxy::Builder::<zbus::blocking::Proxy>::new(conn)
            .destination(self.dest)?
            .path(self.path)?
            .interface(self.iface)?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()?;
        proxy
            .call_with_flags(
                method,
                zbus::proxy::MethodFlags::AllowInteractiveAuth.into(),
                body,
            )?
            .ok_or_else(|| Error::Other("no reply".into()))
    }

    /// [`Obj::call_interactive`] for a method that returns nothing.
    pub fn run_interactive<B>(&self, conn: &Connection, method: &str, body: &B) -> Result<()>
    where
        B: serde::ser::Serialize + DynamicType,
    {
        self.call_interactive::<B, ()>(conn, method, body)
    }

    pub fn props(&self, conn: &Connection) -> Result<Props> {
        Obj::new(self.dest, self.path, "org.freedesktop.DBus.Properties").call(
            conn,
            "GetAll",
            &(self.iface,),
        )
    }

    pub fn set_prop<'v>(
        &self,
        conn: &Connection,
        name: &str,
        value: impl Into<Value<'v>>,
    ) -> Result<()> {
        Obj::new(self.dest, self.path, "org.freedesktop.DBus.Properties").run(
            conn,
            "Set",
            &(self.iface, name, value.into()),
        )
    }

    /// `GetManagedObjects` of the object manager at this path.
    pub fn managed_objects(&self, conn: &Connection) -> Result<Objects> {
        Obj::new(self.dest, self.path, "org.freedesktop.DBus.ObjectManager").call(
            conn,
            "GetManagedObjects",
            &(),
        )
    }
}

/// A property of a `GetAll` reply, when it is there with the right type.
pub fn get<T: TryFrom<OwnedValue>>(props: &Props, key: &str) -> Option<T> {
    props.get(key).and_then(|v| T::try_from(v.clone()).ok())
}

/// Calls `changed` (from this thread) for every signal `senders` send and
/// whenever one of them comes or goes; returns when `changed` returns
/// false or the bus goes away.
pub fn watch(conn: &Connection, senders: &[&str], mut changed: impl FnMut() -> bool) {
    let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(conn) else {
        return;
    };
    for sender in senders {
        let rules = [
            zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(*sender)
                .map(|b| b.build()),
            zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .interface("org.freedesktop.DBus")
                .and_then(|b| b.member("NameOwnerChanged"))
                .and_then(|b| b.arg(0, *sender))
                .map(|b| b.build()),
        ];
        for rule in rules.into_iter().flatten() {
            let _ = dbus.add_match_rule(rule);
        }
    }
    for message in zbus::blocking::MessageIterator::from(conn) {
        let Ok(message) = message else { continue };
        if message.message_type() == zbus::message::Type::Signal
            && message
                .header()
                .member()
                .is_some_and(|m| m.as_str() != "NameAcquired")
            && !changed()
        {
            return;
        }
    }
}

#[cfg(test)]
pub mod testbus {
    //! A private `dbus-daemon` for tests: fakes of system and session
    //! services run on it, never on the user's buses.

    use zbus::blocking::Connection;

    pub use helix_app_common::test_bus::TestBus as Bus;

    /// A new private bus that can start no services (so nothing real,
    /// such as the permission store, runs on it), or `None` (with a
    /// note) without `dbus-daemon`.
    pub fn private_bus() -> Option<Bus> {
        Bus::start()
    }

    pub fn connect(bus: &Bus) -> Connection {
        zbus::blocking::connection::Builder::address(bus.address())
            .unwrap()
            .build()
            .unwrap()
    }

    /// Waits up to two seconds for `cond`.
    pub fn eventually(mut cond: impl FnMut() -> bool) -> bool {
        for _ in 0..200 {
            if cond() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        cond()
    }

    /// Runs [`super::watch`] on a thread; the returned counter goes up
    /// with every change it reports.
    pub fn count_changes(
        bus: &Bus,
        senders: &'static [&'static str],
    ) -> std::sync::Arc<std::sync::atomic::AtomicUsize> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let count = std::sync::Arc::new(AtomicUsize::new(0));
        let conn = connect(bus);
        let c = count.clone();
        std::thread::spawn(move || {
            super::watch(&conn, senders, || {
                c.fetch_add(1, Ordering::SeqCst);
                true
            })
        });
        // Let the match rules reach the bus.
        std::thread::sleep(std::time::Duration::from_millis(100));
        count
    }
}
