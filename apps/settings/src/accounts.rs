//! User accounts through AccountsService (`org.freedesktop.Accounts` on
//! the system bus); administration is authorized by polkit. Blocking
//! zbus; worker threads only.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::io::Read;

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, Value};

use crate::bus::{self, get, Obj, Result};

pub const ACCOUNTS: &str = "org.freedesktop.Accounts";
const ROOT: Obj<'static> = Obj::new(ACCOUNTS, "/org/freedesktop/Accounts", ACCOUNTS);
const USER: &str = "org.freedesktop.Accounts.User";
const POLKIT: Obj<'static> = Obj::new(
    "org.freedesktop.PolicyKit1",
    "/org/freedesktop/PolicyKit1/Authority",
    "org.freedesktop.PolicyKit1.Authority",
);
/// What administering other users takes.
pub const ADMIN_ACTION: &str = "org.freedesktop.accounts.user-administration";

pub const STANDARD: i32 = 0;
pub const ADMINISTRATOR: i32 = 1;
/// `PasswordMode`: set at next login.
pub const SET_AT_LOGIN: i32 = 1;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct User {
    pub path: String,
    pub uid: u64,
    pub name: String,
    pub real_name: String,
    pub account_type: i32,
    pub icon: String,
    pub automatic_login: bool,
    pub language: String,
    pub system: bool,
    pub local: bool,
}

impl User {
    /// What to call them: the full name, else the user name.
    pub fn title(&self) -> &str {
        if self.real_name.is_empty() {
            &self.name
        } else {
            &self.real_name
        }
    }

    pub fn admin(&self) -> bool {
        self.account_type == ADMINISTRATOR
    }
}

fn user(conn: &Connection, path: &str) -> Result<User> {
    let p = Obj::new(ACCOUNTS, path, USER).props(conn)?;
    Ok(User {
        path: path.to_string(),
        uid: get(&p, "Uid").unwrap_or(0),
        name: get(&p, "UserName").unwrap_or_default(),
        real_name: get(&p, "RealName").unwrap_or_default(),
        account_type: get(&p, "AccountType").unwrap_or(STANDARD),
        icon: get(&p, "IconFile").unwrap_or_default(),
        automatic_login: get(&p, "AutomaticLogin").unwrap_or(false),
        language: get(&p, "Language").unwrap_or_default(),
        system: get(&p, "SystemAccount").unwrap_or(false),
        local: get(&p, "LocalAccount").unwrap_or(true),
    })
}

/// The people who can log in: the current user first, then the others
/// by name (system accounts left out).
pub struct Users {
    pub me: User,
    pub others: Vec<User>,
}

/// This process's user id.
pub fn my_uid() -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata("/proc/self")
        .map(|m| m.uid() as u64)
        .unwrap_or(u32::MAX as u64)
}

pub fn read(conn: &Connection, uid: u64) -> Result<Users> {
    let me_path: OwnedObjectPath = ROOT.call(conn, "FindUserById", &(uid as i64))?;
    let me = user(conn, me_path.as_str())?;
    let paths: Vec<OwnedObjectPath> = ROOT.call(conn, "ListCachedUsers", &())?;
    let mut others: Vec<User> = paths
        .iter()
        .filter(|p| p.as_str() != me.path)
        .filter_map(|p| user(conn, p.as_str()).ok())
        .filter(|u| !u.system)
        .collect();
    others.sort_by_key(|u| u.title().to_lowercase());
    Ok(Users { me, others })
}

fn u(path: &str) -> Obj<'_> {
    Obj::new(ACCOUNTS, path, USER)
}

pub fn set_real_name(conn: &Connection, path: &str, name: &str) -> Result<()> {
    u(path).run_interactive(conn, "SetRealName", &(name,))
}

pub fn set_icon(conn: &Connection, path: &str, file: &str) -> Result<()> {
    u(path).run_interactive(conn, "SetIconFile", &(file,))
}

pub fn set_account_type(conn: &Connection, path: &str, kind: i32) -> Result<()> {
    u(path).run_interactive(conn, "SetAccountType", &(kind,))
}

pub fn set_automatic_login(conn: &Connection, path: &str, on: bool) -> Result<()> {
    u(path).run_interactive(conn, "SetAutomaticLogin", &(on,))
}

pub fn set_language(conn: &Connection, path: &str, language: &str) -> Result<()> {
    u(path).run_interactive(conn, "SetLanguage", &(language,))
}

/// Sets a new password (hashed here; the plain text never leaves this
/// process).
pub fn set_password(conn: &Connection, path: &str, password: &str) -> Result<()> {
    let hash = hash_password(password)
        .ok_or_else(|| bus::Error::Other("cannot hash the password".into()))?;
    u(path).run_interactive(conn, "SetPassword", &(hash.as_str(), ""))
}

/// The user chooses a password the next time they log in.
pub fn set_password_at_login(conn: &Connection, path: &str) -> Result<()> {
    u(path).run_interactive(conn, "SetPasswordMode", &(SET_AT_LOGIN,))
}

pub fn create(conn: &Connection, name: &str, real_name: &str, kind: i32) -> Result<String> {
    ROOT.call_interactive::<_, OwnedObjectPath>(conn, "CreateUser", &(name, real_name, kind))
        .map(|p| p.to_string())
}

pub fn delete(conn: &Connection, uid: u64, remove_files: bool) -> Result<()> {
    ROOT.run_interactive(conn, "DeleteUser", &(uid as i64, remove_files))
}

/// Asks polkit (letting it ask the user) whether this connection may
/// administer users.
pub fn unlock(conn: &Connection) -> Result<bool> {
    let name = conn
        .unique_name()
        .map(|n| n.to_string())
        .ok_or_else(|| bus::Error::Other("no bus name".into()))?;
    let subject = (
        "system-bus-name",
        HashMap::from([("name".to_string(), Value::from(name))]),
    );
    let (authorized, _challenge, _details): (bool, bool, HashMap<String, String>) = POLKIT
        .call_interactive(
            conn,
            "CheckAuthorization",
            &(
                subject,
                ADMIN_ACTION,
                HashMap::<String, String>::new(),
                1u32,
                "",
            ),
        )?;
    Ok(authorized)
}

/// Calls `changed` whenever accounts change, or the service comes or goes.
pub fn watch(conn: &Connection, changed: impl FnMut() -> bool) {
    bus::watch(conn, &[ACCOUNTS], changed)
}

/// A user name from a full name: lower case ASCII letters and digits of
/// its first word, or of all of it when that is short.
pub fn user_name_for(real_name: &str) -> String {
    let clean = |s: &str| -> String {
        s.chars()
            .filter_map(|c| {
                let c = c.to_ascii_lowercase();
                (c.is_ascii_lowercase() || c.is_ascii_digit()).then_some(c)
            })
            .collect()
    };
    let first = real_name
        .split_whitespace()
        .next()
        .map(clean)
        .unwrap_or_default();
    let name = if first.len() >= 3 {
        first
    } else {
        clean(real_name)
    };
    let name: String = name
        .trim_start_matches(|c: char| c.is_ascii_digit())
        .chars()
        .take(32)
        .collect();
    name
}

/// Whether `name` is a usable user name.
pub fn user_name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    let first_ok = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_');
    first_ok
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// Installed locales for the language choice (`locale -a`), UTF-8 ones,
/// as `xx_YY.UTF-8`.
pub fn locales() -> Vec<String> {
    let out = std::process::Command::new("locale")
        .arg("-a")
        .env("LC_ALL", "C")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let mut l: Vec<String> = out
        .lines()
        .filter_map(|l| {
            let (base, enc) = l.split_once('.')?;
            matches!(enc.to_ascii_lowercase().as_str(), "utf8" | "utf-8")
                .then(|| format!("{base}.UTF-8"))
        })
        .filter(|l| l.contains('_'))
        .collect();
    l.sort();
    l.dedup();
    l
}

// --- Password hashing with the system's crypt(3) (libcrypt) ---

type CryptFn = unsafe extern "C" fn(*const c_char, *const c_char) -> *mut c_char;

extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const RTLD_NOW: c_int = 2;

fn crypt_fn() -> Option<CryptFn> {
    static CRYPT: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let f = CRYPT.get_or_init(|| {
        for lib in ["libcrypt.so.1", "libcrypt.so.2", "libcrypt.so"] {
            let name = CString::new(lib).ok()?;
            // SAFETY: dlopen with a valid C string; the handle is kept for
            // the life of the process.
            let h = unsafe { dlopen(name.as_ptr(), RTLD_NOW) };
            if h.is_null() {
                continue;
            }
            let sym = CString::new("crypt").ok()?;
            // SAFETY: looking up a symbol in a library we opened.
            let f = unsafe { dlsym(h, sym.as_ptr()) };
            if !f.is_null() {
                return Some(f as usize);
            }
        }
        None
    });
    // SAFETY: the symbol is libcrypt's `crypt`, which has this signature.
    f.map(|p| unsafe { std::mem::transmute::<usize, CryptFn>(p) })
}

/// `crypt(3)` of `key` with `setting` (a salt such as `$6$…`).
pub fn crypt(key: &str, setting: &str) -> Option<String> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let f = crypt_fn()?;
    let (k, s) = (CString::new(key).ok()?, CString::new(setting).ok()?);
    let _guard = LOCK.lock().ok()?;
    // SAFETY: valid C strings; crypt's static result is copied while the
    // lock is held.
    let r = unsafe { f(k.as_ptr(), s.as_ptr()) };
    if r.is_null() {
        return None;
    }
    // SAFETY: crypt returns a NUL-terminated string.
    let out = unsafe { CStr::from_ptr(r) }.to_string_lossy().into_owned();
    // Failures are reported as "*" (or "*0", "*1").
    (!out.starts_with('*')).then_some(out)
}

/// A SHA-512 crypt hash of `password` with a fresh random salt.
pub fn hash_password(password: &str) -> Option<String> {
    const CHARS: &[u8] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .ok()?
        .read_exact(&mut bytes)
        .ok()?;
    let salt: String = bytes
        .iter()
        .map(|b| CHARS[(*b as usize) % CHARS.len()] as char)
        .collect();
    crypt(password, &format!("$6${salt}$"))
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::bus::testbus;
    use std::sync::{Arc, Mutex};
    use zbus::object_server::SignalEmitter;
    use zbus::zvariant::OwnedValue;

    pub type Log = Arc<Mutex<Vec<String>>>;

    pub struct FakeUser {
        pub uid: u64,
        pub name: String,
        pub real_name: String,
        pub account_type: i32,
        pub automatic_login: bool,
        pub system: bool,
        pub log: Log,
    }

    #[zbus::interface(name = "org.freedesktop.Accounts.User")]
    impl FakeUser {
        async fn set_real_name(
            &mut self,
            name: String,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("SetRealName {} {name}", self.name));
            self.real_name = name;
            Self::changed(&e).await?;
            Ok(())
        }
        fn set_icon_file(&self, file: String) {
            self.log
                .lock()
                .unwrap()
                .push(format!("SetIconFile {} {file}", self.name));
        }
        async fn set_account_type(
            &mut self,
            kind: i32,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("SetAccountType {} {kind}", self.name));
            self.account_type = kind;
            Self::changed(&e).await?;
            Ok(())
        }
        fn set_automatic_login(&mut self, on: bool) {
            self.log
                .lock()
                .unwrap()
                .push(format!("SetAutomaticLogin {} {on}", self.name));
            self.automatic_login = on;
        }
        fn set_language(&self, language: String) {
            self.log
                .lock()
                .unwrap()
                .push(format!("SetLanguage {} {language}", self.name));
        }
        fn set_password(&self, hash: String, _hint: String) -> zbus::fdo::Result<()> {
            // Only a hash may arrive, never the password itself.
            let ok = hash.starts_with("$6$")
                && crypt("s3cret pass", &hash).as_deref() == Some(hash.as_str());
            self.log
                .lock()
                .unwrap()
                .push(format!("SetPassword {} hash-ok={ok}", self.name));
            Ok(())
        }
        fn set_password_mode(&self, mode: i32) {
            self.log
                .lock()
                .unwrap()
                .push(format!("SetPasswordMode {} {mode}", self.name));
        }
        #[zbus(signal)]
        async fn changed(e: &SignalEmitter<'_>) -> zbus::Result<()>;
        #[zbus(property)]
        fn uid(&self) -> u64 {
            self.uid
        }
        #[zbus(property)]
        fn user_name(&self) -> String {
            self.name.clone()
        }
        #[zbus(property)]
        fn real_name(&self) -> String {
            self.real_name.clone()
        }
        #[zbus(property)]
        fn account_type(&self) -> i32 {
            self.account_type
        }
        #[zbus(property)]
        fn icon_file(&self) -> String {
            String::new()
        }
        #[zbus(property)]
        fn automatic_login(&self) -> bool {
            self.automatic_login
        }
        #[zbus(property)]
        fn language(&self) -> String {
            "en_GB.UTF-8".into()
        }
        #[zbus(property)]
        fn system_account(&self) -> bool {
            self.system
        }
        #[zbus(property)]
        fn local_account(&self) -> bool {
            true
        }
    }

    pub struct FakeAccounts {
        pub users: Vec<(u64, String)>,
        pub log: Log,
    }

    #[zbus::interface(name = "org.freedesktop.Accounts")]
    impl FakeAccounts {
        fn list_cached_users(&self) -> Vec<OwnedObjectPath> {
            self.users
                .iter()
                .map(|(_, p)| OwnedObjectPath::try_from(p.as_str()).unwrap())
                .collect()
        }
        fn find_user_by_id(&self, id: i64) -> zbus::fdo::Result<OwnedObjectPath> {
            self.users
                .iter()
                .find(|(u, _)| *u as i64 == id)
                .map(|(_, p)| OwnedObjectPath::try_from(p.as_str()).unwrap())
                .ok_or_else(|| zbus::fdo::Error::Failed("no such user".into()))
        }
        async fn create_user(
            &mut self,
            name: String,
            real_name: String,
            kind: i32,
            #[zbus(object_server)] server: &zbus::ObjectServer,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<OwnedObjectPath> {
            self.log
                .lock()
                .unwrap()
                .push(format!("CreateUser {name} {real_name} {kind}"));
            let uid = 1000 + self.users.len() as u64 + 10;
            let path = format!("/org/freedesktop/Accounts/User{uid}");
            server
                .at(
                    path.as_str(),
                    FakeUser {
                        uid,
                        name,
                        real_name,
                        account_type: kind,
                        automatic_login: false,
                        system: false,
                        log: self.log.clone(),
                    },
                )
                .await?;
            self.users.push((uid, path.clone()));
            let p = OwnedObjectPath::try_from(path.as_str()).unwrap();
            Self::user_added(&e, p.clone()).await?;
            Ok(p)
        }
        async fn delete_user(
            &mut self,
            id: i64,
            remove_files: bool,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> zbus::fdo::Result<()> {
            self.log
                .lock()
                .unwrap()
                .push(format!("DeleteUser {id} {remove_files}"));
            let Some(i) = self.users.iter().position(|(u, _)| *u as i64 == id) else {
                return Err(zbus::fdo::Error::Failed("no such user".into()));
            };
            let (_, p) = self.users.remove(i);
            Self::user_deleted(&e, OwnedObjectPath::try_from(p.as_str()).unwrap()).await?;
            Ok(())
        }
        #[zbus(signal)]
        async fn user_added(e: &SignalEmitter<'_>, user: OwnedObjectPath) -> zbus::Result<()>;
        #[zbus(signal)]
        async fn user_deleted(e: &SignalEmitter<'_>, user: OwnedObjectPath) -> zbus::Result<()>;
    }

    pub struct FakePolkit {
        pub allow: bool,
        pub log: Log,
    }

    #[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
    impl FakePolkit {
        fn check_authorization(
            &self,
            subject: (String, HashMap<String, OwnedValue>),
            action: String,
            _details: HashMap<String, String>,
            flags: u32,
            _cancel: String,
        ) -> (bool, bool, HashMap<String, String>) {
            self.log
                .lock()
                .unwrap()
                .push(format!("CheckAuthorization {} {action} {flags}", subject.0));
            (self.allow, false, HashMap::new())
        }
    }

    /// A fake AccountsService (the user `uid` "Ada Lovelace", an
    /// administrator; "Charles Babbage"; a system account) and polkit.
    pub fn serve(conn: &Connection, uid: u64, allow: bool, log: &Log) {
        let s = conn.object_server();
        let mut users = Vec::new();
        for (id, name, real, kind, system) in [
            (uid, "ada", "Ada Lovelace", ADMINISTRATOR, false),
            (uid + 1, "charles", "Charles Babbage", STANDARD, false),
            (120, "colord", "", STANDARD, true),
        ] {
            let path = format!("/org/freedesktop/Accounts/User{id}");
            s.at(
                path.as_str(),
                FakeUser {
                    uid: id,
                    name: name.into(),
                    real_name: real.into(),
                    account_type: kind,
                    automatic_login: false,
                    system,
                    log: log.clone(),
                },
            )
            .unwrap();
            users.push((id, path));
        }
        s.at(
            "/org/freedesktop/Accounts",
            FakeAccounts {
                users,
                log: log.clone(),
            },
        )
        .unwrap();
        s.at(
            "/org/freedesktop/PolicyKit1/Authority",
            FakePolkit {
                allow,
                log: log.clone(),
            },
        )
        .unwrap();
        conn.request_name(ACCOUNTS).unwrap();
        conn.request_name("org.freedesktop.PolicyKit1").unwrap();
    }

    #[test]
    fn against_a_fake_accounts_service() {
        let Some(bus) = testbus::private_bus() else {
            return;
        };
        let app = testbus::connect(&bus);
        assert!(matches!(read(&app, 4242), Err(bus::Error::Missing)));
        let log: Log = Arc::default();
        let service = testbus::service(&bus);
        serve(&service, 4242, true, &log);
        let changes = testbus::count_changes(&bus, &[ACCOUNTS]);

        let users = read(&app, 4242).unwrap();
        assert_eq!(users.me.title(), "Ada Lovelace");
        assert!(users.me.admin());
        let others: Vec<&str> = users.others.iter().map(|u| u.title()).collect();
        assert_eq!(others, ["Charles Babbage"], "system accounts left out");

        assert!(unlock(&app).unwrap());
        let me = users.me.path.clone();
        set_real_name(&app, &me, "Augusta Ada King").unwrap();
        assert!(testbus::eventually(|| changes
            .load(std::sync::atomic::Ordering::SeqCst)
            > 0));
        set_icon(&app, &me, "/usr/share/pixmaps/faces/cat.png").unwrap();
        set_automatic_login(&app, &me, true).unwrap();
        set_language(&app, &me, "fr_FR.UTF-8").unwrap();
        set_password(&app, &me, "s3cret pass").unwrap();
        let charles = users.others[0].clone();
        set_account_type(&app, &charles.path, ADMINISTRATOR).unwrap();
        let new = create(&app, "grace", "Grace Hopper", STANDARD).unwrap();
        set_password_at_login(&app, &new).unwrap();
        delete(&app, charles.uid, true).unwrap();

        let users = read(&app, 4242).unwrap();
        assert_eq!(users.me.real_name, "Augusta Ada King");
        let others: Vec<&str> = users.others.iter().map(|u| u.title()).collect();
        assert_eq!(others, ["Grace Hopper"]);
        assert_eq!(
            *log.lock().unwrap(),
            [
                "CheckAuthorization system-bus-name org.freedesktop.accounts.user-administration 1",
                "SetRealName ada Augusta Ada King",
                "SetIconFile ada /usr/share/pixmaps/faces/cat.png",
                "SetAutomaticLogin ada true",
                "SetLanguage ada fr_FR.UTF-8",
                "SetPassword ada hash-ok=true",
                "SetAccountType charles 1",
                "CreateUser grace Grace Hopper 0",
                "SetPasswordMode grace 1",
                "DeleteUser 4243 true",
            ]
        );
    }

    #[test]
    fn names_and_hashes() {
        assert_eq!(user_name_for("Grace Hopper"), "grace");
        assert_eq!(user_name_for("Al B"), "alb");
        assert_eq!(user_name_for("Zoë Ångström"), "zongstrm");
        assert!(user_name_ok("grace") && user_name_ok("_x-1"));
        assert!(
            !user_name_ok("Grace")
                && !user_name_ok("1abc")
                && !user_name_ok("")
                && !user_name_ok(&"a".repeat(33))
        );
        let h = hash_password("hunter2").unwrap();
        assert!(h.starts_with("$6$"));
        assert_eq!(crypt("hunter2", &h).as_deref(), Some(h.as_str()));
        assert_ne!(crypt("hunter3", &h).as_deref(), Some(h.as_str()));
        assert_ne!(hash_password("hunter2").unwrap(), h, "fresh salt");
    }
}
