//! The Login Screen panel's model: helix-greeter's configuration
//! (`/etc/helix/greeter.toml`, helixdesktop/spec `components/greeter.md`,
//! "Configuration"), read directly (it is world-readable) and changed
//! through helix-desktop's `helix-login-config` run with `pkexec`
//! ("Changing the configuration"); and whether the panel is shown at
//! all: only when helix-greeter is installed and greetd is the display
//! manager ("Installing and switching"). Blocking; worker threads only
//! for the bus.

use std::path::{Path, PathBuf};

use zbus::blocking::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue};

use crate::bus::{self, Obj};
use crate::login_screen::{quoted, system_path, Helper, PKEXEC};
use crate::values;

/// The greeter's configuration file.
pub const CONFIG: &str = "/etc/helix/greeter.toml";
/// helix-greeter's program and greetd's drop-in, both from the package.
pub const GREETER: &str = "/usr/bin/helix-greeter";
pub const DROP_IN: &str = "/usr/lib/systemd/system/greetd.service.d/helix-greeter.conf";
/// The privileged writer (in the `helix-greeter` package).
pub const HELPER: &str = "/usr/libexec/helix/helix-login-config";
/// The display manager greetd's unit must be.
pub const GREETD: &str = "greetd.service";
/// The longest banner the helper takes, in Unicode scalar values.
pub const BANNER_MAX: usize = 200;
/// The most input the helper reads.
pub const INPUT_MAX: usize = 16 * 1024;
/// The largest configuration file read; the helper leaves bigger ones
/// alone.
const FILE_MAX: u64 = 1024 * 1024;

/// The `clock-format` values.
pub const CLOCK_FORMATS: [&str; 2] = ["24h", "12h"];

/// What the file says, key by key (`None`: not set, or not of the
/// key's type: the greeter's default).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    pub user_list: Option<bool>,
    pub banner: Option<String>,
    /// As written (an unknown name is blue to the greeter).
    pub accent: Option<String>,
    /// As written (`12h`, `24h`; anything else: automatic).
    pub clock_format: Option<String>,
}

/// How the file was read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileState {
    /// Read (also an empty file).
    Read,
    /// There is none: every default.
    Missing,
    /// Not valid TOML, not a regular file, or too big: every default
    /// shown, and the helper will not change it.
    Broken,
}

impl Config {
    /// From the file's text; `None` when it is not TOML.
    pub fn parse(text: &str) -> Option<Config> {
        let table: toml::Table = text.parse().ok()?;
        let string = |k: &str| table.get(k).and_then(|v| v.as_str()).map(str::to_owned);
        Some(Config {
            user_list: table.get("user-list").and_then(|v| v.as_bool()),
            banner: string("banner"),
            accent: string("accent"),
            clock_format: string("clock-format"),
        })
    }

    /// Reads `path`; a missing or broken file gives the defaults.
    pub fn read(path: &Path) -> (Config, FileState) {
        let meta = match std::fs::symlink_metadata(path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (Config::default(), FileState::Missing)
            }
            Err(_) => return (Config::default(), FileState::Broken),
        };
        if !meta.file_type().is_file() || meta.len() > FILE_MAX {
            return (Config::default(), FileState::Broken);
        }
        match std::fs::read_to_string(path)
            .ok()
            .and_then(|t| Config::parse(&t))
        {
            Some(c) => (c, FileState::Read),
            None => (Config::default(), FileState::Broken),
        }
    }

    /// The system's file (see [`system_path`]).
    pub fn read_system() -> (Config, FileState) {
        Config::read(&system_path(CONFIG))
    }

    /// Whether the login screen lists the users (the default).
    pub fn user_list(&self) -> bool {
        self.user_list.unwrap_or(true)
    }

    /// The accent the login screen draws with: blue when unset or
    /// unknown.
    pub fn accent(&self) -> &'static str {
        values::accent_nick(self.accent.as_deref().unwrap_or("blue"))
    }

    /// `12h` or `24h`, `None` for automatic (unset or unknown).
    pub fn clock_format(&self) -> Option<&'static str> {
        let c = self.clock_format.as_deref()?;
        CLOCK_FORMATS.into_iter().find(|f| *f == c)
    }

    /// The banner shown: `None` for none (unset, or only white space).
    pub fn banner(&self) -> Option<&str> {
        self.banner.as_deref().filter(|b| !b.trim().is_empty())
    }
}

/// Why a banner is not accepted (the helper's rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerError {
    /// More than [`BANNER_MAX`] characters: how many it has.
    TooLong(usize),
    /// A control character (line breaks and tabs too).
    Control,
    /// A bidirectional override or isolate.
    Bidi,
}

/// Whether the helper would take `banner` (empty or white space only is
/// not a banner: it is removed instead, see [`changes`]).
pub fn check_banner(banner: &str) -> Result<(), BannerError> {
    let n = banner.chars().count();
    if n > BANNER_MAX {
        return Err(BannerError::TooLong(n));
    }
    if banner.chars().any(char::is_control) {
        return Err(BannerError::Control);
    }
    if banner
        .chars()
        .any(|c| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}'))
    {
        return Err(BannerError::Bidi);
    }
    Ok(())
}

/// What to send the helper: only the keys being changed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Changes {
    pub user_list: Option<bool>,
    pub banner: Option<String>,
    pub accent: Option<&'static str>,
    pub clock_format: Option<&'static str>,
    /// Keys to remove, so the greeter uses its defaults.
    pub remove: Vec<&'static str>,
}

/// The changes that make the login screen show `wanted` where it shows
/// `current` now. Values are compared as the greeter sees them (an unset
/// user list is a shown one, an unknown accent blue), so turning a
/// switch off and on again changes nothing; a banner or clock format
/// that is no longer wanted is removed.
pub fn changes(current: &Config, wanted: &Config) -> Changes {
    let mut out = Changes::default();
    if wanted.user_list() != current.user_list() {
        out.user_list = Some(wanted.user_list());
    }
    match (wanted.banner(), current.banner.as_deref()) {
        (Some(w), c) if Some(w) != c => out.banner = Some(w.to_owned()),
        (None, Some(_)) => out.remove.push("banner"),
        _ => {}
    }
    if wanted.accent() != current.accent() {
        out.accent = Some(wanted.accent());
    }
    match (wanted.clock_format(), current.clock_format()) {
        (Some(w), c) if Some(w) != c => out.clock_format = Some(w),
        // Automatic: remove what is there, even an unknown value.
        (None, _) if current.clock_format.is_some() => out.remove.push("clock-format"),
        _ => {}
    }
    out
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        *self == Changes::default()
    }

    /// Whether the helper would take it (a banner it would refuse is
    /// refused here first, so the user sees why at once).
    pub fn check(&self) -> Result<(), BannerError> {
        self.banner.as_deref().map_or(Ok(()), check_banner)
    }

    /// The helper's input: a TOML document of the changed keys and
    /// `remove`.
    pub fn to_toml(&self) -> String {
        let mut out = String::new();
        if let Some(v) = self.user_list {
            out.push_str(&format!("user-list = {v}\n"));
        }
        if let Some(v) = &self.banner {
            out.push_str(&format!("banner = {}\n", quoted(v)));
        }
        if let Some(v) = self.accent {
            out.push_str(&format!("accent = {}\n", quoted(v)));
        }
        if let Some(v) = self.clock_format {
            out.push_str(&format!("clock-format = {}\n", quoted(v)));
        }
        if !self.remove.is_empty() {
            let keys: Vec<String> = self.remove.iter().map(|k| quoted(k)).collect();
            out.push_str(&format!("remove = [{}]\n", keys.join(", ")));
        }
        out
    }
}

/// `helix-login-config` through `pkexec`, if both are installed.
pub fn helper() -> Option<Helper> {
    Helper::at(&system_path(PKEXEC), &system_path(HELPER))
}

/// Where the panel's installation check looks.
#[derive(Debug, Clone)]
pub struct Installed {
    pub greeter: PathBuf,
    pub drop_in: PathBuf,
}

impl Default for Installed {
    fn default() -> Self {
        Installed {
            greeter: system_path(GREETER),
            drop_in: system_path(DROP_IN),
        }
    }
}

impl Installed {
    /// Both files of the `helix-greeter` package are there.
    pub fn is_installed(&self) -> bool {
        self.greeter.is_file() && self.drop_in.is_file()
    }
}

/// Whether the Login Screen panel is shown: helix-greeter is installed
/// and `display_manager` (asked only then) says greetd is the display
/// manager.
pub fn panel_shown(
    installed: &Installed,
    display_manager: impl FnOnce() -> Option<String>,
) -> bool {
    installed.is_installed() && display_manager().as_deref() == Some(GREETD)
}

const SYSTEMD: Obj<'static> = Obj::new(
    "org.freedesktop.systemd1",
    "/org/freedesktop/systemd1",
    "org.freedesktop.systemd1.Manager",
);

/// The unit `display-manager.service` is (its `Id`: `greetd.service`,
/// `gdm.service`…), from systemd on `conn`; `None` when it is not loaded
/// (no display manager) or systemd does not answer.
pub fn display_manager(conn: &Connection) -> Option<String> {
    let path: OwnedObjectPath = SYSTEMD
        .call(conn, "GetUnit", &("display-manager.service",))
        .ok()?;
    let unit = Obj::new(SYSTEMD.dest, path.as_str(), "org.freedesktop.systemd1.Unit");
    let id: OwnedValue = Obj::new(
        SYSTEMD.dest,
        path.as_str(),
        "org.freedesktop.DBus.Properties",
    )
    .call(conn, "Get", &(unit.iface, "Id"))
    .ok()?;
    String::try_from(id).ok()
}

/// [`panel_shown`] for this system (systemd on the system bus).
pub fn panel_shown_here() -> bool {
    panel_shown(&Installed::default(), || {
        bus::system().ok().and_then(|c| display_manager(&c))
    })
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::bus::testbus;
    use crate::login_screen::Outcome;
    use std::os::unix::fs::PermissionsExt;

    /// A scratch directory of its own.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "helix-settings-greeter-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_the_configuration() {
        // The spec's example.
        let c = Config::parse(
            "user-list = true          # false: always ask for the username
banner = \"Authorized use only.\"   # text under the clock
accent = \"blue\"           # the accent color (focus rings, the confirming button)
clock-format = \"24h\"      # 12h or 24h; unset: see \"Clock\"
",
        )
        .unwrap();
        assert_eq!(
            c,
            Config {
                user_list: Some(true),
                banner: Some("Authorized use only.".into()),
                accent: Some("blue".into()),
                clock_format: Some("24h".into()),
            }
        );
        // Empty: every default.
        let d = Config::parse("").unwrap();
        assert_eq!(d, Config::default());
        assert!(d.user_list());
        assert_eq!(d.accent(), "blue");
        assert_eq!(d.clock_format(), None);
        assert_eq!(d.banner(), None);
        // Wrong types and unknown values: the defaults, key by key;
        // unknown keys and tables are no matter.
        let w = Config::parse(
            "user-list = \"no\"\nbanner = 3\naccent = \"magenta\"\nclock-format = \"13h\"\nlater = 1\n[table]\nx = 1\n",
        )
        .unwrap();
        assert_eq!(w.user_list, None);
        assert_eq!(w.banner, None);
        assert_eq!(w.accent(), "blue");
        assert_eq!(w.clock_format(), None);
        assert_eq!(
            Config::parse("banner = \"   \"").unwrap().banner(),
            None,
            "white space only is no banner"
        );
        assert_eq!(Config::parse("user-list = [").map(|_| ()), None);

        let dir = scratch("read");
        let file = dir.join("greeter.toml");
        assert_eq!(Config::read(&file), (Config::default(), FileState::Missing));
        std::fs::write(&file, "user-list = false\naccent = \"teal\"\n").unwrap();
        let (c, state) = Config::read(&file);
        assert_eq!(state, FileState::Read);
        assert!(!c.user_list());
        assert_eq!(c.accent(), "teal");
        std::fs::write(&file, "user-list = \n").unwrap();
        assert_eq!(Config::read(&file), (Config::default(), FileState::Broken));
        std::fs::write(&file, [0xff, 0xfe]).unwrap();
        assert_eq!(Config::read(&file), (Config::default(), FileState::Broken));
        std::fs::remove_file(&file).unwrap();
        std::fs::create_dir(&file).unwrap();
        assert_eq!(Config::read(&file).1, FileState::Broken, "not a file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn banners() {
        assert_eq!(check_banner("Authorized use only."), Ok(()));
        assert_eq!(check_banner("Nur für Befugte — 関係者以外"), Ok(()));
        assert_eq!(check_banner(&"a".repeat(BANNER_MAX)), Ok(()));
        assert_eq!(
            check_banner(&"é".repeat(BANNER_MAX)),
            Ok(()),
            "characters, not bytes"
        );
        assert_eq!(
            check_banner(&"a".repeat(BANNER_MAX + 1)),
            Err(BannerError::TooLong(201))
        );
        for bad in ["a\nb", "a\tb", "a\u{7}", "\u{85}", "a\rb"] {
            assert_eq!(check_banner(bad), Err(BannerError::Control), "{bad:?}");
        }
        for bad in ["\u{202A}", "a\u{202E}b", "\u{2066}", "\u{2069}"] {
            assert_eq!(check_banner(bad), Err(BannerError::Bidi), "{bad:?}");
        }
        assert_eq!(
            check_banner("\u{200F}"),
            Ok(()),
            "a mark is not an override"
        );
    }

    #[test]
    fn changes_are_only_what_changed() {
        let current = Config {
            banner: Some("Authorized use only.".into()),
            clock_format: Some("24h".into()),
            ..Config::default()
        };
        assert!(changes(&current, &current).is_empty());
        // The defaults made explicit are no change.
        let same = Config {
            user_list: Some(true),
            accent: Some("blue".into()),
            ..current.clone()
        };
        assert!(changes(&current, &same).is_empty());

        // The spec's example input.
        let wanted = Config {
            user_list: Some(false),
            accent: Some("teal".into()),
            banner: None,
            clock_format: None,
        };
        let c = changes(&current, &wanted);
        assert_eq!(
            c,
            Changes {
                user_list: Some(false),
                accent: Some("teal"),
                remove: vec!["banner", "clock-format"],
                ..Changes::default()
            }
        );
        assert_eq!(
            c.to_toml(),
            "user-list = false\naccent = \"teal\"\nremove = [\"banner\", \"clock-format\"]\n"
        );
        let back: toml::Table = c.to_toml().parse().unwrap();
        assert_eq!(back.len(), 3);

        // A new banner, quoted; an empty or blank one removes it.
        let banner = |b: &str| Config {
            banner: Some(b.into()),
            ..current.clone()
        };
        let c = changes(&current, &banner("Say \"hi\" \\ there"));
        assert_eq!(c.to_toml(), "banner = \"Say \\\"hi\\\" \\\\ there\"\n");
        let parsed = Config::parse(&c.to_toml()).unwrap();
        assert_eq!(parsed.banner.as_deref(), Some("Say \"hi\" \\ there"));
        assert_eq!(changes(&current, &banner("")).remove, vec!["banner"]);
        assert_eq!(changes(&current, &banner("  ")).remove, vec!["banner"]);
        let empty = Config {
            banner: Some(String::new()),
            ..Config::default()
        };
        assert!(
            changes(&Config::default(), &empty).is_empty(),
            "switch on, no text"
        );
        // A blank banner in the file is removed when none is wanted.
        let blank = Config {
            banner: Some(" ".into()),
            ..Config::default()
        };
        assert_eq!(changes(&blank, &Config::default()).remove, vec!["banner"]);

        // Clock: a format, or automatic (removed, also an unknown one).
        let twelve = Config {
            clock_format: Some("12h".into()),
            ..current.clone()
        };
        assert_eq!(changes(&current, &twelve).clock_format, Some("12h"));
        let odd = Config {
            clock_format: Some("13h".into()),
            ..Config::default()
        };
        assert_eq!(
            changes(&odd, &Config::default()).remove,
            vec!["clock-format"]
        );
        assert!(changes(&Config::default(), &Config::default()).is_empty());

        // Too long a banner is caught before the helper.
        let long = changes(&current, &banner(&"a".repeat(201)));
        assert_eq!(long.check(), Err(BannerError::TooLong(201)));
        assert!(long.to_toml().len() < INPUT_MAX);
        assert_eq!(changes(&current, &twelve).check(), Ok(()));
    }

    /// A fake `pkexec` (runs its argument with `sh`, which reads it rather
    /// than executing a file another test thread may still have open for
    /// writing, or exits `pkexec_status`) and
    /// a fake `helix-login-config` (copies its input to `got`, prints
    /// `message`, exits `status`), in `dir`.
    pub fn fake_helper(
        dir: &Path,
        pkexec_status: Option<i32>,
        status: i32,
        message: &str,
    ) -> Helper {
        let script = |file: &str, body: String| {
            let path = dir.join(file);
            std::fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        };
        let pkexec = script(
            "pkexec",
            match pkexec_status {
                Some(s) => format!("exit {s}\n"),
                None => format!(
                    "echo \"$#:$@\" > '{}'\nexec /bin/sh \"$@\"\n",
                    dir.join("args").display()
                ),
            },
        );
        let say = if message.is_empty() {
            String::new()
        } else {
            format!("echo 'helix-login-config: {message}' >&2\n")
        };
        let helper = script(
            "helix-login-config",
            format!(
                "cat > '{}'\n{say}exit {status}\n",
                dir.join("got").display()
            ),
        );
        Helper::at(&pkexec, &helper).unwrap()
    }

    #[test]
    fn sends_the_changes_through_pkexec() {
        let changes = Changes {
            user_list: Some(false),
            remove: vec!["banner"],
            ..Changes::default()
        };
        let dir = scratch("helper-ok");
        let helper = fake_helper(&dir, None, 0, "");
        assert_eq!(helper.run(&changes.to_toml()), Outcome::Done);
        assert_eq!(
            std::fs::read_to_string(dir.join("got")).unwrap(),
            "user-list = false\nremove = [\"banner\"]\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("args")).unwrap().trim(),
            format!("1:{}", dir.join("helix-login-config").display()),
            "pkexec HELPER, no arguments"
        );
        let _ = std::fs::remove_dir_all(&dir);

        for (pk, status, message, outcome) in [
            (
                None,
                1,
                "banner: too long",
                Outcome::Failed("banner: too long".into()),
            ),
            (
                None,
                2,
                "/etc/helix/greeter.toml is not valid TOML; left as it is",
                Outcome::Failed("/etc/helix/greeter.toml is not valid TOML; left as it is".into()),
            ),
            (None, 2, "", Outcome::Failed("exit status 2".into())),
            (Some(126), 0, "", Outcome::Cancelled),
            (Some(127), 0, "", Outcome::NotAuthorized),
        ] {
            let dir = scratch(&format!("helper-{status}-{pk:?}"));
            let helper = fake_helper(&dir, pk, status, message);
            assert_eq!(helper.run("accent = \"red\"\n"), outcome, "{pk:?} {status}");
            let _ = std::fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn shown_only_with_helix_greeter_under_greetd() {
        let dir = scratch("shown");
        let installed = Installed {
            greeter: dir.join("usr/bin/helix-greeter"),
            drop_in: dir.join("greetd.service.d/helix-greeter.conf"),
        };
        let asked = std::cell::Cell::new(0);
        let greetd = || {
            asked.set(asked.get() + 1);
            Some(GREETD.to_owned())
        };
        // Nothing installed: hidden, and systemd is not asked.
        assert!(!panel_shown(&installed, greetd));
        assert_eq!(asked.get(), 0);
        // The program alone, or the drop-in alone: hidden.
        std::fs::create_dir_all(installed.greeter.parent().unwrap()).unwrap();
        std::fs::write(&installed.greeter, "").unwrap();
        assert!(!panel_shown(&installed, greetd));
        std::fs::remove_file(&installed.greeter).unwrap();
        std::fs::create_dir_all(installed.drop_in.parent().unwrap()).unwrap();
        std::fs::write(&installed.drop_in, "").unwrap();
        assert!(!panel_shown(&installed, greetd));
        assert_eq!(asked.get(), 0);
        // Both: shown when greetd is the display manager.
        std::fs::write(&installed.greeter, "").unwrap();
        assert!(panel_shown(&installed, greetd));
        assert_eq!(asked.get(), 1);
        assert!(!panel_shown(&installed, || Some("gdm.service".into())));
        assert!(!panel_shown(&installed, || None), "no display manager");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A fake systemd: `display-manager.service` is the unit `id`, or
    /// not loaded (`None`).
    pub struct FakeSystemd {
        pub id: Option<String>,
    }

    #[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
    impl FakeSystemd {
        fn get_unit(&self, name: String) -> zbus::fdo::Result<OwnedObjectPath> {
            match (&self.id, name.as_str()) {
                (Some(_), "display-manager.service") => Ok(OwnedObjectPath::try_from(
                    "/org/freedesktop/systemd1/unit/display_2dmanager_2eservice",
                )
                .unwrap()),
                _ => Err(zbus::fdo::Error::Failed(format!("Unit {name} not loaded."))),
            }
        }
    }

    pub struct FakeUnit {
        pub id: String,
    }

    #[zbus::interface(name = "org.freedesktop.systemd1.Unit")]
    impl FakeUnit {
        #[zbus(property)]
        fn id(&self) -> String {
            self.id.clone()
        }
    }

    /// Serves [`FakeSystemd`] as `org.freedesktop.systemd1` on `conn`.
    pub fn serve_systemd(conn: &Connection, id: Option<&str>) {
        let server = conn.object_server();
        server
            .at(
                "/org/freedesktop/systemd1",
                FakeSystemd {
                    id: id.map(str::to_owned),
                },
            )
            .unwrap();
        if let Some(id) = id {
            server
                .at(
                    "/org/freedesktop/systemd1/unit/display_2dmanager_2eservice",
                    FakeUnit { id: id.to_owned() },
                )
                .unwrap();
        }
        conn.request_name("org.freedesktop.systemd1").unwrap();
    }

    #[test]
    fn asks_systemd_which_display_manager() {
        for (unit, answer) in [
            (Some(GREETD), Some(GREETD)),
            (Some("gdm.service"), Some("gdm.service")),
            (None, None),
        ] {
            let Some(bus) = testbus::private_bus() else {
                return;
            };
            let app = testbus::connect(&bus);
            assert_eq!(display_manager(&app), None, "no systemd");
            let systemd = testbus::service(&bus);
            serve_systemd(&systemd, unit);
            assert_eq!(display_manager(&app).as_deref(), answer, "{unit:?}");
        }
    }
}
