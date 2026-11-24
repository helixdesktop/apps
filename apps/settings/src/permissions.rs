//! App permissions in the portals' permission store
//! (`org.freedesktop.impl.portal.PermissionStore` on the session bus), as
//! the portals read them. Blocking zbus; worker threads only.

use std::collections::HashMap;

use helix_app_common::i18n::{n_, tr};
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

use crate::bus::{self, Obj, Result};

pub const NAME: &str = "org.freedesktop.impl.portal.PermissionStore";
const STORE: Obj<'static> = Obj::new(NAME, "/org/freedesktop/impl/portal/PermissionStore", NAME);

/// One kind of permission: where the portals keep it, and how "allowed"
/// and "denied" are written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Kind {
    pub table: &'static str,
    pub id: &'static str,
    pub label: &'static str,
    pub allow: &'static str,
    pub deny: &'static str,
}

pub const KINDS: [Kind; 7] = [
    Kind {
        table: "background",
        id: "background",
        label: n_("Run in Background"),
        allow: "yes",
        deny: "no",
    },
    Kind {
        table: "notifications",
        id: "notification",
        label: n_("Notifications"),
        allow: "yes",
        deny: "no",
    },
    Kind {
        table: "location",
        id: "location",
        label: n_("Location"),
        allow: "EXACT",
        deny: "NONE",
    },
    Kind {
        table: "devices",
        id: "camera",
        label: n_("Camera"),
        allow: "yes",
        deny: "no",
    },
    Kind {
        table: "devices",
        id: "microphone",
        label: n_("Microphone"),
        allow: "yes",
        deny: "no",
    },
    Kind {
        table: "devices",
        id: "speakers",
        label: n_("Speakers"),
        allow: "yes",
        deny: "no",
    },
    Kind {
        table: "screenshot",
        id: "screenshot",
        label: n_("Screenshots"),
        allow: "yes",
        deny: "no",
    },
];

impl Kind {
    pub fn title(&self) -> &'static str {
        tr(self.label)
    }
}

/// What the store says of one app: (kind, allowed) for every kind it has
/// an entry for.
pub type AppPermissions = Vec<(Kind, bool)>;

fn lookup(conn: &Connection, kind: &Kind) -> Result<HashMap<String, Vec<String>>> {
    match STORE.call::<_, (HashMap<String, Vec<String>>, OwnedValue)>(
        conn,
        "Lookup",
        &(kind.table, kind.id),
    ) {
        Ok((perms, _)) => Ok(perms),
        // No such table or id yet: nobody was asked.
        Err(bus::Error::Other(_)) => Ok(HashMap::new()),
        Err(e) => Err(e),
    }
}

/// The permissions recorded for `app` (its id without `.desktop`).
pub fn read(conn: &Connection, app: &str) -> Result<AppPermissions> {
    let mut out = Vec::new();
    for kind in KINDS {
        if let Some(values) = lookup(conn, &kind)?.get(app) {
            let allowed = values.first().is_some_and(|v| v == kind.allow);
            out.push((kind, allowed));
        }
    }
    Ok(out)
}

/// Allows or denies one permission (a location entry keeps its time
/// stamp).
pub fn set(conn: &Connection, app: &str, kind: &Kind, allowed: bool) -> Result<()> {
    let mut values: Vec<String> = lookup(conn, kind)?.remove(app).unwrap_or_default();
    let value = if allowed { kind.allow } else { kind.deny };
    match values.first_mut() {
        Some(v) => *v = value.into(),
        None => values.push(value.into()),
    }
    STORE.run(
        conn,
        "SetPermission",
        &(kind.table, true, kind.id, app, values),
    )
}

/// Forgets every permission of `app` (the portals ask again).
pub fn reset(conn: &Connection, app: &str) -> Result<()> {
    for kind in KINDS {
        if lookup(conn, &kind)?.contains_key(app) {
            STORE.run(conn, "DeletePermission", &(kind.table, kind.id, app))?;
        }
    }
    Ok(())
}

/// Calls `changed` whenever the store changes, or comes or goes.
pub fn watch(conn: &Connection, changed: impl FnMut() -> bool) {
    bus::watch(conn, &[NAME], changed)
}

/// The id the portals use for a desktop file id (`org.x.App.desktop`).
pub fn app_id(desktop_id: &str) -> &str {
    desktop_id.strip_suffix(".desktop").unwrap_or(desktop_id)
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::bus::testbus;
    use std::sync::{Arc, Mutex};
    use zbus::object_server::SignalEmitter;

    type Tables = HashMap<(String, String), HashMap<String, Vec<String>>>;

    pub struct FakeStore(pub Arc<Mutex<Tables>>);

    #[zbus::interface(name = "org.freedesktop.impl.portal.PermissionStore")]
    impl FakeStore {
        fn lookup(
            &self,
            table: String,
            id: String,
        ) -> zbus::fdo::Result<(HashMap<String, Vec<String>>, OwnedValue)> {
            let t = self.0.lock().unwrap();
            match t.get(&(table, id)) {
                Some(p) => Ok((p.clone(), OwnedValue::from(0u8))),
                None => Err(zbus::fdo::Error::Failed("No entry".into())),
            }
        }
        async fn set_permission(
            &self,
            table: String,
            _create: bool,
            id: String,
            app: String,
            permissions: Vec<String>,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            self.0
                .lock()
                .unwrap()
                .entry((table.clone(), id.clone()))
                .or_default()
                .insert(app, permissions);
            Self::changed(&e, &table, &id).await?;
            Ok(())
        }
        async fn delete_permission(
            &self,
            table: String,
            id: String,
            app: String,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            if let Some(t) = self.0.lock().unwrap().get_mut(&(table.clone(), id.clone())) {
                t.remove(&app);
            }
            Self::changed(&e, &table, &id).await?;
            Ok(())
        }
        #[zbus(signal)]
        async fn changed(e: &SignalEmitter<'_>, table: &str, id: &str) -> zbus::Result<()>;
    }

    pub fn serve(conn: &Connection) {
        let mut t = Tables::new();
        let entry = |pairs: &[(&str, &[&str])]| {
            pairs
                .iter()
                .map(|(a, v)| (a.to_string(), v.iter().map(|s| s.to_string()).collect()))
                .collect::<HashMap<_, _>>()
        };
        t.insert(
            ("background".into(), "background".into()),
            entry(&[
                ("org.gnome.Maps", &["no"]),
                ("org.helixdesktop.Clocks", &["yes"]),
            ]),
        );
        t.insert(
            ("location".into(), "location".into()),
            entry(&[("org.gnome.Maps", &["EXACT", "1700000000"])]),
        );
        t.insert(
            ("devices".into(), "camera".into()),
            entry(&[("org.gnome.Maps", &["yes"])]),
        );
        conn.object_server()
            .at(
                "/org/freedesktop/impl/portal/PermissionStore",
                FakeStore(Arc::new(Mutex::new(t))),
            )
            .unwrap();
        conn.request_name(NAME).unwrap();
    }

    #[test]
    fn against_a_fake_store() {
        let Some(bus) = testbus::private_bus() else {
            return;
        };
        let app = testbus::connect(&bus);
        assert_eq!(read(&app, "org.gnome.Maps"), Err(bus::Error::Missing));
        let service = testbus::service(&bus);
        serve(&service);
        let changes = testbus::count_changes(&bus, &[NAME]);

        let p = read(&app, "org.gnome.Maps").unwrap();
        let shown: Vec<(&str, bool)> = p.iter().map(|(k, a)| (k.id, *a)).collect();
        assert_eq!(
            shown,
            [("background", false), ("location", true), ("camera", true)]
        );
        assert!(read(&app, "org.other.App").unwrap().is_empty());

        set(&app, "org.gnome.Maps", &KINDS[2], false).unwrap();
        set(&app, "org.gnome.Maps", &KINDS[0], true).unwrap();
        // A first answer creates the entry.
        set(&app, "org.gnome.Maps", &KINDS[6], true).unwrap();
        assert!(testbus::eventually(|| changes
            .load(std::sync::atomic::Ordering::SeqCst)
            >= 3));
        let p = read(&app, "org.gnome.Maps").unwrap();
        let shown: Vec<(&str, bool)> = p.iter().map(|(k, a)| (k.id, *a)).collect();
        assert_eq!(
            shown,
            [
                ("background", true),
                ("location", false),
                ("camera", true),
                ("screenshot", true)
            ]
        );
        // The location time stamp stays.
        let (loc, _): (HashMap<String, Vec<String>>, OwnedValue) = STORE
            .call(&app, "Lookup", &("location", "location"))
            .unwrap();
        assert_eq!(loc["org.gnome.Maps"], ["NONE", "1700000000"]);

        reset(&app, "org.gnome.Maps").unwrap();
        assert!(read(&app, "org.gnome.Maps").unwrap().is_empty());
        assert_eq!(
            read(&app, "org.helixdesktop.Clocks").unwrap().len(),
            1,
            "others kept"
        );
        assert_eq!(app_id("org.gnome.Maps.desktop"), "org.gnome.Maps");
    }
}
