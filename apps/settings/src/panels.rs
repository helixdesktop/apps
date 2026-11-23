//! Panel registry and sidebar search (pure logic, no GTK).

use helix_app_common::i18n::n_;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelId {
    Wifi,
    Network,
    Bluetooth,
    Display,
    Sound,
    Power,
    Multitasking,
    Appearance,
    Apps,
    Notifications,
    Extensions,
    Mouse,
    Keyboard,
    Printers,
    Accessibility,
    Privacy,
    DateTime,
    Users,
    /// Shown only with the Helix greeter under greetd
    /// ([`crate::greeter::panel_shown`]).
    LoginScreen,
    About,
}

#[derive(Debug)]
pub struct PanelInfo {
    pub id: PanelId,
    /// Stable command-line name.
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub title: &'static str,
    pub icon: &'static str,
    /// Search keywords, each ending in `;` (like a desktop file's
    /// `Keywords`), so translators can add their own.
    pub keywords: &'static str,
    /// Sidebar group; a separator is drawn between groups.
    pub group: u8,
    pub available: bool,
}

pub const PANELS: &[PanelInfo] = &[
    PanelInfo {
        id: PanelId::Wifi,
        name: "wifi",
        aliases: &["wi-fi", "wireless"],
        title: n_("Wi-Fi"),
        icon: "network-wireless-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("network;wireless;wlan;hotspot;internet;airplane;"),
        group: 0,
        available: true,
    },
    PanelInfo {
        id: PanelId::Network,
        name: "network",
        aliases: &["wired"],
        title: n_("Network"),
        icon: "network-wired-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("ethernet;wired;vpn;proxy;ip;dns;internet;"),
        group: 0,
        available: true,
    },
    PanelInfo {
        id: PanelId::Bluetooth,
        name: "bluetooth",
        aliases: &[],
        title: n_("Bluetooth"),
        icon: "bluetooth-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("share;devices;pair;headset;wireless;"),
        group: 0,
        available: true,
    },
    PanelInfo {
        id: PanelId::Display,
        name: "display",
        aliases: &["displays", "screen"],
        title: n_("Displays"),
        icon: "video-display-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("monitor;screen;resolution;refresh;scale;night light;projector;rotation;"),
        group: 1,
        available: true,
    },
    PanelInfo {
        id: PanelId::Sound,
        name: "sound",
        aliases: &["audio"],
        title: n_("Sound"),
        icon: "audio-speakers-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("volume;speaker;microphone;audio;output;input;alert;mute;"),
        group: 1,
        available: true,
    },
    PanelInfo {
        id: PanelId::Power,
        name: "power",
        aliases: &[],
        title: n_("Power"),
        icon: "battery-full-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("battery;suspend;sleep;dim;blank;power button;hibernate;energy;"),
        group: 1,
        available: true,
    },
    PanelInfo {
        id: PanelId::Multitasking,
        name: "multitasking",
        aliases: &["workspaces"],
        title: n_("Multitasking"),
        icon: "view-grid-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("hot corner;workspace;screen edges;tiling;app switching;multi-monitor;"),
        group: 1,
        available: true,
    },
    PanelInfo {
        id: PanelId::Appearance,
        name: "appearance",
        aliases: &["background", "wallpaper"],
        title: n_("Appearance"),
        icon: "preferences-desktop-appearance-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("dark;light;style;theme;icon;icons;accent;color;colour;wallpaper;background;"),
        group: 1,
        available: true,
    },
    PanelInfo {
        id: PanelId::Apps,
        name: "apps",
        aliases: &["applications", "default-apps"],
        title: n_("Apps"),
        icon: "view-app-grid-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("default;applications;permissions;browser;storage;"),
        group: 2,
        available: true,
    },
    PanelInfo {
        id: PanelId::Notifications,
        name: "notifications",
        aliases: &[],
        title: n_("Notifications"),
        icon: "preferences-system-notifications-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("do not disturb;dnd;banner;alerts;lock screen;messages;"),
        group: 2,
        available: true,
    },
    PanelInfo {
        id: PanelId::Extensions,
        name: "extensions",
        aliases: &["shell-extensions", "add-ons"],
        title: n_("Extensions"),
        icon: "application-x-addon-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("add-on;plugin;panel item;indicator;quick settings;dock;sandbox;permissions;"),
        group: 2,
        available: true,
    },
    PanelInfo {
        id: PanelId::Mouse,
        name: "mouse",
        aliases: &["touchpad", "mouse-touchpad"],
        title: n_("Mouse & Touchpad"),
        icon: "input-mouse-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("touchpad;pointer;speed;scroll;natural;tap;click;trackpad;primary button;acceleration;"),
        group: 3,
        available: true,
    },
    PanelInfo {
        id: PanelId::Keyboard,
        name: "keyboard",
        aliases: &["input-sources"],
        title: n_("Keyboard"),
        icon: "input-keyboard-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("layout;input source;language;repeat;delay;typing;xkb;"),
        group: 3,
        available: true,
    },
    PanelInfo {
        id: PanelId::Printers,
        name: "printers",
        aliases: &["printer", "printing"],
        title: n_("Printers"),
        icon: "printer-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("printer;print;cups;queue;jobs;paper;ink;toner;"),
        group: 3,
        available: true,
    },
    PanelInfo {
        id: PanelId::Accessibility,
        name: "accessibility",
        aliases: &["universal-access", "a11y"],
        title: n_("Accessibility"),
        icon: "preferences-desktop-accessibility-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("high contrast;large text;zoom;magnifier;screen reader;cursor size;reduce animation;sticky keys;slow keys;bounce keys;mouse keys;scrollbars;screen keyboard;double-click;locate pointer;"),
        group: 4,
        available: true,
    },
    PanelInfo {
        id: PanelId::Privacy,
        name: "privacy",
        aliases: &["security", "screen-lock", "location"],
        title: n_("Privacy & Security"),
        icon: "preferences-system-privacy-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("screen lock;lock;blank;idle;recent files;file history;trash;temporary;location;camera;microphone;problem reporting;agent;ai;assistant;automation;"),
        group: 4,
        available: true,
    },
    PanelInfo {
        id: PanelId::DateTime,
        name: "datetime",
        aliases: &["date-time", "time"],
        title: n_("Date & Time"),
        icon: "preferences-system-time-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("clock;time zone;timezone;ntp;24-hour;am/pm;week numbers;calendar;"),
        group: 5,
        available: true,
    },
    PanelInfo {
        id: PanelId::Users,
        name: "users",
        aliases: &["user-accounts"],
        title: n_("Users"),
        icon: "system-users-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("account;password;login;avatar;administrator;"),
        group: 5,
        available: true,
    },
    PanelInfo {
        id: PanelId::LoginScreen,
        name: "login-screen",
        aliases: &["greeter", "login"],
        title: n_("Login Screen"),
        icon: "system-lock-screen-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("login;greeter;lock;banner;user list;sign in;display manager;greetd;accent;clock;"),
        group: 5,
        available: true,
    },
    PanelInfo {
        id: PanelId::About,
        name: "about",
        aliases: &["info-overview", "info", "system"],
        title: n_("About"),
        icon: "help-about-symbolic",
        // Translators: search keywords for this Settings panel, separated
        // by semicolons; add words people would search for in your language.
        keywords: n_("device name;hostname;memory;processor;cpu;disk;os;version;kernel;hardware;"),
        group: 5,
        available: true,
    },
];

/// A page inside a panel with its own command-line name.
#[derive(Debug)]
pub struct PageInfo {
    /// Stable command-line name (also the page's navigation tag).
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub panel: PanelId,
    pub title: &'static str,
    /// The page is only there when this schema is installed.
    pub schema: Option<&'static str>,
}

pub const PAGES: &[PageInfo] = &[
    PageInfo {
        // Privacy & Security › Agent Access; the shell's agent indicator
        // opens it. Without the shell's settings it opens Privacy & Security.
        name: "agent-access",
        aliases: &["agents"],
        panel: PanelId::Privacy,
        title: n_("Agent Access"),
        schema: Some("org.helixdesktop.agent-access"),
    },
    PageInfo {
        // Agent Access › View Activity.
        name: "agent-activity",
        aliases: &[],
        panel: PanelId::Privacy,
        title: n_("Agent Activity"),
        schema: Some("org.helixdesktop.agent-access"),
    },
];

/// Resolves a command-line name: a panel ([`lookup`]) or a page in one.
pub fn lookup_target(name: &str) -> Option<(PanelId, Option<&'static str>)> {
    if let Some(id) = lookup(name) {
        return Some((id, None));
    }
    let name = name.trim().to_lowercase();
    PAGES
        .iter()
        .find(|p| p.name == name || p.aliases.contains(&name.as_str()))
        .map(|p| (p.panel, Some(p.name)))
}

pub fn info(id: PanelId) -> &'static PanelInfo {
    PANELS
        .iter()
        .find(|p| p.id == id)
        .expect("every PanelId is registered")
}

/// Resolves a command-line panel name or alias (case-insensitive).
pub fn lookup(name: &str) -> Option<PanelId> {
    let name = name.trim().to_lowercase();
    PANELS
        .iter()
        .find(|p| p.name == name || p.aliases.contains(&name.as_str()))
        .map(|p| p.id)
}

/// Returns the matching panels in display order (best matches first).
/// An empty query returns every panel in registry order.
pub fn search(query: &str) -> Vec<PanelId> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return PANELS.iter().map(|p| p.id).collect();
    }
    let terms: Vec<&str> = q.split_whitespace().collect();
    let mut ranked: Vec<(u8, usize, PanelId)> = PANELS
        .iter()
        .enumerate()
        .filter_map(|(i, p)| rank(p, &q, &terms).map(|r| (r, i, p.id)))
        .collect();
    ranked.sort_by_key(|&(r, i, _)| (r, i));
    ranked.into_iter().map(|(_, _, id)| id).collect()
}

/// The sidebar's rows for `query`: [`search`] order, but panels that
/// work come first and placeholders ("Not yet available") last.
pub fn sidebar_order(query: &str) -> Vec<PanelId> {
    let mut ids = search(query);
    ids.sort_by_key(|id| !info(*id).available);
    ids
}

/// [`sidebar_order`] without the panels in `hidden` (a panel that is
/// not shown on this system is not found by search either).
pub fn sidebar_order_shown(query: &str, hidden: &[PanelId]) -> Vec<PanelId> {
    let mut ids = sidebar_order(query);
    ids.retain(|id| !hidden.contains(id));
    ids
}

/// Lower rank is a better match; `None` means no match.
fn rank(p: &PanelInfo, query: &str, terms: &[&str]) -> Option<u8> {
    let title = helix_app_common::i18n::tr(p.title).to_lowercase();
    // Keywords match in English and in the interface language, as in
    // GNOME Settings, so a search typed either way finds the panel.
    let translated = helix_app_common::i18n::tr(p.keywords).to_lowercase();
    let keywords: Vec<&str> = p
        .keywords
        .split(';')
        .chain(translated.split(';'))
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .collect();
    let all_match = terms
        .iter()
        .all(|t| title.contains(t) || keywords.iter().any(|k| k.contains(t)));
    if !all_match {
        return None;
    }
    if title.starts_with(query) {
        Some(0)
    } else if terms.iter().any(|t| title.contains(t)) {
        Some(1)
    } else {
        Some(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_registered_once() {
        let ids = [
            PanelId::Wifi,
            PanelId::Network,
            PanelId::Bluetooth,
            PanelId::Display,
            PanelId::Sound,
            PanelId::Power,
            PanelId::Multitasking,
            PanelId::Appearance,
            PanelId::Apps,
            PanelId::Notifications,
            PanelId::Extensions,
            PanelId::Mouse,
            PanelId::Keyboard,
            PanelId::Printers,
            PanelId::Accessibility,
            PanelId::Privacy,
            PanelId::DateTime,
            PanelId::Users,
            PanelId::LoginScreen,
            PanelId::About,
        ];
        assert_eq!(ids.len(), PANELS.len());
        for id in ids {
            assert_eq!(PANELS.iter().filter(|p| p.id == id).count(), 1, "{id:?}");
        }
    }

    #[test]
    fn names_and_aliases_unique() {
        let mut all: Vec<&str> = Vec::new();
        for p in PANELS {
            all.push(p.name);
            all.extend(p.aliases);
        }
        for p in PAGES {
            all.push(p.name);
            all.extend(p.aliases);
        }
        let n = all.len();
        all.sort();
        all.dedup();
        assert_eq!(n, all.len());
    }

    #[test]
    fn groups_are_contiguous() {
        let groups: Vec<u8> = PANELS.iter().map(|p| p.group).collect();
        assert!(groups.windows(2).all(|w| w[0] <= w[1]));
    }

    #[test]
    fn availability() {
        assert!(info(PanelId::Appearance).available);
        assert!(info(PanelId::About).available);
        assert!(PANELS.iter().all(|p| p.available), "every panel works");
    }

    #[test]
    fn lookup_names_and_aliases() {
        assert_eq!(lookup("appearance"), Some(PanelId::Appearance));
        assert_eq!(lookup("background"), Some(PanelId::Appearance));
        assert_eq!(lookup("Universal-Access"), Some(PanelId::Accessibility));
        assert_eq!(lookup("info-overview"), Some(PanelId::About));
        assert_eq!(lookup(" datetime "), Some(PanelId::DateTime));
        assert_eq!(lookup("display"), Some(PanelId::Display));
        assert_eq!(lookup("add-ons"), Some(PanelId::Extensions));
        assert_eq!(lookup("printer"), Some(PanelId::Printers));
        assert_eq!(lookup("nonsense"), None);
    }

    #[test]
    fn lookup_pages() {
        assert_eq!(lookup_target("privacy"), Some((PanelId::Privacy, None)));
        assert_eq!(
            lookup_target("agent-access"),
            Some((PanelId::Privacy, Some("agent-access")))
        );
        assert_eq!(
            lookup_target(" Agents "),
            Some((PanelId::Privacy, Some("agent-access")))
        );
        assert_eq!(
            lookup_target("agent-activity"),
            Some((PanelId::Privacy, Some("agent-activity")))
        );
        assert_eq!(lookup("agent-access"), None, "not a panel");
        assert_eq!(lookup_target("nonsense"), None);
    }

    #[test]
    fn empty_query_lists_all() {
        assert_eq!(search("").len(), PANELS.len());
        assert_eq!(search("   ")[0], PanelId::Wifi);
    }

    #[test]
    fn placeholders_listed_last() {
        let order = sidebar_order("");
        assert_eq!(order.len(), PANELS.len());
        if let Some(first_placeholder) = order.iter().position(|id| !info(*id).available) {
            assert!(order[first_placeholder..]
                .iter()
                .all(|id| !info(*id).available));
        }
        // Working panels keep their GNOME order.
        assert_eq!(order[0], PanelId::Wifi);
        assert_eq!(order, search(""));
        // Searching keeps the rule: a working match before a placeholder.
        let r = sidebar_order("wireless");
        assert!(r.iter().all(|id| !info(*id).available) || info(r[0]).available);
    }

    #[test]
    fn title_match() {
        assert_eq!(search("power"), vec![PanelId::Power]);
        assert_eq!(search("Date")[0], PanelId::DateTime);
        assert_eq!(search("KEYB")[0], PanelId::Keyboard);
    }

    #[test]
    fn keyword_match() {
        assert_eq!(search("dark"), vec![PanelId::Appearance]);
        assert_eq!(search("wallpaper"), vec![PanelId::Appearance]);
        assert_eq!(search("hot corner"), vec![PanelId::Multitasking]);
        assert!(search("timezone").contains(&PanelId::DateTime));
        assert!(search("battery").contains(&PanelId::Power));
    }

    #[test]
    fn login_screen_keywords_and_hiding() {
        for q in [
            "login",
            "greeter",
            "lock",
            "banner",
            "user list",
            "Login Screen",
        ] {
            assert!(search(q).contains(&PanelId::LoginScreen), "{q}");
            assert!(
                !sidebar_order_shown(q, &[PanelId::LoginScreen]).contains(&PanelId::LoginScreen),
                "{q}: hidden, not found"
            );
        }
        assert_eq!(search("greeter"), vec![PanelId::LoginScreen]);
        assert_eq!(lookup("login-screen"), Some(PanelId::LoginScreen));
        assert_eq!(lookup("greeter"), Some(PanelId::LoginScreen));
        let all = sidebar_order_shown("", &[PanelId::LoginScreen]);
        assert_eq!(all.len(), PANELS.len() - 1);
        assert_eq!(sidebar_order_shown("", &[]), sidebar_order(""));
    }

    #[test]
    fn all_terms_must_match() {
        assert_eq!(search("screen reader"), vec![PanelId::Accessibility]);
        assert!(search("dark zebra").is_empty());
    }

    #[test]
    fn ranking_title_prefix_first() {
        // "lock" is a keyword of Privacy and Notifications, and no title contains it.
        let r = search("lock");
        assert!(r.contains(&PanelId::Privacy));
        assert!(r.contains(&PanelId::Notifications));
        // "mouse" is in the title of Mouse & Touchpad and a keyword of Accessibility.
        let r = search("mouse");
        assert_eq!(r[0], PanelId::Mouse);
        assert!(r.contains(&PanelId::Accessibility));
        // Title-contains ranks above keyword-only.
        let r = search("time");
        assert_eq!(r[0], PanelId::DateTime);
    }
}
