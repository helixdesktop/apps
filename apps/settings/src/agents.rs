//! Agent Access (helixdesktop/spec `contracts/agent-access/`): the
//! settings' levels, the shell's `org.helixdesktop.AgentAccess1`
//! interface (the agents it knows; it can only take access away) and the
//! activity log the shell keeps. Blocking zbus; worker threads only.

use std::collections::HashMap;
use std::path::Path;

use helix_app_common::i18n::{n_, tr, tr_dyn};
use helix_app_common::{nc_, trf, trnf};
use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

use crate::bus::{self, Obj, Result};

/// The settings (`contracts/gsettings/org.helixdesktop.agent-access.md`).
pub const SCHEMA: &str = "org.helixdesktop.agent-access";
/// The shell's main connection.
pub const SHELL: &str = "org.gnome.Shell";
const IFACE: &str = "org.helixdesktop.AgentAccess1";
const ACCESS: Obj<'static> = Obj::new(SHELL, "/org/helixdesktop/Agent1", IFACE);

/// `access-level` values, each including the ones before it.
pub const LEVELS: [&str; 4] = ["read-only", "desktop", "apps", "screen-and-input"];
/// The level offered only behind a warning.
pub const SCREEN: &str = "screen-and-input";

/// The Access Level combo's choice (index into the first three levels)
/// and whether the separate Screen and Input switch is on, for a stored
/// `access-level`. Unknown values read as `read-only` (the contract).
pub fn level_choice(level: &str) -> (u32, bool) {
    match level {
        "desktop" => (1, false),
        "apps" => (2, false),
        SCREEN => (2, true),
        _ => (0, false),
    }
}

/// The `access-level` to store for a combo choice and the switch.
pub fn level_value(choice: u32, screen: bool) -> &'static str {
    if screen {
        return SCREEN;
    }
    LEVELS[(choice as usize).min(2)]
}

/// A level's name, untranslated (`None` for a value this version does
/// not know).
pub fn level_label(level: &str) -> Option<&'static str> {
    Some(match level {
        "read-only" => n_("Read Only"),
        "desktop" => n_("Control the Desktop"),
        "apps" => n_("Control Apps"),
        SCREEN => n_("Screen and Input"),
        _ => return None,
    })
}

/// One agent of `ListAgents` (`contracts/agent-access/README.md`, "Who
/// may manage grants").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Agent {
    /// The id the shell uses (executable, or interpreter and script).
    pub id: String,
    pub executable: String,
    /// Self-declared display name; never identity.
    pub name: String,
    /// The level of its grant.
    pub level: String,
    /// Allowed for this session.
    pub session: bool,
    /// Remembered ("Always allow").
    pub always: bool,
    /// Denied this session.
    pub denied: bool,
    /// Unix seconds of its last allowed call this session, 0 for none.
    pub last_active: i64,
    pub identified: bool,
}

fn text(record: &HashMap<String, OwnedValue>, key: &str) -> String {
    bus::get::<String>(record, key).unwrap_or_default()
}

impl Agent {
    /// From a record; `None` without an id. Keys this version does not
    /// know are ignored.
    pub fn from_record(record: &HashMap<String, OwnedValue>) -> Option<Agent> {
        let id = text(record, "agent");
        if id.is_empty() {
            return None;
        }
        let kinds: Vec<String> = bus::get(record, "kinds").unwrap_or_default();
        let has = |k: &str| kinds.iter().any(|x| x == k);
        Some(Agent {
            executable: text(record, "executable"),
            name: text(record, "name"),
            level: text(record, "level"),
            session: has("session"),
            always: has("always"),
            denied: has("denied"),
            last_active: bus::get::<i64>(record, "last-active").unwrap_or(0),
            identified: bus::get::<bool>(record, "identified").unwrap_or(false),
            id,
        })
    }

    /// The row's title: the display name, else the program.
    pub fn title(&self) -> &str {
        if !self.name.is_empty() {
            &self.name
        } else {
            self.program()
        }
    }

    /// The real program (its id, which names the script for an
    /// interpreter), shown next to a display name since a name proves
    /// nothing.
    pub fn program(&self) -> &str {
        if self.id.is_empty() {
            &self.executable
        } else {
            &self.id
        }
    }

    /// Revoke has something to end: a session grant or a denial.
    pub fn can_revoke(&self) -> bool {
        self.session || self.denied
    }

    /// Forget has a remembered grant to forget.
    pub fn can_forget(&self) -> bool {
        self.always
    }
}

/// Most recently active first, then by title.
pub fn sort(agents: &mut [Agent]) {
    agents.sort_by(|a, b| {
        b.last_active
            .cmp(&a.last_active)
            .then_with(|| a.title().to_lowercase().cmp(&b.title().to_lowercase()))
            .then_with(|| a.id.cmp(&b.id))
    });
}

/// Every agent the shell knows, sorted.
pub fn list(conn: &Connection) -> Result<Vec<Agent>> {
    let records: Vec<HashMap<String, OwnedValue>> = ACCESS.call(conn, "ListAgents", &())?;
    let mut agents: Vec<Agent> = records.iter().filter_map(Agent::from_record).collect();
    sort(&mut agents);
    Ok(agents)
}

/// Ends the agent's session grant and denial.
pub fn revoke(conn: &Connection, agent: &str) -> Result<()> {
    ACCESS.run(conn, "Revoke", &(agent,))
}

/// Revoke, and forget its remembered grant.
pub fn forget(conn: &Connection, agent: &str) -> Result<()> {
    ACCESS.run(conn, "Forget", &(agent,))
}

/// Revoke for every agent.
pub fn stop_all(conn: &Connection) -> Result<()> {
    ACCESS.run(conn, "StopAll", &())
}

/// Empties the activity log.
pub fn clear_log(conn: &Connection) -> Result<()> {
    ACCESS.run(conn, "ClearLog", &())
}

/// One reminder of `ListReminders` (`contracts/agent-access/README.md`,
/// "Settings' interface"): every agent's reminders not yet shown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reminder {
    pub id: String,
    /// When it is due, Unix seconds.
    pub time: i64,
    /// What the agent asked to be reminded of, as the shell returns it.
    pub text: String,
    /// The agent that set it: its id, executable and display name.
    pub agent: String,
    pub executable: String,
    pub name: String,
}

impl Reminder {
    /// From a record; `None` without an id. Keys this version does not
    /// know are ignored.
    pub fn from_record(record: &HashMap<String, OwnedValue>) -> Option<Reminder> {
        let id = text(record, "id");
        if id.is_empty() {
            return None;
        }
        Some(Reminder {
            time: bus::get::<i64>(record, "time").unwrap_or(0),
            text: text(record, "text"),
            agent: text(record, "agent"),
            executable: text(record, "executable"),
            name: text(record, "name"),
            id,
        })
    }

    /// The agent that set it, as the agents list names it.
    pub fn who(&self) -> Agent {
        Agent {
            id: self.agent.clone(),
            executable: self.executable.clone(),
            name: self.name.clone(),
            ..Agent::default()
        }
    }
}

/// Every agent's waiting reminders, soonest first.
pub fn reminders(conn: &Connection) -> Result<Vec<Reminder>> {
    let records: Vec<HashMap<String, OwnedValue>> = ACCESS.call(conn, "ListReminders", &())?;
    let mut list: Vec<Reminder> = records.iter().filter_map(Reminder::from_record).collect();
    list.sort_by(|a, b| a.time.cmp(&b.time).then_with(|| a.id.cmp(&b.id)));
    Ok(list)
}

/// Cancels any agent's reminder (an unknown id is not an error).
pub fn cancel_reminder(conn: &Connection, id: &str) -> Result<()> {
    ACCESS.run(conn, "CancelReminder", &(id,))
}

/// The ids of the agents' changes that can still be undone
/// (`ListChanges`: the latest of their setting, still at the value the
/// agent set), newest first.
pub fn changes(conn: &Connection) -> Result<Vec<String>> {
    let records: Vec<HashMap<String, OwnedValue>> = ACCESS.call(conn, "ListChanges", &())?;
    Ok(records
        .iter()
        .map(|r| text(r, "id"))
        .filter(|id| !id.is_empty())
        .collect())
}

/// Why `Undo` did not undo a change.
#[derive(Debug, Clone, PartialEq)]
pub enum UndoError {
    /// No longer undoable: undone, put back, or another change of the
    /// setting came since.
    Gone,
    /// The setting no longer has the value the agent set.
    Changed,
    /// The old value cannot be set any more (the picture, the power mode
    /// or the device is gone).
    Unavailable,
    Bus(bus::Error),
}

/// Puts a change's old value back (`Undo`).
pub fn undo(conn: &Connection, id: &str) -> std::result::Result<(), UndoError> {
    let Err(err) = conn.call_method(
        Some(ACCESS.dest),
        ACCESS.path,
        Some(ACCESS.iface),
        "Undo",
        &(id,),
    ) else {
        return Ok(());
    };
    if let zbus::Error::MethodError(name, message, _) = &err {
        match (name.as_str(), message.as_deref().unwrap_or("").trim()) {
            ("org.helixdesktop.Agent1.Error.NotFound", _) => return Err(UndoError::Gone),
            ("org.helixdesktop.Agent1.Error.Failed", "changed") => return Err(UndoError::Changed),
            ("org.helixdesktop.Agent1.Error.Failed", "unavailable") => {
                return Err(UndoError::Unavailable)
            }
            _ => {}
        }
    }
    Err(UndoError::Bus(err.into()))
}

/// Whether log line `e` is a change Settings offers to undo, given the
/// changes `ListChanges` listed: an allowed change of an agent's that is
/// still listed.
pub fn undoable(e: &Entry, listed: &[String]) -> bool {
    !e.change.is_empty()
        && e.outcome == "allowed"
        && e.error.is_empty()
        && CHANGE_METHODS.contains(&e.method.as_str())
        && listed.contains(&e.change)
}

/// As [`watch`], for `ChangesChanged`.
pub fn watch_changes(conn: &Connection, changed: impl FnMut() -> bool) {
    watch_signal(conn, "ChangesChanged", changed)
}

/// Calls `changed` for every `AgentsChanged` and whenever the shell
/// comes or goes; returns when `changed` returns false or the bus goes
/// away.
pub fn watch(conn: &Connection, changed: impl FnMut() -> bool) {
    watch_signal(conn, "AgentsChanged", changed)
}

/// As [`watch`], for `RemindersChanged`.
pub fn watch_reminders(conn: &Connection, changed: impl FnMut() -> bool) {
    watch_signal(conn, "RemindersChanged", changed)
}

fn watch_signal(conn: &Connection, signal: &str, mut changed: impl FnMut() -> bool) {
    let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(conn) else {
        return;
    };
    let rules = [
        zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(SHELL)
            .and_then(|b| b.interface(IFACE))
            .and_then(|b| b.member(signal))
            .map(|b| b.build()),
        zbus::MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .interface("org.freedesktop.DBus")
            .and_then(|b| b.member("NameOwnerChanged"))
            .and_then(|b| b.arg(0, SHELL))
            .map(|b| b.build()),
    ];
    for rule in rules.into_iter().flatten() {
        let _ = dbus.add_match_rule(rule);
    }
    for message in zbus::blocking::MessageIterator::from(conn) {
        let Ok(message) = message else { continue };
        let header = message.header();
        let wanted = message.message_type() == zbus::message::Type::Signal
            && match header.member().map(|m| m.as_str()) {
                Some(m) if m == signal => header.interface().is_some_and(|i| i.as_str() == IFACE),
                Some("NameOwnerChanged") => true,
                _ => false,
            };
        if wanted && !changed() {
            return;
        }
    }
}

/// One line of the activity log (`$XDG_STATE_HOME/helix/agent-activity/
/// YYYY-MM-DD.jsonl`, contract "Activity log"). Missing keys are empty
/// (0, `None`); keys this version does not know are ignored, and so are
/// most lengths (`query-length` and the like), which say little to the
/// user: only `text-length` (what an agent typed into a field or a
/// reminder) is kept.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Entry {
    /// Unix seconds (0 when unreadable).
    pub time: i64,
    pub agent: String,
    pub name: String,
    /// An `Agent1` method, an `AgentAccess1` one or an event.
    pub method: String,
    pub target: String,
    pub level: String,
    /// `allowed`, `refused:<error>` or `cancelled`.
    pub outcome: String,
    /// A refused or cancelled call: why, as a token (`password-field`,
    /// `user-input`, …); empty in lines from before the shell wrote it.
    pub reason: String,
    /// An allowed call that then failed: the error's short name.
    pub error: String,
    /// Consent prompts: `session` or `always`.
    pub grant: String,
    /// Coalesced refusals: how many calls the line stands for (0 or 1
    /// for one) and the last one's time.
    pub count: u64,
    pub until: i64,
    /// `LogFull`: `refusals` or `all`.
    pub dropped: String,
    /// `A11yAction`, `A11ySetValue`: the node's role; `A11yFind`: the
    /// role looked for.
    pub role: String,
    /// What a call did (`A11yAction`'s `click`, `A11ySetValue`'s
    /// `set-text`, …).
    pub action: String,
    /// `A11yTree`: nodes returned; `A11yFind`: nodes matched.
    pub node_count: Option<u64>,
    /// Characters of text an agent set (`A11ySetValue`, `AddReminder`).
    pub text_length: Option<u64>,
    /// `Ask` with choices: how many.
    pub choice_count: Option<u64>,
    /// `Screenshot`: `screen`, `monitor` or `window`.
    pub scope: String,
    /// `Screenshot`: the picture's size in pixels.
    pub width: Option<u64>,
    pub height: Option<u64>,
    /// `Screenshot`: how many windows and surfaces were blacked out.
    pub filled_count: Option<u64>,
    /// `GetClipboard`, `SetClipboard`: the type as a common kind
    /// (`text/plain`, `image/png`, …, `other`).
    pub mime_type: String,
    /// `GetClipboard`, `SetClipboard`: bytes read or sent.
    pub data_length: Option<u64>,
    /// `Input`: how many events it had, and how many steps were carried
    /// out (steps are not events: a click is a press and a release).
    pub event_count: Option<u64>,
    pub done_count: Option<u64>,
    /// `Input`: how many of each kind of event (absent for none).
    pub move_count: Option<u64>,
    pub click_count: Option<u64>,
    pub button_count: Option<u64>,
    pub scroll_count: Option<u64>,
    pub key_count: Option<u64>,
    pub shortcut_count: Option<u64>,
    /// A change (`SetWallpaper`, `SetAccent`, …), its `Restore` and its
    /// `Undo`: the change's id, and the value before and after (for the
    /// background also its dark style's picture and placement).
    pub change: String,
    pub old: String,
    pub new: String,
    pub old_dark: String,
    pub new_dark: String,
    pub old_fit: String,
    pub new_fit: String,
    /// `SetWallpaper`: `light`, `dark` or `both`.
    pub variant: String,
    /// A temporary change: when it will be put back.
    pub restore_at: Option<i64>,
    /// `Restore`: why (`expired`, `stopped`, `session-ended`,
    /// `restore-now`).
    pub cause: String,
    /// `SetToggle`: `on` or `off`.
    pub state: String,
}

/// A time: Unix seconds, or ISO 8601 text (0 when unreadable).
fn json_time(v: Option<&serde_json::Value>) -> i64 {
    match v {
        Some(serde_json::Value::Number(n)) => n.as_i64().unwrap_or(0),
        Some(serde_json::Value::String(t)) => gtk::glib::DateTime::from_iso8601(t, None)
            .map(|d| d.to_unix())
            .unwrap_or(0),
        _ => 0,
    }
}

impl Entry {
    /// From one line; `None` for one that is not a JSON object.
    pub fn parse(line: &str) -> Option<Entry> {
        let v = serde_json::from_str::<serde_json::Value>(line.trim()).ok()?;
        let obj = v.as_object()?;
        let s = |k: &str| {
            obj.get(k)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        };
        let n = |k: &str| obj.get(k).and_then(|v| v.as_u64());
        Some(Entry {
            time: json_time(obj.get("time")),
            agent: s("agent"),
            name: s("name"),
            method: s("method"),
            target: s("target"),
            level: s("level"),
            outcome: s("outcome"),
            reason: s("reason"),
            error: s("error"),
            grant: s("grant"),
            count: obj.get("count").and_then(|v| v.as_u64()).unwrap_or(0),
            until: json_time(obj.get("until")),
            dropped: s("dropped"),
            role: s("role"),
            action: s("action"),
            node_count: n("node-count"),
            text_length: n("text-length"),
            choice_count: n("choice-count"),
            scope: s("scope"),
            width: n("width"),
            height: n("height"),
            filled_count: n("filled-count"),
            mime_type: s("mime-type"),
            data_length: n("data-length"),
            event_count: n("event-count"),
            done_count: n("done-count"),
            move_count: n("move-count"),
            click_count: n("click-count"),
            button_count: n("button-count"),
            scroll_count: n("scroll-count"),
            key_count: n("key-count"),
            shortcut_count: n("shortcut-count"),
            change: s("change"),
            old: s("old"),
            new: s("new"),
            old_dark: s("old-dark"),
            new_dark: s("new-dark"),
            old_fit: s("old-fit"),
            new_fit: s("new-fit"),
            variant: s("variant"),
            restore_at: obj.get("restore-at").and_then(|v| v.as_i64()),
            cause: s("cause"),
            state: s("state"),
        })
    }

    /// Who: the display name, else the agent id.
    pub fn who(&self) -> &str {
        if self.name.is_empty() {
            &self.agent
        } else {
            &self.name
        }
    }

    /// The refusal's error name for `refused:<Error>`.
    pub fn refusal(&self) -> Option<&str> {
        match self.outcome.split_once(':') {
            Some(("refused", why)) => Some(why),
            _ if self.outcome == "refused" => Some(""),
            _ => None,
        }
    }

    /// The last call a coalesced line stands for, when it is one.
    pub fn coalesced(&self) -> Option<(u64, i64)> {
        (self.count >= 2).then_some((self.count, self.until.max(self.time)))
    }

    /// A call to Settings' `AgentAccess1` rather than to `Agent1`: its
    /// own methods, and `ListReminders`/`CancelReminder`, which both
    /// interfaces have, without a `level` (every `Agent1` line has one)
    /// or refused with `AccessDenied` (only `AgentAccess1` refuses so).
    pub fn is_settings_call(&self) -> bool {
        SETTINGS_METHODS.contains(&self.method.as_str())
            || (SHARED_METHODS.contains(&self.method.as_str())
                && (self.level.is_empty() || self.refusal() == Some("AccessDenied")))
    }
}

/// The `AgentAccess1` methods (Settings' own; `target` is the agent, or
/// for `Undo` the setting).
const SETTINGS_METHODS: [&str; 7] = [
    "ListAgents",
    "Revoke",
    "Forget",
    "StopAll",
    "ClearLog",
    "ListChanges",
    "Undo",
];

/// The methods whose lines are changes Settings may offer to undo
/// (contract "Changes, undo and restoring").
pub const CHANGE_METHODS: [&str; 6] = [
    "SetWallpaper",
    "SetAccent",
    "SetPowerMode",
    "SetToggle",
    "StarFile",
    "SetFavorite",
];
/// Methods of both `Agent1` and `AgentAccess1`.
const SHARED_METHODS: [&str; 2] = ["ListReminders", "CancelReminder"];

/// What a method or event did, as a short untranslated phrase (`None`
/// for a name this version does not know). Every method of `Agent1` and
/// `AgentAccess1`, and the log's events.
pub fn method_label(method: &str) -> Option<&'static str> {
    Some(match method {
        // Agent1.
        "GetStatus" => n_("Checked its access"),
        "RequestAccess" => n_("Asked for access"),
        "GetState" => n_("Read the desktop state"),
        "ListWindows" => n_("Listed windows"),
        "ListApps" => n_("Listed apps"),
        "ListWorkspaces" => n_("Listed workspaces"),
        "ListMonitors" => n_("Listed displays"),
        "ListNotifications" => n_("Listed notifications"),
        "RecentFiles" => n_("Listed recent files"),
        "SearchFiles" => n_("Searched files"),
        "StarredFiles" => n_("Listed starred files"),
        "GetMedia" => n_("Read the media players"),
        "GetPower" => n_("Read the battery and power mode"),
        "WaitForWindow" => n_("Waited for a window"),
        "WaitForWindowClosed" => n_("Waited for a window to close"),
        "Ask" => n_("Asked you a question"),
        "Confirm" => n_("Asked you to confirm"),
        "AddReminder" => n_("Added a reminder"),
        "ListReminders" => n_("Listed reminders"),
        "CancelReminder" => n_("Cancelled a reminder"),
        "Open" => n_("Opened a file or link"),
        "Launch" => n_("Started an app"),
        "WindowAction" => n_("Changed a window"),
        "SwitchWorkspace" => n_("Switched workspace"),
        "SetToggle" => n_("Turned a setting on or off"),
        "SetValue" => n_("Adjusted a setting"),
        "MediaControl" => n_("Controlled a media player"),
        "Notify" => n_("Sent a notification"),
        "GetClipboard" => n_("Read the clipboard"),
        "SetClipboard" => n_("Changed the clipboard"),
        "A11yTree" => n_("Read an app’s contents"),
        "A11yFind" => n_("Searched an app’s controls"),
        "A11yAction" => n_("Used a control in an app"),
        "A11ySetValue" => n_("Filled in a field in an app"),
        "Screenshot" => n_("Took a screenshot"),
        "Input" => n_("Typed or clicked"),
        "SetWallpaper" => n_("Changed the background"),
        "SetAccent" => n_("Changed the accent color"),
        "SetPowerMode" => n_("Changed the power mode"),
        "StarFile" => n_("Starred or unstarred a file"),
        "SetFavorite" => n_("Changed the dock"),
        "DismissNotification" => n_("Dismissed a notification"),
        "NotificationAction" => n_("Pressed a notification button"),
        "RevealFile" => n_("Showed a file in the file manager"),
        // AgentAccess1, as Settings did it.
        "ListAgents" => n_("Listed agents"),
        "Revoke" => n_("Access revoked"),
        "Forget" => n_("Access revoked and forgotten"),
        "StopAll" => n_("All access revoked"),
        "ClearLog" => n_("Activity log cleared"),
        "ListChanges" => n_("Listed changes"),
        "Undo" => n_("Undid a change"),
        // Events.
        "ConsentPrompt" => n_("Asked for access"),
        "SensitivePrompt" => n_("Asked to confirm an action"),
        "Stop" => n_("Stopped by you"),
        "Reminder" => n_("Reminder due"),
        "LogFull" => n_("Activity log full"),
        "Restore" => n_("Put a temporary change back"),
        _ => return None,
    })
}

/// An allowed `AgentAccess1` call Settings made, where it reads other
/// than the `Agent1` method of the same name.
pub fn settings_method_label(method: &str) -> Option<&'static str> {
    Some(match method {
        "CancelReminder" => n_("Reminder cancelled"),
        "Undo" => n_("Change undone"),
        _ => return None,
    })
}

/// An `AgentAccess1` call that was refused (a program other than
/// Settings tried it).
pub fn settings_method_refused_label(method: &str) -> Option<&'static str> {
    Some(match method {
        "ListAgents" => n_("Tried to list agents"),
        "ListReminders" => n_("Tried to list reminders"),
        "CancelReminder" => n_("Tried to cancel a reminder"),
        "Revoke" => n_("Tried to revoke access"),
        "Forget" => n_("Tried to forget an agent"),
        "StopAll" => n_("Tried to revoke all access"),
        "ClearLog" => n_("Tried to clear the activity log"),
        "ListChanges" => n_("Tried to list changes"),
        "Undo" => n_("Tried to undo a change"),
        _ => return None,
    })
}

/// Why the policy refused a call (`refused:<Error>`), untranslated.
pub fn refusal_label(error: &str) -> Option<&'static str> {
    Some(match error {
        "Sandboxed" => n_("From a sandboxed app"),
        "AccessOff" => n_("Agent Access was off"),
        "Remote" => n_("From a remote session"),
        "Locked" => n_("The screen was locked"),
        "LevelTooLow" => n_("Needs a higher access level"),
        "FeatureOff" => n_("That feature was turned off"),
        "NotGranted" => n_("Not allowed yet"),
        "DeniedByUser" => n_("Denied by you"),
        "Excluded" => n_("Excluded app"),
        "NeedsConfirmation" => n_("Needed your confirmation"),
        "AccessDenied" => n_("Only Settings may do this"),
        _ => return None,
    })
}

/// How an allowed call then failed (`error`), untranslated.
pub fn failure_label(error: &str) -> Option<&'static str> {
    Some(match error {
        "TimedOut" => n_("Failed: timed out"),
        "NotFound" => n_("Failed: not found"),
        "NotSupported" => n_("Failed: not supported"),
        "InvalidArgs" => n_("Failed: invalid request"),
        "Cancelled" => n_("Failed: cancelled"),
        "Failed" => n_("Failed"),
        _ => return None,
    })
}

/// An accessible role (`role`, AT-SPI's name) for people, untranslated
/// (`None` for the less common ones, shown as they are).
pub fn role_label(role: &str) -> Option<&'static str> {
    Some(match role {
        "button" => nc_!("accessible role", "Button"),
        "toggle button" => nc_!("accessible role", "Toggle button"),
        "push button menu" => nc_!("accessible role", "Menu button"),
        "check box" => nc_!("accessible role", "Check box"),
        "radio button" => nc_!("accessible role", "Option button"),
        "switch" => nc_!("accessible role", "Switch"),
        "text" | "entry" => nc_!("accessible role", "Text field"),
        "password text" => nc_!("accessible role", "Password field"),
        "combo box" => nc_!("accessible role", "Drop-down list"),
        "slider" => nc_!("accessible role", "Slider"),
        "spin button" => nc_!("accessible role", "Number field"),
        "link" => nc_!("accessible role", "Link"),
        "menu item" | "check menu item" | "radio menu item" => nc_!("accessible role", "Menu item"),
        "page tab" => nc_!("accessible role", "Tab"),
        "list item" | "tree item" => nc_!("accessible role", "List item"),
        "table cell" => nc_!("accessible role", "Table cell"),
        "label" => nc_!("accessible role", "Label"),
        "heading" => nc_!("accessible role", "Heading"),
        "image" | "icon" => nc_!("accessible role", "Image"),
        "dialog" => nc_!("accessible role", "Dialog"),
        "window" | "frame" => nc_!("accessible role", "Window"),
        _ => return None,
    })
}

/// What an `A11yAction` or `A11ySetValue` did (`action`), untranslated
/// (`None` for a name this version does not know).
pub fn control_action_label(action: &str) -> Option<&'static str> {
    Some(match action {
        "click" | "press" => nc_!("control action", "Clicked"),
        "activate" | "default" | "default.activate" => nc_!("control action", "Activated"),
        "toggle" => nc_!("control action", "Toggled"),
        "select" => nc_!("control action", "Selected"),
        "focus" => nc_!("control action", "Focused"),
        "expand" => nc_!("control action", "Expanded"),
        "collapse" => nc_!("control action", "Collapsed"),
        "open" => nc_!("control action", "Opened"),
        "jump" | "link.open" => nc_!("control action", "Followed a link"),
        "menu" | "showmenu" | "menu.popup" => nc_!("control action", "Opened a menu"),
        "increment" => nc_!("control action", "Increased"),
        "decrement" => nc_!("control action", "Decreased"),
        "window.close" => nc_!("control action", "Closed the window"),
        "window.minimize" => nc_!("control action", "Minimized the window"),
        "window.toggle-maximized" => nc_!("control action", "Maximized or restored the window"),
        "selection.select-all" => nc_!("control action", "Selected all"),
        "selection.delete" => nc_!("control action", "Deleted the selection"),
        "text.clear" => nc_!("control action", "Cleared the text"),
        "clipboard.copy" => nc_!("control action", "Copied"),
        "clipboard.cut" => nc_!("control action", "Cut"),
        "clipboard.paste" => nc_!("control action", "Pasted"),
        "link.copy" => nc_!("control action", "Copied a link"),
        "misc.insert-emoji" => nc_!("control action", "Inserted an emoji"),
        "other" => nc_!("control action", "Another action"),
        "set-text" => nc_!("control action", "Set text"),
        "set-value" => nc_!("control action", "Set a value"),
        _ => return None,
    })
}

/// What a `MediaControl` did (`action`), untranslated (`None` for a name
/// this version does not know).
pub fn media_action_label(action: &str) -> Option<&'static str> {
    Some(match action {
        "play" => nc_!("media action", "Played"),
        "pause" => nc_!("media action", "Paused"),
        "play-pause" => nc_!("media action", "Played or paused"),
        "next" => nc_!("media action", "Skipped to the next track"),
        "previous" => nc_!("media action", "Went back to the previous track"),
        "stop" => nc_!("media action", "Stopped"),
        "seek" => nc_!("media action", "Moved in the track"),
        _ => return None,
    })
}

/// A setting a change changed (a Change's `name`, a toggle's name: the
/// `target` of a change, `Restore` or `Undo` line), untranslated; `None`
/// for a starred file or a favorite (their target is the file or app).
pub fn setting_label(setting: &str) -> Option<&'static str> {
    Some(match setting {
        "wallpaper" => nc_!("changed setting", "Background"),
        "accent" => nc_!("changed setting", "Accent color"),
        "power-mode" => nc_!("changed setting", "Power mode"),
        "dark-style" => nc_!("changed setting", "Dark style"),
        "do-not-disturb" => nc_!("changed setting", "Do Not Disturb"),
        "night-light" => nc_!("changed setting", "Night Light"),
        "wifi" => nc_!("changed setting", "Wi-Fi"),
        "airplane-mode" => nc_!("changed setting", "Airplane mode"),
        "mute" => nc_!("changed setting", "Sound muted"),
        _ => return None,
    })
}

/// A change's `old` or `new` value as Settings shows it: an accent color
/// or power mode by name, a picture by its file's name, a star or a
/// favorite in words, a switch as on or off.
pub fn change_value(method: &str, target: &str, value: &str) -> String {
    let star = method == "StarFile"
        || (matches!(method, "Restore" | "Undo") && target.starts_with("file://"));
    let favorite = method == "SetFavorite"
        || (matches!(method, "Restore" | "Undo")
            && setting_label(target).is_none()
            && !target.starts_with("file://"));
    let label = match (value, star, favorite) {
        ("on", true, _) => Some(nc_!("star", "Starred")),
        ("off", true, _) => Some(nc_!("star", "Not starred")),
        ("on", _, true) => Some(nc_!("dock", "In the dock")),
        ("off", _, true) => Some(nc_!("dock", "Not in the dock")),
        ("on", ..) => Some(nc_!("switch", "On")),
        ("off", ..) => Some(nc_!("switch", "Off")),
        ("blue", ..) => Some(nc_!("accent color", "Blue")),
        ("teal", ..) => Some(nc_!("accent color", "Teal")),
        ("green", ..) => Some(nc_!("accent color", "Green")),
        ("yellow", ..) => Some(nc_!("accent color", "Yellow")),
        ("orange", ..) => Some(nc_!("accent color", "Orange")),
        ("red", ..) => Some(nc_!("accent color", "Red")),
        ("pink", ..) => Some(nc_!("accent color", "Pink")),
        ("purple", ..) => Some(nc_!("accent color", "Purple")),
        ("slate", ..) => Some(nc_!("accent color", "Slate")),
        ("power-saver", ..) => Some(nc_!("power mode", "Power Saver")),
        ("balanced", ..) => Some(nc_!("power mode", "Balanced")),
        ("performance", ..) => Some(nc_!("power mode", "Performance")),
        ("", ..) if target == "wallpaper" || method == "SetWallpaper" => {
            Some(nc_!("background", "None"))
        }
        _ => None,
    };
    if let Some(label) = label {
        return tr_dyn(label);
    }
    // A picture by its file's name.
    if value.starts_with("file://") {
        if let Ok((path, _)) = gtk::glib::filename_from_uri(value) {
            if let Some(name) = path.file_name() {
                return name.to_string_lossy().into_owned();
            }
        }
    }
    value.to_string()
}

/// Why a temporary change was put back (`cause`), untranslated.
pub fn cause_label(cause: &str) -> Option<&'static str> {
    Some(match cause {
        "expired" => n_("Its time was up"),
        "stopped" => n_("The agent was stopped"),
        "session-ended" => n_("The session ended"),
        "restore-now" => n_("Put back from the top bar"),
        _ => return None,
    })
}

/// What a line says about what was done, beyond its title and target, in
/// people's words: the role and action of a control, how many nodes,
/// characters or choices. Only what the line holds.
pub fn details(e: &Entry) -> Vec<String> {
    let mut out = Vec::new();
    if !e.role.is_empty() {
        out.push(role_label(&e.role).map_or_else(|| e.role.clone(), tr_dyn));
    }
    // Other methods' actions (WindowAction's, a SensitivePrompt's) are
    // said by the title and target.
    if !e.action.is_empty() && matches!(e.method.as_str(), "A11yAction" | "A11ySetValue") {
        out.push(control_action_label(&e.action).map_or_else(|| e.action.clone(), tr_dyn));
    }
    if !e.action.is_empty() && e.method == "MediaControl" {
        out.push(media_action_label(&e.action).map_or_else(|| e.action.clone(), tr_dyn));
    }
    if e.method == "NotificationAction" {
        match e.action.as_str() {
            "button" => out.push(tr("One of its buttons").to_string()),
            "default" => out.push(tr("The notification itself").to_string()),
            _ => {}
        }
    }
    // A change: from what to what (never more than setting values).
    if !e.old.is_empty() || !e.new.is_empty() {
        let (old, new) = (
            change_value(&e.method, &e.target, &e.old),
            change_value(&e.method, &e.target, &e.new),
        );
        out.push(if old == new {
            trf!("Already {value}", value = new)
        } else {
            trf!("{old} → {new}", old = old, new = new)
        });
    }
    match e.variant.as_str() {
        "light" => out.push(tr("Light style only").to_string()),
        "dark" => out.push(tr("Dark style only").to_string()),
        _ => {}
    }
    if let Some(at) = e
        .restore_at
        .and_then(|t| gtk::glib::DateTime::from_unix_local(t).ok())
    {
        let time = at
            .format("%H:%M")
            .map(|s| s.to_string())
            .unwrap_or_default();
        out.push(trf!("Until {time}", time = time));
    }
    if let Some(cause) = cause_label(&e.cause) {
        out.push(tr_dyn(cause));
    }
    if let Some(n) = e.node_count {
        out.push(if e.method == "A11yFind" {
            trnf!("{n} match", "{n} matches", n)
        } else {
            trnf!("{n} item", "{n} items", n)
        });
    }
    // The clipboard: what kind of content, and how much.
    if !e.mime_type.is_empty() {
        out.push(mime_kind_label(&e.mime_type).map_or_else(|| e.mime_type.clone(), tr_dyn));
    }
    if let Some(n) = e.data_length {
        out.push(gtk::glib::format_size(n).to_string());
    }
    // A screenshot: its size and what was blacked out in it.
    if let (Some(width), Some(height)) = (e.width, e.height) {
        out.push(trf!(
            "{width} × {height} pixels",
            width = width,
            height = height
        ));
    }
    match e.filled_count {
        Some(0) => out.push(tr("Nothing hidden").to_string()),
        Some(n) => out.push(trnf!("{n} area hidden", "{n} areas hidden", n)),
        None => {}
    }
    // Input: how many events of each kind, never which keys or where.
    if let Some(n) = e.event_count {
        out.push(trnf!("{n} action", "{n} actions", n));
    }
    let kinds = [
        e.click_count.map(|n| trnf!("{n} click", "{n} clicks", n)),
        e.shortcut_count
            .map(|n| trnf!("{n} shortcut", "{n} shortcuts", n)),
        e.key_count.map(|n| trnf!("{n} key", "{n} keys", n)),
        e.move_count
            .map(|n| trnf!("{n} pointer move", "{n} pointer moves", n)),
        e.scroll_count
            .map(|n| trnf!("{n} scroll", "{n} scrolls", n)),
        e.button_count.map(|n| {
            trnf!(
                "{n} button press or release",
                "{n} button presses or releases",
                n
            )
        }),
    ];
    out.extend(kinds.into_iter().flatten());
    if let Some(n) = e.text_length {
        out.push(if e.method == "Input" {
            trnf!("{n} character typed", "{n} characters typed", n)
        } else {
            trnf!("{n} character", "{n} characters", n)
        });
    }
    if let Some(n) = e.choice_count {
        out.push(trnf!("{n} choice", "{n} choices", n));
    }
    // How far an input went when it did not go to the end (a finished
    // one carried out every step: nothing to add).
    if let Some(n) = e.done_count {
        if e.outcome != "allowed" || !e.error.is_empty() {
            out.push(if n == 0 {
                tr("Stopped before the first step").to_string()
            } else {
                trnf!("Stopped after {n} step", "Stopped after {n} steps", n)
            });
        }
    }
    out
}

/// What kind of content went to or from the clipboard (`mime-type`, as
/// the log names it), untranslated (`None` for a kind this version does
/// not know).
pub fn mime_kind_label(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "text/plain" => nc_!("clipboard content", "Text"),
        "text/html" => nc_!("clipboard content", "HTML"),
        "text/uri-list" => nc_!("clipboard content", "Links"),
        "image/png" | "image/jpeg" | "image/gif" | "image/svg+xml" => {
            nc_!("clipboard content", "Picture")
        }
        "application/pdf" => nc_!("clipboard content", "PDF document"),
        "x-kde-passwordManagerHint" => nc_!("clipboard content", "Password manager hint"),
        "other" => nc_!("clipboard content", "Other"),
        _ => return None,
    })
}

/// Why the policy refused a call, where the method says more than the
/// error alone (`Excluded` covers protected apps, password fields and
/// system prompts): for lines without a `reason` ([`reason_label`]).
pub fn method_refusal_label(method: &str, error: &str) -> Option<&'static str> {
    Some(match (method, error) {
        ("GetClipboard", "Excluded") => n_("Copied from a protected app, or a password"),
        ("SetClipboard", "Excluded") => n_("A system prompt was open"),
        ("GetClipboard" | "SetClipboard", "FeatureOff") => n_("Clipboard access was off"),
        ("NotificationAction", "FeatureOff") => n_("Notification access was off"),
        ("Screenshot", "Excluded") => n_("A protected window or a system prompt"),
        ("Input", "Excluded") => n_("A protected window, a password field or a system prompt"),
        _ => return refusal_label(error),
    })
}

/// Why a call was refused or cancelled (`reason`, contract "Activity
/// log", "Reasons"), untranslated; `None` for a token this version does
/// not know (the line is then shown as if it had none).
pub fn reason_label(method: &str, reason: &str) -> Option<&'static str> {
    Some(match (method, reason) {
        ("GetClipboard", "password-field") => n_("A password manager marked it secret"),
        (_, "password-field") => n_("A password field"),
        (_, "system-prompt") => n_("A system prompt was open"),
        (_, "shell") => n_("Part of the desktop"),
        (_, "settings") => n_("Settings"),
        (_, "app") => n_("An excluded app"),
        (_, "clipboard") => n_("Clipboard access was off"),
        (_, "notifications") => n_("Notification access was off"),
        (_, "private-file") => n_("A file in a hidden or system folder"),
        (_, "changed") => n_("Not put back: it was changed since"),
        (_, "unavailable") => n_("Not put back: the old value is gone"),
        (_, "user-input") => n_("Stopped: you used the keyboard or pointer"),
        (_, "cooldown") => n_("Refused: you had just used the keyboard or pointer"),
        (_, "stopped") => n_("Stopped from the indicator or Settings"),
        _ => return None,
    })
}

/// A log line as the page shows it, translated: what happened, what it
/// was done to, who did it, and how it went.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Described {
    pub title: String,
    /// Empty for none.
    pub target: String,
    /// More about what was done ([`details`]).
    pub details: Vec<String>,
    /// Empty for none (`LogFull`).
    pub who: String,
    pub status: Vec<String>,
}

/// A target for people: a window by its app, a `file://` URI as a path;
/// a `SensitivePrompt`'s method by what it does; nothing for a reminder
/// (`reminder:<id>`: the title says it is one, the id says nothing).
pub fn target_label(method: &str, target: &str) -> String {
    if method == "SensitivePrompt" {
        return method_label(target).map_or_else(|| target.to_string(), tr_dyn);
    }
    if target.starts_with("reminder:") {
        return String::new();
    }
    match (method, target) {
        // Where the content came from.
        ("GetClipboard", app) if !app.is_empty() && !app.starts_with("window:") => {
            return trf!("Copied from {app}", app = app);
        }
        ("Screenshot", "screen") => return tr("The whole screen").to_string(),
        ("Screenshot", t) if t.starts_with("monitor:") => {
            return trf!("Display {name}", name = &t["monitor:".len()..]);
        }
        _ => {}
    }
    // A notification by its app when the line names it.
    if let Some(note) = target.strip_prefix("notification:") {
        return match note.split_once(" (") {
            Some((_, app)) if app.ends_with(')') && app.len() > 1 => {
                trf!("A notification of {app}", app = &app[..app.len() - 1])
            }
            _ => tr("A notification").to_string(),
        };
    }
    // A changed setting by its name.
    if matches!(
        method,
        "SetToggle" | "SetWallpaper" | "SetAccent" | "SetPowerMode" | "Restore" | "Undo"
    ) {
        if let Some(label) = setting_label(target) {
            return tr_dyn(label);
        }
    }
    // A media player by its app when the line names it, else by its id.
    if let Some(player) = target.strip_prefix("player:") {
        return match player.split_once(" (") {
            Some((_, app)) if app.ends_with(')') && app.len() > 1 => {
                trf!("A media player of {app}", app = &app[..app.len() - 1])
            }
            _ => trf!("Media player “{id}”", id = player),
        };
    }
    if let Some(window) = target.strip_prefix("window:") {
        return match window.split_once(" (") {
            Some((_, app)) if app.ends_with(')') && app.len() > 1 => {
                trf!("A window of {app}", app = &app[..app.len() - 1])
            }
            _ => tr("A window").to_string(),
        };
    }
    if target.starts_with("file://") {
        if let Ok((path, _)) = gtk::glib::filename_from_uri(target) {
            return path.display().to_string();
        }
    }
    target.to_string()
}

/// Describes `e` (contract "Activity log": lines, outcomes and events).
pub fn describe(e: &Entry) -> Described {
    let method = e.method.as_str();
    let raw = |s: &str| s.to_string();
    let settings = e.is_settings_call();
    let refusal = e.refusal();
    let title = match settings
        .then(|| match refusal {
            Some(_) => settings_method_refused_label(method),
            None => settings_method_label(method),
        })
        .flatten()
        .or_else(|| method_label(method))
    {
        Some(t) => tr_dyn(t),
        None => raw(method),
    };
    let who = if settings && e.outcome == "allowed" {
        tr("Settings").to_string()
    } else {
        raw(e.who())
    };
    // The line's `reason` says more than the error; older lines have
    // none, and get what the method and error say.
    let known_reason = reason_label(method, &e.reason);
    let reason = |why: &str| match known_reason.or_else(|| method_refusal_label(method, why)) {
        Some(t) => tr_dyn(t),
        None => raw(why),
    };
    let mut status: Vec<String> = Vec::new();
    match (method, e.outcome.as_str()) {
        ("ConsentPrompt", outcome) => {
            if let Some(level) = level_label(&e.level) {
                status.push(tr_dyn(level));
            }
            status.push(match (outcome, e.grant.as_str()) {
                ("allowed", "session") => tr("Allowed for this session").to_string(),
                ("allowed", "always") => tr("Always allowed").to_string(),
                ("allowed", _) => tr("Allowed").to_string(),
                ("refused:DeniedByUser", _) => tr("Denied").to_string(),
                ("cancelled", _) => tr("Not answered").to_string(),
                _ => raw(outcome),
            });
        }
        ("SensitivePrompt", outcome) => status.push(match outcome {
            "allowed" => tr("Allowed").to_string(),
            "refused:DeniedByUser" => tr("Denied").to_string(),
            "cancelled" => tr("Not answered").to_string(),
            _ => raw(outcome),
        }),
        // The title says it all.
        ("Stop", "cancelled") => {}
        ("Reminder", outcome) => match (outcome, refusal) {
            ("allowed", _) => status.push(tr("Shown").to_string()),
            ("cancelled", _) => status.push(tr("Cancelled by you").to_string()),
            (_, Some(why)) => {
                status.push(tr("Not shown").to_string());
                if !why.is_empty() {
                    status.push(reason(why));
                }
            }
            _ => status.push(raw(outcome)),
        },
        ("Restore", "allowed") => status.push(tr("Put back").to_string()),
        // Settings' Undo that did nothing.
        ("Undo", "allowed") if settings && !e.error.is_empty() => {
            status.push(match e.error.as_str() {
                "NotFound" => tr("Not undone: it could no longer be undone").to_string(),
                _ => tr("Not undone").to_string(),
            })
        }
        ("LogFull", _) => match e.dropped.as_str() {
            "refusals" => status.push(tr("Refused calls are no longer recorded today").to_string()),
            "all" => status.push(tr("Nothing more is recorded today").to_string()),
            _ => {}
        },
        (_, "allowed") if settings => {}
        (_, "allowed") => {
            status.push(tr("Allowed").to_string());
            if !e.error.is_empty() {
                status.push(match failure_label(&e.error) {
                    Some(t) => tr_dyn(t),
                    None => trf!("Failed: {error}", error = e.error),
                });
            }
        }
        // Stopped, and the line says why.
        (_, "cancelled") if known_reason.is_some() => {
            status.extend(known_reason.map(tr_dyn));
        }
        // Stopped while it ran (it has a done-count): the user's input,
        // Stop, or the cooldown after the user's input.
        ("Input", "cancelled") if e.done_count.is_some() => {
            status.push(tr("Stopped by you").to_string())
        }
        (_, "cancelled") => status.push(tr("Cancelled").to_string()),
        (_, outcome) => match refusal {
            Some(why) => {
                status.push(match e.coalesced() {
                    Some((n, _)) => trnf!("Refused {n} time", "Refused {n} times", n),
                    None => tr("Refused").to_string(),
                });
                if !why.is_empty() {
                    status.push(reason(why));
                }
            }
            None => status.push(raw(outcome)),
        },
    }
    let mut target = target_label(method, &e.target);
    if target.is_empty() && method == "Screenshot" {
        // No target recorded: what was taken, from the scope.
        target = match e.scope.as_str() {
            "screen" => tr("The whole screen").to_string(),
            "monitor" => tr("A display").to_string(),
            "window" => tr("A window").to_string(),
            _ => String::new(),
        };
    }
    Described {
        title,
        target,
        details: details(e),
        who,
        status,
    }
}

/// The newest `limit` entries of the log in `dir`, newest first, and
/// whether there were more. A missing directory is an empty log;
/// unreadable lines are skipped.
pub fn read_log(dir: &Path, limit: usize) -> (Vec<Entry>, bool) {
    let mut days: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    // YYYY-MM-DD sorts by date.
    days.sort();
    let mut entries = Vec::new();
    let mut more = false;
    for day in days.iter().rev() {
        let Ok(text) = std::fs::read_to_string(day) else {
            continue;
        };
        let mut of_day: Vec<Entry> = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .filter_map(Entry::parse)
            .collect();
        of_day.sort_by_key(|e| std::cmp::Reverse(e.time));
        for e in of_day {
            if entries.len() == limit {
                more = true;
                break;
            }
            entries.push(e);
        }
        if more {
            break;
        }
    }
    (entries, more)
}

/// How long ago `then` was, in whole units (`now`, `then` in Unix
/// seconds): `None` for never (0), else (count, unit) with unit `s`
/// (under a minute; count 0), `m`, `h` or `d`.
pub fn ago(now: i64, then: i64) -> Option<(i64, char)> {
    if then <= 0 {
        return None;
    }
    let d = (now - then).max(0);
    Some(match d {
        0..=59 => (0, 's'),
        60..=3599 => (d / 60, 'm'),
        3600..=86399 => (d / 3600, 'h'),
        _ => (d / 86400, 'd'),
    })
}

/// `excluded-apps` with `app` added (without a `.desktop` suffix, as the
/// contract compares them), or `None` when it is already there.
pub fn with_excluded(list: &[String], app: &str) -> Option<Vec<String>> {
    let app = app.strip_suffix(".desktop").unwrap_or(app);
    if app.is_empty()
        || list
            .iter()
            .any(|a| a.strip_suffix(".desktop").unwrap_or(a) == app)
    {
        return None;
    }
    let mut out = list.to_vec();
    out.push(app.to_string());
    Some(out)
}

/// `excluded-apps` without `app`.
pub fn without_excluded(list: &[String], app: &str) -> Vec<String> {
    list.iter()
        .filter(|a| a.strip_suffix(".desktop").unwrap_or(a) != app)
        .cloned()
        .collect()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::bus::testbus;
    use std::sync::{Arc, Mutex};
    use zbus::object_server::SignalEmitter;
    use zbus::zvariant::Value;

    #[test]
    fn levels_map_to_the_combo_and_the_switch() {
        assert_eq!(level_choice("read-only"), (0, false));
        assert_eq!(level_choice("desktop"), (1, false));
        assert_eq!(level_choice("apps"), (2, false));
        assert_eq!(level_choice("screen-and-input"), (2, true));
        assert_eq!(
            level_choice("nonsense"),
            (0, false),
            "unknown reads as read-only"
        );
        for level in LEVELS {
            let (choice, screen) = level_choice(level);
            assert_eq!(level_value(choice, screen), level, "round trip");
            assert!(level_label(level).is_some());
        }
        // Turning Screen and Input off leaves Control Apps.
        assert_eq!(level_value(2, false), "apps");
        assert_eq!(level_value(0, true), SCREEN);
        assert_eq!(level_value(9, false), "apps");
        assert_eq!(level_label("x"), None);
    }

    fn record(pairs: &[(&str, Value<'_>)]) -> HashMap<String, OwnedValue> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.try_to_owned().unwrap()))
            .collect()
    }

    #[test]
    fn agents_from_records() {
        let a = Agent::from_record(&record(&[
            ("agent", Value::from("/usr/bin/node /home/me/agent.js")),
            ("executable", Value::from("/usr/bin/node")),
            ("name", Value::from("Example Agent")),
            ("level", Value::from("desktop")),
            ("kinds", Value::from(vec!["session", "always"])),
            ("granted", Value::from(1795132800i64)),
            ("last-active", Value::from(1795132900i64)),
            ("identified", Value::from(true)),
            ("future-key", Value::from(7u32)),
        ]))
        .unwrap();
        assert_eq!(a.title(), "Example Agent");
        assert_eq!(a.program(), "/usr/bin/node /home/me/agent.js");
        assert!(a.session && a.always && !a.denied && a.identified);
        assert!(a.can_revoke() && a.can_forget());
        assert_eq!(a.last_active, 1795132900);

        let b = Agent::from_record(&record(&[
            ("agent", Value::from("/usr/bin/foot")),
            ("kinds", Value::from(vec!["denied"])),
        ]))
        .unwrap();
        assert_eq!(b.title(), "/usr/bin/foot", "no name: the program");
        assert!(b.can_revoke() && !b.can_forget());
        assert_eq!(b.last_active, 0);

        let c = Agent::from_record(&record(&[("kinds", Value::from(vec!["always"]))]));
        assert_eq!(c, None, "no id");

        let mut list = vec![
            Agent {
                id: "/b".into(),
                name: "beta".into(),
                ..Agent::default()
            },
            Agent {
                id: "/a".into(),
                name: "Alpha".into(),
                ..Agent::default()
            },
            Agent {
                id: "/c".into(),
                last_active: 5,
                ..Agent::default()
            },
        ];
        sort(&mut list);
        let ids: Vec<&str> = list.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, ["/c", "/a", "/b"]);
    }

    /// Every method of the contract's interfaces
    /// (`contracts/agent-access/*.xml`) and the log's events.
    const METHODS: [&str; 51] = [
        "GetStatus",
        "RequestAccess",
        "GetState",
        "ListWindows",
        "ListApps",
        "ListWorkspaces",
        "ListMonitors",
        "ListNotifications",
        "RecentFiles",
        "SearchFiles",
        "StarredFiles",
        "GetMedia",
        "GetPower",
        "WaitForWindow",
        "WaitForWindowClosed",
        "Ask",
        "Confirm",
        "AddReminder",
        "ListReminders",
        "CancelReminder",
        "Open",
        "Launch",
        "WindowAction",
        "SwitchWorkspace",
        "SetToggle",
        "SetValue",
        "MediaControl",
        "Notify",
        "GetClipboard",
        "SetClipboard",
        "A11yTree",
        "A11yFind",
        "A11yAction",
        "A11ySetValue",
        "Screenshot",
        "Input",
        "ListAgents",
        "Revoke",
        "Forget",
        "StopAll",
        "ClearLog",
        "SetWallpaper",
        "SetAccent",
        "SetPowerMode",
        "StarFile",
        "SetFavorite",
        "DismissNotification",
        "NotificationAction",
        "RevealFile",
        "ListChanges",
        "Undo",
    ];
    /// The contract's methods this version does not label yet (a later
    /// stage of the contract lists them here until Settings has them).
    const LATER: [&str; 0] = [];
    const EVENTS: [&str; 6] = [
        "ConsentPrompt",
        "SensitivePrompt",
        "Stop",
        "Reminder",
        "LogFull",
        "Restore",
    ];

    #[test]
    fn every_method_and_event_has_a_label() {
        for m in METHODS.iter().chain(&EVENTS) {
            let label = method_label(m).unwrap_or_else(|| panic!("{m}"));
            assert_ne!(label, *m, "{m}: a phrase, not the name");
        }
        // The contract's XML, when it is next to this repository.
        let spec = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../helix-desktop-spec/contracts/agent-access");
        for file in [
            "org.helixdesktop.Agent1.xml",
            "org.helixdesktop.AgentAccess1.xml",
        ] {
            let Ok(xml) = std::fs::read_to_string(spec.join(file)) else {
                continue;
            };
            for part in xml.split("<method name=\"").skip(1) {
                let name = part.split('"').next().unwrap();
                assert!(
                    METHODS.contains(&name) || LATER.contains(&name),
                    "{name} from {file}"
                );
            }
        }
        for m in SETTINGS_METHODS.iter().chain(&SHARED_METHODS) {
            assert!(settings_method_refused_label(m).is_some(), "{m}");
        }
        // AgentAccess1's methods are all Settings' or shared with Agent1.
        if let Ok(xml) = std::fs::read_to_string(spec.join("org.helixdesktop.AgentAccess1.xml")) {
            for part in xml.split("<method name=\"").skip(1) {
                let name = part.split('"').next().unwrap();
                assert!(
                    SETTINGS_METHODS.contains(&name)
                        || SHARED_METHODS.contains(&name)
                        || LATER.contains(&name),
                    "{name}"
                );
            }
        }
        assert_eq!(method_label("A11yFind"), Some("Searched an app’s controls"));
        assert_eq!(method_label("Reminder"), Some("Reminder due"));
        assert_eq!(method_label("FutureMethod"), None);
        assert_eq!(method_label("ListWindows"), Some("Listed windows"));
        assert_eq!(method_label("SearchFiles"), Some("Searched files"));
        assert_eq!(method_label("Stop"), Some("Stopped by you"));
        assert_eq!(method_label("Revoke"), Some("Access revoked"));
        for why in [
            "Sandboxed",
            "AccessOff",
            "Remote",
            "Locked",
            "LevelTooLow",
            "FeatureOff",
            "NotGranted",
            "DeniedByUser",
            "Excluded",
            "NeedsConfirmation",
            "AccessDenied",
        ] {
            assert!(refusal_label(why).is_some(), "{why}");
        }
        assert_eq!(refusal_label("Later"), None);
        for error in [
            "TimedOut",
            "NotFound",
            "NotSupported",
            "Failed",
            "InvalidArgs",
        ] {
            assert!(failure_label(error).is_some(), "{error}");
        }
        assert_eq!(failure_label("TimedOut"), Some("Failed: timed out"));
        assert_eq!(failure_label("Later"), None);
    }

    fn shown(line: &str) -> (String, String, String, String) {
        let d = describe(&Entry::parse(line).expect(line));
        (d.title, d.target, d.who, d.status.join(" · "))
    }

    fn details_of(line: &str) -> String {
        describe(&Entry::parse(line).expect(line))
            .details
            .join(" · ")
    }

    #[test]
    fn levels_three_and_four_lines() {
        let agent =
            r#""time":1795082462,"agent":"/usr/bin/node /home/me/agent.js","name":"Example""#;
        let line = |rest: &str| format!("{{{agent},{rest}}}");
        // The contract's examples.
        let ask = line(
            r#""method":"Ask","level":"read-only","outcome":"allowed","question-length":29,"choice-count":2"#,
        );
        assert_eq!(
            shown(&ask),
            four("Asked you a question", "", "Example", "Allowed")
        );
        assert_eq!(details_of(&ask), "2 choices", "no question length");
        let add = line(
            r#""method":"AddReminder","target":"reminder:3f9a0c2e7b1d4a56","level":"read-only","outcome":"allowed","text-length":16"#,
        );
        assert_eq!(
            shown(&add),
            four("Added a reminder", "", "Example", "Allowed")
        );
        assert_eq!(details_of(&add), "16 characters");
        let due =
            line(r#""method":"Reminder","target":"reminder:3f9a0c2e7b1d4a56","outcome":"allowed""#);
        assert_eq!(shown(&due), four("Reminder due", "", "Example", "Shown"));
        assert_eq!(
            shown(&line(
                r#""method":"Reminder","target":"reminder:1","outcome":"refused:AccessOff""#
            ))
            .3,
            "Not shown · Agent Access was off"
        );
        assert_eq!(
            shown(&line(
                r#""method":"Reminder","target":"reminder:1","outcome":"cancelled""#
            ))
            .3,
            "Cancelled by you"
        );
        let tree = line(
            r#""method":"A11yTree","target":"window:21 (org.gnome.TextEditor)","level":"apps","outcome":"allowed","node-count":143"#,
        );
        assert_eq!(
            shown(&tree),
            four(
                "Read an app’s contents",
                "A window of org.gnome.TextEditor",
                "Example",
                "Allowed"
            )
        );
        assert_eq!(details_of(&tree), "143 items");
        let set = line(
            r#""method":"A11ySetValue","target":"window:21 (org.gnome.TextEditor)","level":"apps","outcome":"allowed","text-length":17,"role":"text","action":"set-text""#,
        );
        assert_eq!(details_of(&set), "Text field · Set text · 17 characters");
        let password = line(
            r#""method":"A11ySetValue","target":"window:21 (org.gnome.TextEditor)","level":"apps","outcome":"refused:Excluded","text-length":5,"role":"password text""#,
        );
        assert_eq!(shown(&password).3, "Refused · Excluded app");
        assert_eq!(details_of(&password), "Password field · 5 characters");
        // A11yFind: the role looked for and how many matched.
        let find = line(
            r#""method":"A11yFind","target":"window:21","level":"apps","outcome":"allowed","role":"button","name-length":4,"node-count":1"#,
        );
        assert_eq!(
            shown(&find),
            four(
                "Searched an app’s controls",
                "A window",
                "Example",
                "Allowed"
            )
        );
        assert_eq!(details_of(&find), "Button · 1 match");
        assert_eq!(
            details_of(&line(
                r#""method":"A11yFind","outcome":"allowed","node-count":0"#
            )),
            "0 matches"
        );
        // Actions: known ones in words, `other` too, unknown ones raw;
        // roles this version does not name, raw.
        let act = |action: &str, role: &str| {
            details_of(&line(&format!(
                r#""method":"A11yAction","target":"window:3","level":"apps","outcome":"allowed","action":"{action}","role":"{role}""#
            )))
        };
        assert_eq!(act("click", "button"), "Button · Clicked");
        assert_eq!(act("clipboard.paste", "text"), "Text field · Pasted");
        assert_eq!(act("window.close", "frame"), "Window · Closed the window");
        assert_eq!(
            act("other", "toggle button"),
            "Toggle button · Another action"
        );
        assert_eq!(act("later", "tree table"), "tree table · later");
        // Media players: the action in words, the player by its app.
        let media = line(
            r#""method":"MediaControl","target":"player:vlc (org.videolan.VLC)","level":"desktop","outcome":"allowed","action":"play-pause""#,
        );
        assert_eq!(
            shown(&media),
            four(
                "Controlled a media player",
                "A media player of org.videolan.VLC",
                "Example",
                "Allowed"
            )
        );
        assert_eq!(details_of(&media), "Played or paused");
        let seek = line(
            r#""method":"MediaControl","target":"player:firefox.instance_1_42","level":"desktop","outcome":"refused:Excluded","reason":"app","action":"seek""#,
        );
        assert_eq!(shown(&seek).1, "Media player “firefox.instance_1_42”");
        assert_eq!(details_of(&seek), "Moved in the track");
        for action in [
            "play",
            "pause",
            "play-pause",
            "next",
            "previous",
            "stop",
            "seek",
        ] {
            assert!(media_action_label(action).is_some(), "{action}");
        }
        assert_eq!(media_action_label("later"), None);
        assert_eq!(method_label("GetMedia"), Some("Read the media players"));
        // Other methods' actions are not repeated.
        assert_eq!(
            details_of(&line(
                r#""method":"WindowAction","target":"window:21","level":"desktop","outcome":"allowed","action":"tile-left""#
            )),
            ""
        );
        for role in [
            "button",
            "text",
            "entry",
            "password text",
            "check box",
            "link",
            "window",
        ] {
            assert!(role_label(role).is_some(), "{role}");
        }
        for action in [
            "click",
            "press",
            "activate",
            "toggle",
            "select",
            "focus",
            "default",
            "expand",
            "collapse",
            "open",
            "jump",
            "menu",
            "showmenu",
            "increment",
            "decrement",
            "window.close",
            "window.minimize",
            "window.toggle-maximized",
            "default.activate",
            "menu.popup",
            "selection.select-all",
            "selection.delete",
            "text.clear",
            "clipboard.copy",
            "clipboard.cut",
            "clipboard.paste",
            "link.open",
            "link.copy",
            "misc.insert-emoji",
            "other",
            "set-text",
            "set-value",
        ] {
            assert!(control_action_label(action).is_some(), "{action}");
        }

        // Settings' own reminder calls, and another program trying.
        let settings = r#"{"time":1795082470,"agent":"/usr/bin/helix-settings-app","name":"","#;
        assert_eq!(
            shown(&format!(
                r#"{settings}"method":"CancelReminder","target":"reminder:3f9a","outcome":"allowed"}}"#
            )),
            four("Reminder cancelled", "", "Settings", "")
        );
        assert_eq!(
            shown(&line(
                r#""method":"ListReminders","outcome":"refused:AccessDenied""#
            )),
            four(
                "Tried to list reminders",
                "",
                "Example",
                "Refused · Only Settings may do this"
            )
        );
        assert_eq!(
            shown(&line(
                r#""method":"CancelReminder","outcome":"refused:AccessDenied""#
            ))
            .0,
            "Tried to cancel a reminder"
        );
        // An agent's own reminder calls (Agent1, with a level).
        assert_eq!(
            shown(&line(
                r#""method":"CancelReminder","target":"reminder:3f9a","level":"read-only","outcome":"allowed""#
            )),
            four("Cancelled a reminder", "", "Example", "Allowed")
        );
        assert_eq!(
            shown(&line(
                r#""method":"ListReminders","level":"read-only","outcome":"refused:AccessOff""#
            )),
            four(
                "Listed reminders",
                "",
                "Example",
                "Refused · Agent Access was off"
            )
        );
    }

    #[test]
    fn clipboard_lines() {
        let agent =
            r#""time":1795082470,"agent":"/usr/bin/node /home/me/agent.js","name":"Example""#;
        let line = |rest: &str| format!("{{{agent},{rest}}}");
        // Read: the kind, the size and where it came from; never the text.
        let get = line(
            r#""method":"GetClipboard","target":"org.gnome.TextEditor","level":"read-only","outcome":"allowed","mime-type":"text/plain","data-length":12345"#,
        );
        assert_eq!(
            shown(&get),
            four(
                "Read the clipboard",
                "Copied from org.gnome.TextEditor",
                "Example",
                "Allowed"
            )
        );
        // GLib's size, in GLib's words ("12.3 kB").
        let size = |n: u64| gtk::glib::format_size(n).to_string();
        assert!(size(12345).starts_with("12.3"));
        assert_eq!(details_of(&get), format!("Text · {}", size(12345)));
        // From an app whose window is not known: no target.
        let unknown = line(
            r#""method":"GetClipboard","level":"read-only","outcome":"allowed","mime-type":"image/png","data-length":1"#,
        );
        assert_eq!(shown(&unknown).1, "");
        assert_eq!(details_of(&unknown), format!("Picture · {}", size(1)));
        // Set: what the agent sent.
        let set = line(
            r#""method":"SetClipboard","level":"desktop","outcome":"allowed","mime-type":"text/html","data-length":2048"#,
        );
        assert_eq!(
            shown(&set),
            four("Changed the clipboard", "", "Example", "Allowed")
        );
        assert_eq!(details_of(&set), format!("HTML · {}", size(2048)));
        for (kind, word) in [
            ("text/uri-list", "Links"),
            ("image/jpeg", "Picture"),
            ("image/gif", "Picture"),
            ("image/svg+xml", "Picture"),
            ("application/pdf", "PDF document"),
            ("x-kde-passwordManagerHint", "Password manager hint"),
            ("other", "Other"),
        ] {
            let l = line(&format!(
                r#""method":"GetClipboard","level":"read-only","outcome":"allowed","mime-type":"{kind}""#
            ));
            assert_eq!(details_of(&l), word, "{kind}");
        }
        // A kind this version does not know shows as the log has it.
        assert_eq!(
            details_of(&line(
                r#""method":"GetClipboard","level":"read-only","outcome":"allowed","mime-type":"text/x-later""#
            )),
            "text/x-later"
        );
        // Refusals: the switch, a protected source, a system prompt.
        assert_eq!(
            shown(&line(
                r#""method":"GetClipboard","level":"read-only","outcome":"refused:FeatureOff""#
            ))
            .3,
            "Refused · Clipboard access was off"
        );
        let excluded = line(
            r#""method":"GetClipboard","target":"org.keepassxc.KeePassXC","level":"read-only","outcome":"refused:Excluded","mime-type":"text/plain""#,
        );
        assert_eq!(
            shown(&excluded),
            four(
                "Read the clipboard",
                "Copied from org.keepassxc.KeePassXC",
                "Example",
                "Refused · Copied from a protected app, or a password"
            )
        );
        assert_eq!(
            shown(&line(
                r#""method":"SetClipboard","level":"desktop","outcome":"refused:Excluded","mime-type":"text/plain","data-length":5"#
            ))
            .3,
            "Refused · A system prompt was open"
        );
        // Failed after the check: the clipboard changed meanwhile.
        assert_eq!(
            shown(&line(
                r#""method":"GetClipboard","level":"read-only","outcome":"allowed","error":"Failed","mime-type":"text/plain""#
            ))
            .3,
            "Allowed · Failed"
        );
        // Other methods' refusals keep their reasons.
        assert_eq!(
            method_refusal_label("A11yAction", "Excluded"),
            Some("Excluded app")
        );
    }

    #[test]
    fn screen_and_input_lines() {
        let agent =
            r#""time":1795082480,"agent":"/usr/bin/node /home/me/agent.js","name":"Example""#;
        let line = |rest: &str| format!("{{{agent},{rest}}}");
        // The shell's own test lines (helix-shell agent_screen_bus).
        let screen = line(
            r#""method":"Screenshot","target":"screen","level":"screen-and-input","outcome":"allowed","scope":"screen","width":2560,"height":720,"filled-count":3"#,
        );
        assert_eq!(
            shown(&screen),
            four(
                "Took a screenshot",
                "The whole screen",
                "Example",
                "Allowed"
            )
        );
        assert_eq!(details_of(&screen), "2560 × 720 pixels · 3 areas hidden");
        let monitor = line(
            r#""method":"Screenshot","target":"monitor:HDMI-A-1","level":"screen-and-input","outcome":"allowed","scope":"monitor","width":1920,"height":1080,"filled-count":1"#,
        );
        assert_eq!(shown(&monitor).1, "Display HDMI-A-1");
        assert_eq!(details_of(&monitor), "1920 × 1080 pixels · 1 area hidden");
        let window = line(
            r#""method":"Screenshot","target":"window:11 (org.gnome.TextEditor)","level":"screen-and-input","outcome":"allowed","scope":"window","width":800,"height":600,"filled-count":0"#,
        );
        assert_eq!(shown(&window).1, "A window of org.gnome.TextEditor");
        assert_eq!(details_of(&window), "800 × 600 pixels · Nothing hidden");
        // No target: the scope says what.
        for (scope, said) in [
            ("screen", "The whole screen"),
            ("monitor", "A display"),
            ("window", "A window"),
        ] {
            let l = line(&format!(
                r#""method":"Screenshot","level":"screen-and-input","outcome":"allowed","scope":"{scope}""#
            ));
            assert_eq!(shown(&l).1, said, "{scope}");
        }
        let protected = line(
            r#""method":"Screenshot","target":"window:12 (org.example.Vault)","level":"screen-and-input","outcome":"refused:Excluded","scope":"window""#,
        );
        assert_eq!(
            shown(&protected).3,
            "Refused · A protected window or a system prompt"
        );
        assert_eq!(details_of(&protected), "");

        // Input: counts by kind, never keys, text or positions.
        let input = line(
            r#""method":"Input","target":"window:11 (org.gnome.TextEditor)","level":"screen-and-input","outcome":"allowed","event-count":5,"click-count":1,"scroll-count":1,"shortcut-count":1,"text-length":10,"done-count":10"#,
        );
        assert_eq!(
            shown(&input),
            four(
                "Typed or clicked",
                "A window of org.gnome.TextEditor",
                "Example",
                "Allowed"
            )
        );
        assert_eq!(
            details_of(&input),
            "5 actions · 1 click · 1 shortcut · 1 scroll · 10 characters typed",
            "finished: no step count"
        );
        let every = line(
            r#""method":"Input","target":"window:11","level":"screen-and-input","outcome":"allowed","event-count":8,"move-count":1,"click-count":2,"button-count":1,"scroll-count":1,"key-count":2,"shortcut-count":1,"text-length":6,"done-count":14"#,
        );
        assert_eq!(
            details_of(&every),
            "8 actions · 2 clicks · 1 shortcut · 2 keys · 1 pointer move · 1 scroll · \
             1 button press or release · 6 characters typed"
        );
        // Stopped by the user's input or Stop, part way.
        let stopped = line(
            r#""method":"Input","target":"window:11 (org.gnome.TextEditor)","level":"screen-and-input","outcome":"cancelled","event-count":1,"text-length":42,"done-count":2"#,
        );
        assert_eq!(shown(&stopped).3, "Stopped by you");
        assert_eq!(
            details_of(&stopped),
            "1 action · 42 characters typed · Stopped after 2 steps"
        );
        // Refused during the cooldown: nothing carried out.
        let cooldown = line(
            r#""method":"Input","target":"window:11","level":"screen-and-input","outcome":"cancelled","event-count":1,"click-count":1,"done-count":0"#,
        );
        assert_eq!(shown(&cooldown).3, "Stopped by you");
        assert_eq!(
            details_of(&cooldown),
            "1 action · 1 click · Stopped before the first step"
        );
        // A sensitive prompt left unanswered: never ran, no done-count.
        let unanswered = line(
            r#""method":"Input","target":"window:11","level":"screen-and-input","outcome":"cancelled","event-count":1,"key-count":1"#,
        );
        assert_eq!(shown(&unanswered).3, "Cancelled");
        assert_eq!(details_of(&unanswered), "1 action · 1 key");
        // A password field took the focus part way.
        let password = line(
            r#""method":"Input","target":"window:11 (org.gnome.TextEditor)","level":"screen-and-input","outcome":"refused:Excluded","event-count":3,"click-count":1,"text-length":8,"done-count":2"#,
        );
        assert_eq!(
            shown(&password).3,
            "Refused · A protected window, a password field or a system prompt"
        );
        assert_eq!(
            details_of(&password),
            "3 actions · 1 click · 8 characters typed · Stopped after 2 steps"
        );
        // The window lost the focus: allowed, then failed part way.
        let lost = line(
            r#""method":"Input","target":"window:11","level":"screen-and-input","outcome":"allowed","error":"Failed","event-count":2,"key-count":2,"done-count":1"#,
        );
        assert_eq!(shown(&lost).3, "Allowed · Failed");
        assert_eq!(
            details_of(&lost),
            "2 actions · 2 keys · Stopped after 1 step"
        );
        // Refused before it began: no done-count, nothing about steps.
        assert_eq!(
            shown(&line(
                r#""method":"Input","level":"screen-and-input","outcome":"refused:LevelTooLow""#
            ))
            .3,
            "Refused · Needs a higher access level"
        );
    }

    #[test]
    fn reasons() {
        let agent =
            r#""time":1795082480,"agent":"/usr/bin/node /home/me/agent.js","name":"Example""#;
        let line = |rest: &str| format!("{{{agent},{rest}}}");
        let parsed = Entry::parse(&line(
            r#""method":"Input","outcome":"cancelled","reason":"user-input""#,
        ))
        .unwrap();
        assert_eq!(parsed.reason, "user-input");
        // Every token, refused (Excluded, FeatureOff) or cancelled (Input).
        for (method, outcome, reason, said) in [
            (
                "Input",
                "refused:Excluded",
                "password-field",
                "Refused · A password field",
            ),
            (
                "A11ySetValue",
                "refused:Excluded",
                "password-field",
                "Refused · A password field",
            ),
            (
                "GetClipboard",
                "refused:Excluded",
                "password-field",
                "Refused · A password manager marked it secret",
            ),
            (
                "Screenshot",
                "refused:Excluded",
                "system-prompt",
                "Refused · A system prompt was open",
            ),
            (
                "A11yTree",
                "refused:Excluded",
                "shell",
                "Refused · Part of the desktop",
            ),
            (
                "GetClipboard",
                "refused:Excluded",
                "shell",
                "Refused · Part of the desktop",
            ),
            (
                "WindowAction",
                "refused:Excluded",
                "settings",
                "Refused · Settings",
            ),
            (
                "Input",
                "refused:Excluded",
                "app",
                "Refused · An excluded app",
            ),
            (
                "GetClipboard",
                "refused:Excluded",
                "app",
                "Refused · An excluded app",
            ),
            (
                "A11yAction",
                "refused:FeatureOff",
                "clipboard",
                "Refused · Clipboard access was off",
            ),
            (
                "Input",
                "cancelled",
                "user-input",
                "Stopped: you used the keyboard or pointer",
            ),
            (
                "Input",
                "cancelled",
                "cooldown",
                "Refused: you had just used the keyboard or pointer",
            ),
            (
                "Input",
                "cancelled",
                "stopped",
                "Stopped from the indicator or Settings",
            ),
        ] {
            let l = line(&format!(
                r#""method":"{method}","target":"window:11","level":"screen-and-input","outcome":"{outcome}","reason":"{reason}","done-count":2"#
            ));
            assert_eq!(shown(&l).3, said, "{method} {reason}");
            assert!(reason_label(method, reason).is_some(), "{reason}");
        }
        // A coalesced refusal keeps its count.
        assert_eq!(
            shown(&line(
                r#""method":"ListWindows","target":"org.example.Vault","level":"read-only","outcome":"refused:Excluded","reason":"app","count":3,"until":1795082490"#
            ))
            .3,
            "Refused 3 times · An excluded app"
        );
        // No reason (older lines) or one this version does not know: what
        // the method and error say.
        for reason in ["", r#","reason":"later""#] {
            let input = |outcome: &str, more: &str| {
                shown(&line(&format!(
                    r#""method":"Input","level":"screen-and-input","outcome":"{outcome}"{reason}{more}"#
                )))
                .3
            };
            assert_eq!(
                input("refused:Excluded", ""),
                "Refused · A protected window, a password field or a system prompt"
            );
            assert_eq!(input("cancelled", r#","done-count":2"#), "Stopped by you");
            assert_eq!(input("cancelled", ""), "Cancelled");
            assert_eq!(
                shown(&line(&format!(
                    r#""method":"GetClipboard","level":"read-only","outcome":"refused:Excluded"{reason}"#
                )))
                .3,
                "Refused · Copied from a protected app, or a password"
            );
            assert_eq!(
                shown(&line(&format!(
                    r#""method":"ListWindows","level":"read-only","outcome":"refused:Excluded"{reason}"#
                )))
                .3,
                "Refused · Excluded app"
            );
        }
        assert_eq!(reason_label("Input", "later"), None);
        assert_eq!(reason_label("Input", ""), None);
        // A reason never turns an allowed call into anything else.
        assert_eq!(
            shown(&line(
                r#""method":"Input","level":"screen-and-input","outcome":"allowed","reason":"app""#
            ))
            .3,
            "Allowed"
        );
    }

    fn four(a: &str, b: &str, c: &str, d: &str) -> (String, String, String, String) {
        (a.into(), b.into(), c.into(), d.into())
    }

    #[test]
    fn the_contracts_example_lines() {
        let e = Entry::parse(r#"{"time":1795082400,"agent":"/usr/bin/node /home/me/agent.js","name":"Example","method":"ListWindows","target":"org.gnome.TextEditor","level":"read-only","outcome":"allowed"}"#).unwrap();
        assert_eq!(
            (e.time, e.level.as_str(), e.count),
            (1795082400, "read-only", 0)
        );
        assert_eq!(e.coalesced(), None);
        assert_eq!(
            describe(&e),
            Described {
                title: "Listed windows".into(),
                target: "org.gnome.TextEditor".into(),
                who: "Example".into(),
                status: vec!["Allowed".into()],
                details: Vec::new(),
            }
        );
        // Lengths are not shown.
        assert_eq!(
            shown(
                r#"{"time":1795082410,"agent":"/usr/bin/node /home/me/agent.js","name":"Example","method":"SearchFiles","target":"file:///home/me","level":"read-only","outcome":"allowed","query-length":6}"#
            ),
            four("Searched files", "/home/me", "Example", "Allowed")
        );
        assert_eq!(
            shown(
                r#"{"time":1795082420,"agent":"/usr/bin/node /home/me/agent.js","name":"Example","method":"WaitForWindowClosed","target":"window:21 (org.example.Vault)","level":"read-only","outcome":"refused:Excluded"}"#
            ),
            four(
                "Waited for a window to close",
                "A window of org.example.Vault",
                "Example",
                "Refused · Excluded app"
            )
        );
        assert_eq!(
            shown(
                r#"{"time":1795082430,"agent":"/usr/bin/node /home/me/agent.js","name":"Example","method":"WaitForWindow","level":"read-only","outcome":"allowed","error":"TimedOut","title-length":5}"#
            ),
            four(
                "Waited for a window",
                "",
                "Example",
                "Allowed · Failed: timed out"
            )
        );
        assert_eq!(
            shown(
                r#"{"time":1795082440,"agent":"/usr/bin/node /home/me/agent.js","name":"Example","method":"ConsentPrompt","level":"read-only","outcome":"allowed","grant":"session"}"#
            ),
            four(
                "Asked for access",
                "",
                "Example",
                "Read Only · Allowed for this session"
            )
        );
        let line = r#"{"time":1795082501,"agent":"/opt/bot","name":"","method":"GetState","level":"read-only","outcome":"refused:AccessOff","count":120,"until":1795082559}"#;
        assert_eq!(
            Entry::parse(line).unwrap().coalesced(),
            Some((120, 1795082559))
        );
        assert_eq!(
            shown(line),
            four(
                "Read the desktop state",
                "",
                "/opt/bot",
                "Refused 120 times · Agent Access was off"
            )
        );
    }

    /// The scoped actions' changes (helixdesktop/desktop#157): what changed
    /// from what to what, until when, why it was put back or not, and
    /// Settings' Undo.
    #[test]
    fn change_lines() {
        let full = |line: &str| {
            let d = describe(&Entry::parse(line).expect(line));
            (
                d.title,
                d.target,
                d.details.join(" · "),
                d.status.join(" · "),
            )
        };
        let four = |a: &str, b: &str, c: &str, d: &str| {
            (a.to_string(), b.to_string(), c.to_string(), d.to_string())
        };
        let until = gtk::glib::DateTime::from_unix_local(1795086200)
            .unwrap()
            .format("%H:%M")
            .unwrap()
            .to_string();
        assert_eq!(
            full(
                r#"{"time":1795082600,"agent":"/usr/bin/node /home/me/agent.js","name":"Example","method":"SetAccent","target":"accent","level":"desktop","outcome":"allowed","change":"9c1d0e2f4a6b8c3d","old":"blue","new":"red","restore-at":1795086200}"#
            ),
            four(
                "Changed the accent color",
                "Accent color",
                &format!("Blue → Red · Until {until}"),
                "Allowed"
            )
        );
        assert_eq!(
            full(
                r#"{"time":1795082610,"agent":"/usr/bin/node /home/me/agent.js","name":"Example","method":"SetToggle","target":"dark-style","level":"desktop","outcome":"allowed","state":"on","change":"5e7f9a1b3c2d4e6f","old":"off","new":"on"}"#
            ),
            four(
                "Turned a setting on or off",
                "Dark style",
                "Off → On",
                "Allowed"
            )
        );
        assert_eq!(
            full(
                r#"{"time":1795082620,"agent":"/usr/bin/node /home/me/agent.js","name":"Example","method":"SetWallpaper","target":"wallpaper","level":"desktop","outcome":"refused:Excluded","reason":"private-file","variant":"both"}"#
            ),
            four(
                "Changed the background",
                "Background",
                "",
                "Refused · A file in a hidden or system folder"
            )
        );
        assert_eq!(
            full(r#"{"time":1,"agent":"/opt/bot","name":"","method":"SetWallpaper","target":"wallpaper","level":"desktop","outcome":"allowed","variant":"dark","change":"1","old":"","new":"file:///home/me/.local/share/backgrounds/2026-11-20-10-00-00-sea.png","old-dark":"","new-dark":"file:///home/me/.local/share/backgrounds/2026-11-20-10-00-00-sea.png"}"#).2,
            "None → 2026-11-20-10-00-00-sea.png · Dark style only"
        );
        assert_eq!(
            full(
                r#"{"time":1795082630,"agent":"/usr/bin/node /home/me/agent.js","name":"Example","method":"NotificationAction","target":"notification:42 (org.gnome.Fractal)","level":"desktop","outcome":"refused:FeatureOff","reason":"notifications","action":"button"}"#
            ),
            four(
                "Pressed a notification button",
                "A notification of org.gnome.Fractal",
                "One of its buttons",
                "Refused · Notification access was off"
            )
        );
        assert_eq!(
            full(
                r#"{"time":1795082650,"agent":"/usr/bin/helix-settings-app","name":"","method":"Undo","target":"dark-style","outcome":"allowed","change":"5e7f9a1b3c2d4e6f","old":"on","new":"off"}"#
            ),
            four("Change undone", "Dark style", "On → Off", "")
        );
        assert_eq!(
            full(r#"{"time":1795082650,"agent":"/usr/bin/helix-settings-app","name":"","method":"Undo","target":"accent","outcome":"allowed","change":"5e7f9a1b3c2d4e6f","old":"red","new":"blue","error":"Failed"}"#).3,
            "Not undone"
        );
        assert_eq!(
            full(
                r#"{"time":1795086200,"agent":"/usr/bin/node /home/me/agent.js","name":"Example","method":"Restore","target":"accent","outcome":"allowed","change":"9c1d0e2f4a6b8c3d","old":"red","new":"blue","cause":"expired"}"#
            ),
            four(
                "Put a temporary change back",
                "Accent color",
                "Red → Blue · Its time was up",
                "Put back"
            )
        );
        assert_eq!(
            full(
                r#"{"time":1,"agent":"/opt/bot","name":"Bot","method":"Restore","target":"power-mode","outcome":"cancelled","reason":"changed","change":"2","old":"performance","new":"balanced","cause":"stopped"}"#
            ),
            four(
                "Put a temporary change back",
                "Power mode",
                "Performance → Balanced · The agent was stopped",
                "Not put back: it was changed since"
            )
        );
        assert_eq!(
            full(
                r#"{"time":1,"agent":"/opt/bot","name":"Bot","method":"StarFile","target":"file:///home/me/report.pdf","level":"desktop","outcome":"allowed","change":"3","old":"off","new":"on"}"#
            ),
            four(
                "Starred or unstarred a file",
                "/home/me/report.pdf",
                "Not starred → Starred",
                "Allowed"
            )
        );
        assert_eq!(
            full(r#"{"time":1,"agent":"/opt/bot","name":"Bot","method":"SetFavorite","target":"org.gnome.Maps","level":"desktop","outcome":"allowed","old":"on","new":"on"}"#).2,
            "Already In the dock"
        );
        assert_eq!(
            full(r#"{"time":1,"agent":"/opt/bot","name":"Bot","method":"Undo","target":"org.gnome.Maps","outcome":"allowed","change":"4","old":"on","new":"off"}"#).2,
            "In the dock → Not in the dock"
        );
        // Offered Undo while ListChanges lists it.
        let e = Entry::parse(r#"{"time":1,"agent":"/opt/bot","name":"Bot","method":"SetAccent","target":"accent","level":"desktop","outcome":"allowed","change":"abc","old":"blue","new":"red"}"#).unwrap();
        assert!(undoable(&e, &["abc".into()]));
        assert!(!undoable(&e, &[]));
        let restore = Entry {
            method: "Restore".into(),
            ..e.clone()
        };
        assert!(
            !undoable(&restore, &["abc".into()]),
            "only the change's own line"
        );
        let failed = Entry {
            error: "Failed".into(),
            ..e
        };
        assert!(!undoable(&failed, &["abc".into()]));
    }

    #[test]
    fn other_line_shapes() {
        let line =
            |rest: &str| format!(r#"{{"time":1795082400,"agent":"/opt/bot","name":"Bot",{rest}}}"#);
        // Consent prompts.
        assert_eq!(
            shown(&line(
                r#""method":"ConsentPrompt","level":"desktop","outcome":"allowed","grant":"always""#
            ))
            .3,
            "Control the Desktop · Always allowed"
        );
        assert_eq!(
            shown(&line(
                r#""method":"ConsentPrompt","level":"read-only","outcome":"refused:DeniedByUser""#
            ))
            .3,
            "Read Only · Denied"
        );
        assert_eq!(
            shown(&line(r#""method":"ConsentPrompt","outcome":"cancelled""#)).3,
            "Not answered"
        );
        // Sensitive-action prompts name the method by what it does.
        assert_eq!(
            shown(&line(
                r#""method":"SensitivePrompt","target":"Open","outcome":"refused:DeniedByUser""#
            )),
            four(
                "Asked to confirm an action",
                "Opened a file or link",
                "Bot",
                "Denied"
            )
        );
        assert_eq!(
            shown(&line(
                r#""method":"SensitivePrompt","target":"Later","outcome":"allowed""#
            ))
            .1,
            "Later"
        );
        assert_eq!(
            shown(&line(
                r#""method":"Stop","level":"apps","outcome":"cancelled""#
            )),
            four("Stopped by you", "", "Bot", "")
        );
        assert_eq!(
            shown(
                r#"{"time":1795082400,"agent":"","name":"","method":"LogFull","outcome":"allowed","dropped":"refusals"}"#
            ),
            four(
                "Activity log full",
                "",
                "",
                "Refused calls are no longer recorded today"
            )
        );
        assert_eq!(
            shown(r#"{"time":1795082400,"agent":"","name":"","method":"LogFull","outcome":"allowed","dropped":"all"}"#).3,
            "Nothing more is recorded today"
        );
        // Settings' own calls.
        let settings = r#"{"time":1795082400,"agent":"/usr/bin/helix-settings-app","name":"","#;
        assert_eq!(
            shown(&format!(
                r#"{settings}"method":"ClearLog","outcome":"allowed"}}"#
            )),
            four("Activity log cleared", "", "Settings", "")
        );
        assert_eq!(
            shown(&format!(
                r#"{settings}"method":"Revoke","target":"/opt/bot","outcome":"allowed"}}"#
            )),
            four("Access revoked", "/opt/bot", "Settings", "")
        );
        assert_eq!(
            shown(&format!(
                r#"{settings}"method":"Forget","target":"/opt/bot","outcome":"allowed"}}"#
            ))
            .0,
            "Access revoked and forgotten"
        );
        assert_eq!(
            shown(&format!(
                r#"{settings}"method":"StopAll","outcome":"allowed"}}"#
            ))
            .0,
            "All access revoked"
        );
        // Another program trying.
        assert_eq!(
            shown(&line(
                r#""method":"ClearLog","outcome":"refused:AccessDenied""#
            )),
            four(
                "Tried to clear the activity log",
                "",
                "Bot",
                "Refused · Only Settings may do this"
            )
        );
        // Failures and refusals this version does not know, unknown
        // methods and outcomes: the raw names.
        assert_eq!(
            shown(&line(
                r#""method":"Open","target":"file:///tmp/a%20b","outcome":"allowed","error":"Busy""#
            )),
            four(
                "Opened a file or link",
                "/tmp/a b",
                "Bot",
                "Allowed · Failed: Busy"
            )
        );
        assert_eq!(
            shown(&line(
                r#""method":"Teleport","outcome":"refused:TooFar","count":1"#
            )),
            four("Teleport", "", "Bot", "Refused · TooFar")
        );
        assert_eq!(
            shown(&line(r#""method":"Open","outcome":"refused""#)).3,
            "Refused"
        );
        assert_eq!(
            shown(&line(r#""method":"Open","outcome":"maybe""#)).3,
            "maybe"
        );
        assert_eq!(
            shown(&line(r#""method":"Ask","outcome":"cancelled""#)).3,
            "Cancelled"
        );
        assert_eq!(
            shown(&line(
                r#""method":"WindowAction","target":"window:7","outcome":"allowed""#
            ))
            .1,
            "A window"
        );
        // No name: the agent id.
        assert_eq!(
            shown(r#"{"time":1,"agent":"/usr/bin/foot","method":"ListApps","outcome":"allowed"}"#)
                .2,
            "/usr/bin/foot"
        );
        // Tolerance: other types, unknown keys, ISO times.
        let e = Entry::parse(r#"{"time":"2026-11-19T10:00:00Z","method":7,"count":"x","until":"2026-11-19T10:00:30Z","new":{"a":1}}"#).unwrap();
        assert_eq!((e.time, e.until, e.count), (1795082400, 1795082430, 0));
        assert_eq!(e.method, "");
        assert_eq!(Entry::parse("[1,2]"), None);
        assert_eq!(Entry::parse("{\"time\":1"), None);
        // A coalesced line's end is never before its start.
        let e = Entry::parse(r#"{"time":100,"count":3}"#).unwrap();
        assert_eq!(e.coalesced(), Some((3, 100)));
    }

    #[test]
    fn reminders_from_records() {
        let r = Reminder::from_record(&record(&[
            ("id", Value::from("3f9a0c2e7b1d4a56")),
            ("time", Value::from(1795084200i64)),
            ("text", Value::from("Stretch")),
            ("agent", Value::from("/usr/bin/node /home/me/agent.js")),
            ("executable", Value::from("/usr/bin/node")),
            ("name", Value::from("Example")),
            ("future-key", Value::from(7u32)),
        ]))
        .unwrap();
        assert_eq!((r.time, r.text.as_str()), (1795084200, "Stretch"));
        assert_eq!(r.who().title(), "Example");
        assert_eq!(r.who().program(), "/usr/bin/node /home/me/agent.js");
        let bare = Reminder::from_record(&record(&[("id", Value::from("1"))])).unwrap();
        assert_eq!((bare.time, bare.text.as_str()), (0, ""));
        assert_eq!(bare.who().title(), "", "no agent: nothing made up");
        assert_eq!(
            Reminder::from_record(&record(&[("text", Value::from("x"))])),
            None,
            "no id"
        );
    }

    #[test]
    fn times() {
        assert_eq!(ago(100, 0), None);
        assert_eq!(ago(100, 90), Some((0, 's')));
        assert_eq!(ago(1000, 100), Some((15, 'm')));
        assert_eq!(ago(10_000, 100), Some((2, 'h')));
        assert_eq!(ago(300_000, 100), Some((3, 'd')));
        assert_eq!(ago(100, 200), Some((0, 's')), "clock skew");
    }

    #[test]
    fn excluded_apps_list() {
        let list = vec!["org.example.Bank".to_string()];
        assert_eq!(with_excluded(&list, "org.example.Bank.desktop"), None);
        assert_eq!(with_excluded(&list, ""), None);
        assert_eq!(
            with_excluded(&list, "org.gnome.Maps.desktop").unwrap(),
            ["org.example.Bank", "org.gnome.Maps"]
        );
        let dotted = vec!["org.example.Bank.desktop".to_string()];
        assert_eq!(with_excluded(&dotted, "org.example.Bank"), None);
        assert!(without_excluded(&dotted, "org.example.Bank").is_empty());
        assert_eq!(without_excluded(&list, "x"), list);
    }

    #[test]
    fn log_is_read_newest_first() {
        let dir = std::env::temp_dir().join(format!("helix-agent-log-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(read_log(&dir, 10), (Vec::new(), false), "no directory");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("2026-11-18.jsonl"),
            "{\"time\":1795000000,\"agent\":\"/usr/bin/a\",\"name\":\"A\",\"method\":\"GetState\",\"outcome\":\"allowed\"}\n",
        )
        .unwrap();
        std::fs::write(
            dir.join("2026-11-19.jsonl"),
            "{\"time\":1795082400,\"agent\":\"/usr/bin/a\",\"name\":\"A\",\"method\":\"ListWindows\",\"level\":\"read-only\",\"outcome\":\"allowed\"}\n\
             not json\n\
             \n\
             {\"time\":\"2026-11-19T10:01:40Z\",\"agent\":\"/usr/bin/b\",\"method\":\"Open\",\"target\":\"file:///tmp/x\",\"outcome\":\"refused:LevelTooLow\"}\n",
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let (all, more) = read_log(&dir, 10);
        assert!(!more);
        let methods: Vec<&str> = all.iter().map(|e| e.method.as_str()).collect();
        assert_eq!(methods, ["Open", "ListWindows", "GetState"]);
        assert_eq!(all[0].time, 1795082500);
        assert_eq!(all[0].who(), "/usr/bin/b");
        assert_eq!(all[0].target, "file:///tmp/x");
        assert_eq!(all[1].who(), "A");
        assert_eq!(all[1].level, "read-only");
        let (two, more) = read_log(&dir, 2);
        assert_eq!(two.len(), 2);
        assert!(more);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The shell's side, as the contract describes it (without its
    /// caller check).
    pub struct FakeShell {
        pub agents: Arc<Mutex<Vec<Agent>>>,
        pub reminders: Arc<Mutex<Vec<Reminder>>>,
        pub calls: Arc<Mutex<Vec<String>>>,
        /// The changes `ListChanges` lists, by id.
        pub changes: Arc<Mutex<Vec<String>>>,
    }

    /// `Undo`'s errors, as the shell names them.
    #[derive(Debug, zbus::DBusError)]
    #[zbus(prefix = "org.helixdesktop.Agent1.Error")]
    pub enum UndoFailure {
        #[zbus(error)]
        ZBus(zbus::Error),
        NotFound(String),
        Failed(String),
    }

    fn reminder_record(r: &Reminder) -> HashMap<String, OwnedValue> {
        record(&[
            ("id", Value::from(r.id.as_str())),
            ("time", Value::from(r.time)),
            ("text", Value::from(r.text.as_str())),
            ("agent", Value::from(r.agent.as_str())),
            ("executable", Value::from(r.executable.as_str())),
            ("name", Value::from(r.name.as_str())),
        ])
    }

    fn to_record(a: &Agent) -> HashMap<String, OwnedValue> {
        let mut kinds = Vec::new();
        for (on, k) in [
            (a.session, "session"),
            (a.always, "always"),
            (a.denied, "denied"),
        ] {
            if on {
                kinds.push(k);
            }
        }
        record(&[
            ("agent", Value::from(a.id.as_str())),
            ("executable", Value::from(a.executable.as_str())),
            ("name", Value::from(a.name.as_str())),
            ("level", Value::from(a.level.as_str())),
            ("kinds", Value::from(kinds)),
            ("granted", Value::from(1795000000i64)),
            ("last-active", Value::from(a.last_active)),
            ("identified", Value::from(a.identified)),
        ])
    }

    #[zbus::interface(name = "org.helixdesktop.AgentAccess1")]
    impl FakeShell {
        fn list_agents(&self) -> Vec<HashMap<String, OwnedValue>> {
            self.agents.lock().unwrap().iter().map(to_record).collect()
        }
        async fn revoke(&self, agent: String, #[zbus(signal_emitter)] e: SignalEmitter<'_>) {
            self.calls.lock().unwrap().push(format!("Revoke {agent}"));
            self.agents.lock().unwrap().retain_mut(|a| {
                if a.id == agent {
                    a.session = false;
                    a.denied = false;
                }
                a.always || a.session || a.denied
            });
            let _ = Self::agents_changed(&e).await;
        }
        async fn forget(&self, agent: String, #[zbus(signal_emitter)] e: SignalEmitter<'_>) {
            self.calls.lock().unwrap().push(format!("Forget {agent}"));
            self.agents.lock().unwrap().retain(|a| a.id != agent);
            let _ = Self::agents_changed(&e).await;
        }
        async fn stop_all(&self, #[zbus(signal_emitter)] e: SignalEmitter<'_>) {
            self.calls.lock().unwrap().push("StopAll".into());
            self.agents.lock().unwrap().retain_mut(|a| {
                a.session = false;
                a.denied = false;
                a.always
            });
            let _ = Self::agents_changed(&e).await;
        }
        fn clear_log(&self) {
            self.calls.lock().unwrap().push("ClearLog".into());
        }
        fn list_reminders(&self) -> Vec<HashMap<String, OwnedValue>> {
            // Not by time: Settings sorts.
            self.reminders
                .lock()
                .unwrap()
                .iter()
                .rev()
                .map(reminder_record)
                .collect()
        }
        async fn cancel_reminder(&self, id: String, #[zbus(signal_emitter)] e: SignalEmitter<'_>) {
            self.calls
                .lock()
                .unwrap()
                .push(format!("CancelReminder {id}"));
            self.reminders.lock().unwrap().retain(|r| r.id != id);
            let _ = Self::reminders_changed(&e).await;
        }
        fn list_changes(&self) -> Vec<HashMap<String, OwnedValue>> {
            self.changes
                .lock()
                .unwrap()
                .iter()
                .map(|id| {
                    record(&[
                        ("id", Value::from(id.as_str())),
                        ("setting", Value::from("accent")),
                    ])
                })
                .collect()
        }
        /// Ids `changed-…` and `gone-…` fail as the shell's would.
        async fn undo(
            &self,
            id: String,
            #[zbus(signal_emitter)] e: SignalEmitter<'_>,
        ) -> std::result::Result<(), UndoFailure> {
            self.calls.lock().unwrap().push(format!("Undo {id}"));
            if id.starts_with("changed") {
                return Err(UndoFailure::Failed("changed".into()));
            }
            if id.starts_with("gone") {
                return Err(UndoFailure::Failed("unavailable".into()));
            }
            let found = {
                let mut changes = self.changes.lock().unwrap();
                let at = changes.iter().position(|c| *c == id);
                at.map(|at| changes.remove(at))
            };
            if found.is_none() {
                return Err(UndoFailure::NotFound("no such change".into()));
            }
            let _ = Self::changes_changed(&e).await;
            Ok(())
        }
        #[zbus(signal)]
        async fn agents_changed(e: &SignalEmitter<'_>) -> zbus::Result<()>;
        #[zbus(signal)]
        async fn reminders_changed(e: &SignalEmitter<'_>) -> zbus::Result<()>;
        #[zbus(signal)]
        async fn changes_changed(e: &SignalEmitter<'_>) -> zbus::Result<()>;
    }

    /// Example changes `ListChanges` lists.
    pub fn sample_changes() -> Vec<String> {
        vec![
            "9c1d0e2f4a6b8c3d".into(),
            "changed-1".into(),
            "gone-1".into(),
        ]
    }

    /// Example agents for the tests and for looking at the page.
    pub fn sample_agents() -> Vec<Agent> {
        vec![
            Agent {
                id: "/usr/bin/node /home/me/.local/lib/assistant/cli.js".into(),
                executable: "/usr/bin/node".into(),
                name: "Coding Assistant".into(),
                level: "desktop".into(),
                session: true,
                always: true,
                last_active: 1795082400,
                identified: true,
                ..Agent::default()
            },
            Agent {
                id: "/usr/bin/foot".into(),
                executable: "/usr/bin/foot".into(),
                level: "read-only".into(),
                denied: true,
                ..Agent::default()
            },
        ]
    }

    /// Example reminders, due at fixed times.
    pub fn sample_reminders() -> Vec<Reminder> {
        let agent = &sample_agents()[0];
        vec![
            Reminder {
                id: "3f9a0c2e7b1d4a56".into(),
                time: 1795084200,
                text: "Check that the nightly build passed".into(),
                agent: agent.id.clone(),
                executable: agent.executable.clone(),
                name: agent.name.clone(),
            },
            Reminder {
                id: "0b7e5d1c9a2f8e34".into(),
                time: 1795170600,
                text: "Stand-up meeting in 5 minutes".into(),
                agent: "/opt/bot/bin/bot".into(),
                executable: "/opt/bot/bin/bot".into(),
                name: String::new(),
            },
        ]
    }

    pub fn serve(conn: &Connection, agents: Vec<Agent>) -> Arc<Mutex<Vec<String>>> {
        let calls = serve_object(conn, agents);
        conn.request_name(SHELL).unwrap();
        calls
    }

    /// The object only, on a connection that already owns [`SHELL`].
    pub fn serve_object(conn: &Connection, agents: Vec<Agent>) -> Arc<Mutex<Vec<String>>> {
        let calls = Arc::new(Mutex::new(Vec::new()));
        conn.object_server()
            .at(
                "/org/helixdesktop/Agent1",
                FakeShell {
                    agents: Arc::new(Mutex::new(agents)),
                    reminders: Arc::new(Mutex::new(sample_reminders())),
                    calls: calls.clone(),
                    changes: Arc::new(Mutex::new(sample_changes())),
                },
            )
            .unwrap();
        calls
    }

    #[test]
    fn against_a_fake_shell() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let Some(bus) = testbus::private_bus() else {
            return;
        };
        let app = testbus::connect(&bus);
        assert_eq!(list(&app), Err(bus::Error::Missing), "no shell");

        let changes = Arc::new(AtomicUsize::new(0));
        let c = changes.clone();
        let watcher = testbus::connect(&bus);
        std::thread::spawn(move || {
            watch(&watcher, || {
                c.fetch_add(1, Ordering::SeqCst);
                true
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(100));

        let service = testbus::service(&bus);
        let calls = serve(&service, sample_agents());
        assert!(
            testbus::eventually(|| changes.load(Ordering::SeqCst) >= 1),
            "the shell came"
        );
        let agents = list(&app).unwrap();
        assert_eq!(agents.len(), 2);
        assert_eq!(agents[0].title(), "Coding Assistant", "active first");
        assert!(agents[1].denied);

        let before = changes.load(Ordering::SeqCst);
        revoke(&app, "/usr/bin/foot").unwrap();
        assert!(testbus::eventually(
            || changes.load(Ordering::SeqCst) > before
        ));
        assert_eq!(list(&app).unwrap().len(), 1);

        // A remembered grant outlives Revoke and StopAll, not Forget.
        stop_all(&app).unwrap();
        let left = list(&app).unwrap();
        assert!(left[0].always && !left[0].session);
        forget(&app, &left[0].id).unwrap();
        assert!(list(&app).unwrap().is_empty());
        // Unknown agents are not an error.
        revoke(&app, "/nowhere").unwrap();
        clear_log(&app).unwrap();

        // Reminders, soonest first, and RemindersChanged after a cancel.
        let reminded = Arc::new(AtomicUsize::new(0));
        let r = reminded.clone();
        let watcher = testbus::connect(&bus);
        std::thread::spawn(move || {
            watch_reminders(&watcher, || {
                r.fetch_add(1, Ordering::SeqCst);
                true
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        let list = reminders(&app).unwrap();
        let ids: Vec<&str> = list.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["3f9a0c2e7b1d4a56", "0b7e5d1c9a2f8e34"]);
        assert_eq!(list[0].text, "Check that the nightly build passed");
        assert_eq!(list[1].who().title(), "/opt/bot/bin/bot");
        cancel_reminder(&app, "3f9a0c2e7b1d4a56").unwrap();
        assert!(testbus::eventually(|| reminded.load(Ordering::SeqCst) >= 1));
        assert_eq!(reminders(&app).unwrap().len(), 1);
        // Unknown ids are not an error.
        cancel_reminder(&app, "nowhere").unwrap();

        // Changes: listed, undone (ChangesChanged), and why not.
        let changed = Arc::new(AtomicUsize::new(0));
        let c = changed.clone();
        let watcher = testbus::connect(&bus);
        std::thread::spawn(move || {
            watch_changes(&watcher, || {
                c.fetch_add(1, Ordering::SeqCst);
                true
            })
        });
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert_eq!(super::changes(&app).unwrap(), sample_changes());
        undo(&app, "9c1d0e2f4a6b8c3d").unwrap();
        assert!(testbus::eventually(|| changed.load(Ordering::SeqCst) >= 1));
        assert_eq!(undo(&app, "9c1d0e2f4a6b8c3d"), Err(UndoError::Gone));
        assert_eq!(undo(&app, "changed-1"), Err(UndoError::Changed));
        assert_eq!(undo(&app, "gone-1"), Err(UndoError::Unavailable));
        assert_eq!(super::changes(&app).unwrap().len(), 2);
        assert_eq!(
            *calls.lock().unwrap(),
            [
                "Revoke /usr/bin/foot",
                "StopAll",
                "Forget /usr/bin/node /home/me/.local/lib/assistant/cli.js",
                "Revoke /nowhere",
                "ClearLog",
                "CancelReminder 3f9a0c2e7b1d4a56",
                "CancelReminder nowhere",
                "Undo 9c1d0e2f4a6b8c3d",
                "Undo 9c1d0e2f4a6b8c3d",
                "Undo changed-1",
                "Undo gone-1"
            ]
        );
    }
}
