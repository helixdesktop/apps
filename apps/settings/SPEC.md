# Settings

Drop-in replacement for GNOME Settings.

- **App ID**: `org.helixdesktop.Settings`
- **Binary**: `helix-settings-app`
- **Crate**: `apps/settings` (package `helix-settings-app`)

The package and binary are deliberately **not** named `helix-settings`:
helix-desktop already has a crate called `helix-settings`, and two packages
or installed binaries with the same name would clash when both repos are
built or installed side by side.

## Purpose
Primary configuration UI for the desktop and system.

## Main windows
- Sidebar of panels (Wi-Fi, Bluetooth, Network, Appearance, Displays, Sound, Power, etc.).
- Search across panels.
- Modal dialogs for complex settings (displays, network connections).

## Contracts used
- GSettings schemas under `org.gnome.desktop.*`, `org.gnome.settings-daemon.*`, NetworkManager, etc.
- XDG portals: Settings, Background, Lockdown.
- CUPS over IPP, and cups-pk-helper (`org.opensuse.CupsPkHelper.Mechanism`) for changes (Printers).
- Session D-Bus: SessionManager, Shell, DisplayConfig, IdleMonitor, NetworkManager, UPower, etc.
- Helix: `org.helixdesktop.agent-access` (GSettings) and `org.helixdesktop.AgentAccess1` (the shell), helixdesktop/spec `contracts/agent-access/`.
- Helix greeter: `/etc/helix/greeter.toml`, `helix-login-config` and
  `helix-login-monitors` through `pkexec`, helixdesktop/spec
  `components/greeter.md` ("Configuration", "Changing the configuration",
  "Setting the login screen's layout", "Installing and switching").

## Non-goals
- Every panel from day one. Implement panels incrementally.
- GNOME-specific private APIs that are not part of the public contracts.

---

## Behaviour (derived spec)

### Window
- One `AdwApplicationWindow` (default 980×680) containing an
  `AdwNavigationSplitView`. It collapses to a single pane below 700 px wide
  (`AdwBreakpoint`); selecting a panel then pushes it and the back button
  returns to the list.
- The window's minimum size is 360×294. Every implemented panel fits that
  width without clipping: groups of cards, swatches and thumbnails wrap
  onto more lines instead of widening the page, and sliders in rows are at
  least 150 px wide (growing with the row).
- **Sidebar**: header bar titled *Settings* with a search toggle and the
  primary menu (*Keyboard Shortcuts*, *About Settings*), a search entry
  (shown with the toggle or `Ctrl+F`), and the panel list grouped by
  separators (GNOME's groups and order, with panels that are not yet
  available moved to the end, see below):
  1. Wi-Fi, Network, Bluetooth
  2. Displays, Sound, Power, Multitasking, Appearance
  3. Apps, Notifications
  4. Mouse & Touchpad, Keyboard
  5. Accessibility, Privacy & Security
  6. Date & Time, Users, About
- Each row: symbolic icon + title. Panels that are not implemented yet are
  listed after all working panels (also in search results) under a "Not
  Yet Available" heading, dimmed with the subtitle *Not yet available*,
  which is also their accessible description ("Wi-Fi, Not yet
  available"); selecting one shows a status page saying the panel will
  come in a later release.
- **Content**: an `AdwNavigationPage` with a header bar titled after the
  panel and an `AdwPreferencesPage` built on first visit (panels are built
  lazily and cached for the window's lifetime).
- The window opens on *Appearance* unless a panel is given on the command
  line.

### Search
- Typing filters the sidebar. The query is lower-cased and split on
  whitespace; a panel matches when **every** term is a substring of its title
  or of one of its keywords (each panel carries keywords for the settings it
  contains, e.g. Appearance: *dark, light, style, theme, accent, color,
  wallpaper, background*).
- Results are ordered: title starts with the query, then a term occurs in the
  title, then keyword-only matches; registry order within each rank. Group
  separators are hidden while searching.
- `Enter` in the search entry opens the first result; `Escape` clears and
  closes the search. The entry is named "Search Settings".
- A query without matches replaces the list with a "No Results Found"
  status page; clearing the query brings the list back. The number of
  matching panels ("3 panels found", "No results found") is announced once
  typing settles.

### Panel registry
- A static table of `PanelInfo { id, title, icon, keywords, group,
  available, aliases }`. Available panels have a builder implementing the
  `Panel` trait (`fn build(&self, ctx) -> gtk::Widget`).
- Panel ids and accepted aliases (aliases include GNOME Settings panel names
  so scripts that launch `gnome-control-center <panel>` keep working):

| Id | Aliases | Available |
|---|---|---|
| `wifi` | `wi-fi`, `wireless` | yes |
| `network` | `wired` | yes |
| `bluetooth` | | yes |
| `display` | `displays`, `screen` | yes |
| `sound` | `audio` | yes |
| `power` | | yes |
| `multitasking` | `workspaces` | yes |
| `appearance` | `background`, `wallpaper` | yes |
| `apps` | `applications`, `default-apps` | yes |
| `notifications` | | yes |
| `extensions` | `shell-extensions`, `add-ons` | yes |
| `mouse` | `touchpad`, `mouse-touchpad` | yes |
| `keyboard` | `input-sources` | yes |
| `accessibility` | `universal-access`, `a11y` | yes |
| `privacy` | `security`, `screen-lock`, `location` | yes |
| `datetime` | `date-time`, `time` | yes |
| `users` | `user-accounts` | yes |
| `login-screen` | `greeter`, `login` | only with the Helix greeter under greetd (below) |
| `about` | `info-overview`, `info`, `system` | yes |

- Pages inside a panel can have their own names (`panels::PAGES`,
  resolved by `panels::lookup_target` after the panel names): the panel
  opens on that page.

| Page | Aliases | Panel | Needs |
|---|---|---|---|
| `agent-access` | `agents` | `privacy` | schema `org.helixdesktop.agent-access` (without it: Privacy & Security's main page) |

### Settings bindings
- Every control reads its key on build and writes on change. When the key
  exists in the installed schema it goes through GSettings and the control
  follows external changes (`changed::key`). When the schema or key is
  missing (for example `accent-color` on schemas older than GNOME 47) the
  value lives in memory only (`helix_app_common::Prefs`) and the row says
  *Not supported by this system*.
- `data/org.helixdesktop.Settings.gnome-compat.gschema.xml` holds GNOME-compatible
  definitions of exactly the keys used (same schema ids, paths, types,
  defaults and enum nicks), for systems that do not ship the GNOME schemas.
- Delay choices are combo rows over fixed presets (seconds). A stored value
  that is not a preset is added as an extra entry (e.g. "7 minutes") so it is
  shown, not silently changed.

### Panels

#### Appearance
- **Style**: two toggle cards, *Default* and *Dark*, writing
  `org.gnome.desktop.interface color-scheme` `default` / `prefer-dark`.
  `prefer-light` (set by other tools) selects *Default* and is kept until the
  user picks a card.
- **Accent Color**: nine round swatches (blue, teal, green, yellow, orange,
  red, pink, purple, slate) → `accent-color`, wrapping onto a second line
  when narrow. The two style cards stack likewise.
- **Icons**: an *Icon Theme* row listing the installed icon themes by their
  `index.theme` `Name`, writing the theme's directory name to
  `icon-theme`. Themes are found in `$XDG_DATA_HOME/icons`, `~/.icons` and
  `icons` under each `$XDG_DATA_DIRS` entry (an earlier directory's theme
  hides a later one of the same name); `hicolor`, `default`, hidden themes
  and cursor-only themes (no `Directories`) are not listed. A value set by
  other tools that names no installed theme is listed as it is. Running apps
  follow the change.
- **Background**: a grid of thumbnails (at least two per line at 360 px,
  up to six) of the images in
  `/usr/share/backgrounds` (plus one level of subdirectories) and
  `$XDG_DATA_HOME/backgrounds`. Files named `<stem>-light.<ext>` /
  `<stem>-dark.<ext>` (also `-l` / `-d`) pair into one entry. Choosing an
  entry writes `org.gnome.desktop.background picture-uri` (light image) and
  `picture-uri-dark` (dark image, or the same image). The current entry
  (either URI matches) is outlined and carries a check mark, and its tile
  — a toggle button — is pressed, so screen readers read it as the
  current one; pressing it again keeps it. Tiles are named after the file
  ("Adwaita, light and dark variants" for pairs) and take one Tab stop
  each. With no images the group says "No Backgrounds Found". Thumbnails
  load on worker threads.
- **Add Picture…**: file chooser filtered to images; the file is copied into
  `$XDG_DATA_HOME/backgrounds/` (name de-duplicated) and applied to both
  keys.

#### Mouse & Touchpad
- General: *Primary Button* Left/Right (`peripherals.mouse left-handed`).
- Mouse: *Pointer Speed* (`mouse speed`, −1…1), *Mouse Acceleration*
  (`accel-profile`: on = `default`, off = `flat`), *Natural Scrolling*
  (`mouse natural-scroll`).
- Touchpad: *Touchpad* on/off (`touchpad send-events` `enabled` /
  `disabled`), *Disable While Typing*, *Pointer Speed* (`touchpad speed`),
  *Secondary Click* (`click-method`: `fingers` = Two Finger Push, `areas` =
  Corner Push), *Tap to Click*, *Two-Finger Scrolling* / *Edge Scrolling*
  (`two-finger-scrolling-enabled` / `edge-scrolling-enabled`, mutually
  exclusive), *Natural Scrolling* (`touchpad natural-scroll`).

#### Keyboard
- **Input Sources**: `org.gnome.desktop.input-sources sources` (`a(ss)`)
  shown with human-readable names from `/usr/share/X11/xkb/rules/evdev.lst`
  (layouts and `layout+variant`; `ibus` sources show the engine id). Rows
  have *Move Up*, *Move Down* and *Remove*. *Add Input Source…* opens a
  searchable dialog of all XKB layouts and variants plus the IBus input
  methods that IBus component files (`/usr/share/ibus/component/*.xml`,
  or `IBUS_COMPONENT_PATH`) list statically, without `xkb:` engines and
  without contacting the IBus daemon; picking one adds `('ibus', name)`.
  *Switch input sources individually for each window* → `per-window`.
- Both pickers (input sources, time zones) share one dialog: a spinner
  ("Loading Input Sources…" / "Loading Time Zones…", announced after
  0.6 s) until the choices are read, a "No Results Found" page, the
  result count announced once typing settles, `Enter` in the search
  picks the first result and `Down` moves into the list. At most 400 rows
  are listed; a note above them says "Showing 400 of N matches. Type to
  narrow the search."
- **Typing**: *Repeat Keys* (`peripherals.keyboard repeat`), *Delay*
  (`delay`, 100–2000 ms) and *Speed* (`repeat-interval`; slider position
  *p* ∈ [0,1], right = faster, maps to `round(110 − 100·p)` ms). Both
  sliders are insensitive while repeat is off.

#### Accessibility
GNOME 46+ layout: the panel is an `AdwNavigationView` (it brings its own
header bars). The main page has *Always Show Accessibility Menu*
(`a11y always-show-universal-access-status`) and rows opening five
sub-pages. Options with parameters are expander rows whose switch is the
feature key; the parameters show when expanded. Keys were checked
against the installed GNOME schemas (memory backend); all exist there and
in the compat schema.
- **Seeing**: *High Contrast* (`a11y.interface high-contrast`), *Show
  Status Shapes* (`a11y.interface show-status-shapes`), *Large Text*
  (`interface text-scaling-factor`: on = 1.25, off = 1.0; shown on when
  ≥ 1.25), *Cursor Size* (`cursor-size` 24/32/48/64/96), *Reduce Animation*
  (inverse of `interface enable-animations`), *Always Show Scrollbars*
  (inverse of `overlay-scrolling`), *Screen Reader*
  (`a11y.applications screen-reader-enabled`).
- **Hearing**: *Overamplification* (`sound allow-volume-above-100-percent`),
  *Visual Alerts* (`wm.preferences visual-bell`) with *Flash Area*
  (`visual-bell-type`: Flash Entire Screen = `fullscreen-flash`, Flash
  Focused Window = `frame-flash`).
- **Typing**: *Screen Keyboard* (`screen-keyboard-enabled`), *Cursor
  Blinking* (`interface cursor-blink`) with *Blink Speed*
  (`cursor-blink-time` 100–2500 ms, slider slow → fast), *Sound Keys*
  (`a11y.keyboard togglekeys-enable`). Typing Assist: *Enable by
  Keyboard* (`a11y.keyboard enable`); *Sticky Keys* (`stickykeys-enable`)
  with `stickykeys-two-key-off`, `stickykeys-modifier-beep`; *Slow Keys*
  (`slowkeys-enable`) with *Acceptance Delay* (`slowkeys-delay` 0–1000 ms)
  and `slowkeys-beep-press` / `-accept` / `-reject`; *Bounce Keys*
  (`bouncekeys-enable`) with `bouncekeys-delay` and
  `bouncekeys-beep-reject`.
- **Pointing & Clicking**: *Mouse Keys* (`a11y.keyboard mousekeys-enable`),
  *Locate Pointer* (`interface locate-pointer`), *Double-Click Delay*
  (`peripherals.mouse double-click` 100–1000 ms). Click Assist:
  *Simulated Secondary Click* (`a11y.mouse secondary-click-enabled`, delay
  `secondary-click-time` 0.5–3 s), *Hover Click* (`dwell-click-enabled`,
  `dwell-time` 0.2–3 s, `dwell-threshold` 0–30 px).
- **Zoom**: *Zoom* (`a11y.applications screen-magnifier-enabled`),
  *Magnification* (`a11y.magnifier mag-factor` 1–20), *Magnifier Position*
  (`screen-position`), *Follow the Pointer* (`mouse-tracking`:
  proportional, centered, push, none); *Crosshairs* (`show-cross-hairs`)
  with *Overlap Pointer* (inverse of `cross-hairs-clip`),
  `cross-hairs-thickness` 1–100, `cross-hairs-length` 20–4096; Color
  Effects: *Invert Colors* (`invert-lightness`), *Brightness* / *Contrast*
  (one slider writing `brightness-{red,green,blue}` /
  `contrast-{red,green,blue}`, −1…1), *Color* (`color-saturation`).
  Crosshair colour (`cross-hairs-color`) is not offered yet.
- Any key missing from the installed schemas makes its row say *Not
  supported by this system* (the value then lives in memory).

#### Privacy & Security
- Screen Lock: *Blank Screen Delay* (`session idle-delay`: 1, 2, 3, 4, 5, 8,
  10, 12, 15 minutes, Never = 0), *Automatic Screen Lock*
  (`screensaver lock-enabled`), *Automatic Screen Lock Delay*
  (`screensaver lock-delay`: Screen Turns Off = 0, 30 seconds, 1, 2, 3, 5, 30
  minutes, 1 hour; insensitive while auto lock is off), *Lock Screen
  Notifications* (`notifications show-in-lock-screen`).
- File History & Trash: *File History* (`privacy remember-recent-files`),
  *File History Duration* (`recent-files-max-age` days: 1, 7, 30, Forever
  = −1), *Automatically Delete Trash Content* (`remove-old-trash-files`),
  *Automatically Delete Temporary Files* (`remove-old-temp-files`),
  *Automatically Delete Period* (`old-files-age` days: 1, 2, 3, 4, 5, 6, 7,
  14, 30).
- Location: *Location Services* (`org.gnome.system.location enabled`).
- Devices: *Camera* (inverse `privacy disable-camera`), *Microphone*
  (inverse `disable-microphone`).
- Agents (only when the schema `org.helixdesktop.agent-access` is
  installed; an older helix-desktop has none and the group is not
  shown): an *Agent Access* row (subtitle *On*/*Off* from `enabled`)
  opening the Agent Access page below.
- Diagnostics: *Automatic Problem Reporting* (`report-technical-problems`).
- The panel is an `AdwNavigationView` (root page tagged `privacy`) so
  sub-pages push with a back button.

##### Agent Access
Helix's permission for agents (programs such as AI assistants) to read
and control the session through the desktop
(helixdesktop/spec#1, contract `contracts/agent-access/` and
`contracts/gsettings/org.helixdesktop.agent-access.md`). Settings writes
the GSettings keys directly; the shell enforces them. Navigation tag and
command-line name `agent-access` (`helix-settings-app agent-access`; the
shell's agent indicator opens it).

- Top: a description, then the master switch *Agent Access* (`enabled`,
  default off). While it is off the Access, Asking, Information,
  Safeguards and Excluded Apps groups are insensitive (Allowed Agents,
  Reminders and View Activity stay usable: they only take access away,
  cancel or show history).
- Access: *Access Level* combo over the first three levels of
  `access-level` (*Read Only* `read-only`, *Control the Desktop*
  `desktop`, *Control Apps* `apps`; the choice shown as the row's
  subtitle so long names fit), the group's description saying what the
  chosen level allows; *Screen and Input* switch (`screen-and-input`). Turning
  the switch on shows an `AdwAlertDialog` *Allow Screen and Input?*
  (Cancel default, *Allow* destructive) and the switch stays off unless
  *Allow* is chosen; while it is on the combo shows *Control Apps* and is
  insensitive; turning it off stores `apps`. An unknown stored value
  shows as *Read Only* (the contract).
- Asking: radio rows *Ask Each Session* (`remember-mode` `ask`) and
  *Remember* (`remember`); unknown values show as *Ask Each Session*.
- Allowed Agents: from the shell's `org.helixdesktop.AgentAccess1`
  (`ListAgents`, session bus, `org.gnome.Shell`,
  `/org/helixdesktop/Agent1`), most recently active first. Each row: the
  display name (else the program) as title; the program (agent id: the
  executable, or interpreter and script) under a display name, since a
  name proves nothing; then level · *Always allowed* / *Allowed this
  session* / *Denied this session* · *Active 5 minutes ago* / *Not
  active this session*. *Revoke* (shown when it has a session grant or a
  denial) calls `Revoke`; *Forget* (shown with a remembered grant) calls
  `Forget`. Header button *Revoke All* (`StopAll`, sensitive when some
  agent has something to revoke). The list follows `AgentsChanged` and
  the shell coming and going, and refreshes every 30 s while shown (for
  "last active"). The group's description says the top-bar indicator is
  always shown while an agent is active (not a setting). Placeholders:
  no agents, shell not running, and `AccessDenied` (the shell only
  accepts `helix-settings-app` beside its own binary or in `/usr/bin`:
  "Only the installed Settings app can list agents").
- Reminders: every agent's reminders not yet shown, from
  `AgentAccess1.ListReminders`, soonest first. Each row: the reminder's
  text as the shell returns it (*Reminder* when empty) as title; *Due
  19 November 2026, 14:30* · the agent (display name, else the program;
  the program as the tooltip under a display name); a *Cancel* button
  (accessible name *Cancel Reminder “text”*) calling `CancelReminder`,
  then reading the list again (a failure is a toast). The list is read
  when the page is shown, on `RemindersChanged` and when the shell comes
  or goes. Empty: *No reminders*. The group is hidden while the shell
  does not answer (not running, or one without reminders).
- Information: *Notifications Access* (`notifications`) and *Clipboard
  Access* (`clipboard`), both off by default, subtitles saying what they
  expose (messages and sign-in codes; passwords and copied text); *Read
  Text Fields* (`read-text-fields`, default off: "Agents controlling apps
  can see what is typed in fields; password fields are never shown"),
  shown only when the installed schema has the key (older desktops lack
  it). It is not tied to the access level (no switch on the page is).
- Safeguards: *Confirm Sensitive Actions* (`confirm-sensitive`, default
  on), *Allow From Remote Sessions* (`allow-remote`, SSH, default off).
- Excluded Apps (`excluded-apps`, app ids without `.desktop`): one row
  per app (icon, name, id; unknown ids shown as the id) with a Remove
  button ("Remove *App*"); *Add App…* in the header opens the searchable
  picker over installed apps (those already excluded and Settings itself
  left out). The description names the hard exclusions in one line:
  password fields, the lock screen, system password prompts and
  Settings (the shell excludes every Settings window from agents).
- *View Activity* row → *Agent Activity* page (tag and command-line
  name `agent-activity`, `helix-settings-app agent-activity`):
  *Keep Activity For* (`log-retention-days`: 1, 7, 30, 90, 365 days, a
  stored non-preset is added), *Clear Activity* (*Clear…*, confirmed by
  an alert, then `ClearLog`), and *Recent Activity*: the newest 300
  entries of `$XDG_STATE_HOME/helix/agent-activity/YYYY-MM-DD.jsonl`,
  read directly and sorted by `time`, as the contract's "Activity log"
  pins the lines (keys `time` (Unix seconds or ISO 8601), `agent`,
  `name`, `method`, `target`, `level`, `outcome`, `reason`, `error`, `grant`,
  `count`/`until`, `dropped`, `role`, `action`, `node-count`,
  `text-length`, `choice-count`, `mime-type`, `data-length`, `scope`,
  `width`/`height`, `filled-count`, `event-count`, `done-count`,
  `move-count`, `click-count`, `button-count`, `scroll-count`,
  `key-count`, `shortcut-count`; unknown keys, the other `*-length` keys
  and lines that are not a JSON object are ignored). Each row:
  - title: what happened, as a short translated phrase for every
    `Agent1` and `AgentAccess1` method (also those the shell does not
    implement yet) and for the events: *Listed windows*, *Searched
    files*, *Asked for access* (`RequestAccess`, `ConsentPrompt`),
    *Asked to confirm an action* (`SensitivePrompt`), *Stopped by you*
    (`Stop`), *Reminder due* (`Reminder`), *Searched an app’s
    controls* (`A11yFind`), *Activity log full* (`LogFull`), *Activity log cleared*
    (`ClearLog`), *Access revoked* (`Revoke`), *Access revoked and
    forgotten* (`Forget`), *All access revoked* (`StopAll`), *Reminder
    cancelled* (Settings' `CancelReminder`); a refused `AgentAccess1`
    call (another program) reads *Tried to clear the activity log*,
    *Tried to list reminders* and the like. `ListReminders` and
    `CancelReminder` are in both interfaces: a line without `level`, or
    refused with `AccessDenied`, is Settings' (`Agent1` lines always
    have a level). Unknown names show as they are.
  - subtitle line 1 (when there is a target): a window as *A window of
    org.example.App* (*A window* without its app), a `file://` URI as a
    path, a `SensitivePrompt`'s method by its phrase, a reminder
    (`reminder:<id>`) not at all, `GetClipboard`'s app as *Copied from
    org.example.App*, a `Screenshot`'s `screen` as *The whole screen* and
    `monitor:<connector>` as *Display HDMI-A-1* (without a target, its
    `scope`: *The whole screen*, *A display*, *A window*), else as
    written; then, from the line's
    own keys: the control's role (*Button*, *Text field*, *Password
    field*…, less common AT-SPI roles as named), an `A11yAction`/
    `A11ySetValue` action (*Clicked*, *Pasted*, *Closed the window*,
    *Another action* for `other`, *Set text*…, unknown ones raw), *143
    items* (`A11yTree` `node-count`), *1 match* (`A11yFind`), *17
    characters* (`text-length`), *2 choices* (`choice-count`); the
    clipboard's kind of content (`mime-type`: *Text*, *HTML*, *Links*,
    *Picture* for every image kind, *PDF document*, *Password manager
    hint*, *Other*; unknown kinds raw) and size (`data-length`, GLib's
    *12.3 kB*); a screenshot's *2560 × 720 pixels* and *3 areas hidden*
    (`filled-count`; *Nothing hidden* for 0); an `Input`'s *5 actions*
    (`event-count`), then *1 click*, *1 shortcut*, *2 keys*, *1 pointer
    move*, *1 scroll*, *1 button press or release* (each kind's count,
    when there) and *10 characters typed* (`text-length`), and, when it
    did not finish (refused, cancelled or failed with a `done-count`),
    *Stopped after 2 steps* / *Stopped before the first step*
    (`done-count` counts steps, not events: a click is a press and a
    release, so it is never shown against `event-count`). This line is
    set a little apart from the next so the two do not read as one.
  - subtitle line 2: who (display name, else the agent id; *Settings*
    for Settings' own allowed `AgentAccess1` calls; nothing for
    `LogFull`) · how it went · when. How it went: *Allowed*, plus
    *Failed: timed out* / *not found* / *not supported* / *invalid
    request* / *cancelled* / *Failed* for an `error` (*Failed: Name*
    for others); *Refused* (*Refused 37 times* for a coalesced line)
    and the reason. When the line has a `reason` this version knows
    (contract "Activity log", "Reasons"), that says it: *A password
    field* (`password-field`; *A password manager marked it secret* for
    `GetClipboard`), *A system prompt was open* (`system-prompt`), *Part
    of the desktop* (`shell`), *Settings* (`settings`), *An excluded
    app* (`app`), *Clipboard access was off* (`clipboard`). Otherwise
    (older lines, unknown tokens) the error's: *Agent Access was off*,
    *Needs a higher access level*, *Excluded app*, *Denied by you*,
    *Only Settings may do this*, …, the raw name for unknown ones; where
    the method says more: *Clipboard access was off* (`FeatureOff` on
    the clipboard methods), *Copied from a protected app, or a password*
    (`GetClipboard` `Excluded`), *A system prompt was open*
    (`SetClipboard`), *A protected window or a system prompt*
    (`Screenshot`), *A protected window, a password field or a system
    prompt* (`Input`). A `cancelled` line with a known `reason` shows
    only it: *Stopped: you used the keyboard or pointer* (`user-input`),
    *Refused: you had just used the keyboard or pointer* (`cooldown`),
    *Stopped from the indicator or Settings* (`stopped`: *Stop*,
    *Revoke*, *Forget*, *Stop all* or Agent Access turned off). Without
    one: *Cancelled*, and for an `Input` stopped while it ran
    (`cancelled` with a `done-count`) *Stopped by you*. Consent
    prompts: the level asked and *Allowed for this session* / *Always
    allowed* (`grant`) / *Denied* / *Not answered*; sensitive prompts
    *Allowed* / *Denied* / *Not answered*; `Stop` and Settings' own
    calls nothing; `Reminder` *Shown*, *Not shown* · the reason, or
    *Cancelled by you*; `LogFull` *Refused calls are no longer recorded
    today* (`dropped` `refusals`) or *Nothing more is recorded today*
    (`all`). When: date and time, and for a coalesced line the end
    (*19 November 2026, 14:02–14:03*, the date again on another day).
  Empty: "What agents do will be listed here".
- Settings never grants anything: the interface has no such method.

#### Notifications
- *Do Not Disturb* (inverse of `org.gnome.desktop.notifications
  show-banners`), *Lock Screen Notifications* (`show-in-lock-screen`).
- *Applications*: one switch row per id in `application-children`, bound to
  `enable` of the relocatable schema
  `org.gnome.desktop.notifications.application` at
  `/org/gnome/desktop/notifications/application/<id>/`. Names and icons come
  from the matching desktop file when found (icons are presentational;
  the row title names the app). The group is hidden when the
  relocatable schema is not installed or the list is empty.

#### Extensions
Helix shell extensions (helixdesktop/spec `contracts/extensions/README.md`),
through `org.helixdesktop.Extensions1` at `/org/helixdesktop/Extensions1` on the session
bus (owned by the shell, `org.gnome.Shell`). The shell's own "Extensions…"
menu entry opens this panel when Settings is installed.

- Until the first read of the list answers, the page shows a spinner
  labelled *Loading Extensions…* (also its accessible name).
- When reading the list fails (not a Helix session, or the shell not
  running) the page is a status page: *Extensions Are Not Available* /
  "They need the Helix shell." It is never shown before that answer. The
  list follows the service: it is read when the panel is built, again on
  every `Changed(id)` signal (at most every 0.3 s), and when the service
  appears or goes away.
- *Use Extensions*: a switch row, the inverse of `org.helixdesktop.extensions
  disable-all` (marked *Not supported by this system* without the schema).
- One expander row per extension, user and system ones by name, then
  development ones (with "Development" in the subtitle). Title: its name;
  subtitle: its version and state: *Running*, *Starting*, *Not running*,
  *Off*, *Off for this session* (Shift held at login), *Needs approval*,
  *Cannot run: <reason>*, *Stopped working* (with the last log line when
  there is one). A suffix switch is on when it is enabled and approved
  (development extensions have none: they run without enabling).
  - Turning it on calls `Enable(id, true)` on a worker thread, with no
    reply timeout: the shell may first ask the user (its approval prompt).
    The switch is insensitive meanwhile. Refused
    (`org.freedesktop.DBus.Error.AccessDenied`): the switch goes back off
    and a toast says "<name> was not turned on". Turning it off calls
    `Disable(id)`.
  - Rows inside: the description; *Access*: "Sandboxed" (with "Can: …"
    and the permission texts below), "Its permissions let it out of its
    sandbox: it can do anything you can." for a sandboxed one whose
    permissions do (`leaves-sandbox`; from a shell without that key,
    `files:home` or `session-bus`), or "Full access: it can do anything
    you can" for one that is not sandboxed, plus the permission texts;
    the last two are shown as warnings. The permission texts are the
    shell's, word for word (helixdesktop/spec
    `contracts/extensions/README.md`, "Permissions"), and each
    `session-bus:…` rule names what it allows ("talk to
    org.freedesktop.Notifications");
    *Problem* (invalid or crashed) with the reason and a *Show Log* button
    (a dialog with `GetLog(id)`, newest last); *Restart* (running or
    crashed) calls `Restart(id)`.
  - Its settings (`GetSettings(id)`, in that order), each written with
    `SetSetting(id, key, value)` when changed: `bool` a switch row;
    `int` a spin row (step 1, `min`/`max` when declared, else
    −2147483648…2147483647); `double` a spin row (two decimals; step a
    twentieth of `max` − `min` when both are declared, else 0.1); `enum` a
    combo row of its choices; `string` an entry row applied with its
    apply button; `string-list` an entry row of comma-separated values
    (spaces around them dropped, empty ones left out), applied the same
    way. The setting's `help` is the row's subtitle (entry rows: its
    tooltip). A refused value (`InvalidArgs`) gives a toast with the
    error and the row shows the stored value again.
  - Its commands (`commands` of `ListExtensions`): one row each, its
    title, with the key as a shortcut label ("None" when unbound) and
    *Change…*. *Change…* opens a dialog "Press the new keys for <title>";
    the first combination with Ctrl, Alt or Super (or a function key
    alone) is sent with `BindCommand(command, accelerator)` and closes it;
    Escape cancels; Backspace clears the key (`""`). A combination the
    shell refuses (in use, or does not parse) is shown in the dialog
    ("Could not use these keys: …"), which stays open; a plain key without
    Ctrl, Alt or Super shows "Use Ctrl, Alt or Super with this key" (a
    modifier alone just starts a combination). Both messages are announced
    at once (assertive), since focus stays in the dialog.
- Changes made elsewhere (the shell, `helix-extensions`) show through
  `Changed`; the rows are updated in place, and rebuilt when the set of
  extensions or of their settings changes.

#### Multitasking
- General: *Hot Corner* (`interface enable-hot-corners`), *Active Screen
  Edges* (`org.gnome.mutter edge-tiling`).
- Workspaces: *Dynamic Workspaces* / *Fixed Number of Workspaces* (radio
  rows on `mutter dynamic-workspaces`), *Number of Workspaces*
  (`wm.preferences num-workspaces` 1–36, sensitive only when fixed).
- Multi-Monitor: *Workspaces on Primary Display Only*
  (`mutter workspaces-only-on-primary`).
- App Switching: *Include Apps From the Current Workspace Only*
  (`org.gnome.shell.app-switcher current-workspace-only`).

#### Power
- Battery (shown only when UPower reports a present battery; read-only):
  a row titled "Battery" with the percentage first in its subtitle
  ("57 % · Discharging — 2 hours remaining") and a level bar named
  "Battery Level" whose value text is the percentage (read as "Battery
  Level, 57 %"); state (Charging, Discharging, Fully Charged, …) and
  time to empty / full, from the UPower `DisplayDevice`
  (`org.freedesktop.UPower.Device`: `IsPresent`, `Type`, `Percentage`,
  `State`, `TimeToEmpty`, `TimeToFull`), fetched on a worker thread when the
  panel is built and every 30 s afterwards.
- Power Saving: *Dim Screen* (`settings-daemon.plugins.power idle-dim`),
  *Screen Blank* (`session idle-delay`, same presets as Privacy),
  *Automatic Suspend* when plugged in / on battery: switch (type `suspend`
  vs `nothing`) + delay (`sleep-inactive-{ac,battery}-timeout`: 15, 20, 25,
  30, 45, 60, 80, 90, 100, 120 minutes). The battery row is shown only when a
  battery is present. Both delay rows show "Delay" but are named "Suspend
  Delay When Plugged In" / "Suspend Delay On Battery Power".
- General: *Power Button Behavior* (`power-button-action`: Suspend, Power
  Off = `interactive`, Hibernate, Nothing), *Show Battery Percentage*
  (`interface show-battery-percentage`).

#### Wi-Fi
NetworkManager on the system bus: one reading is the root object's
properties, the object manager at `/org/freedesktop` (devices, access
points, active connections, IP configurations) and `GetSettings` of every
saved connection (`nm.rs`). The page follows NetworkManager and BlueZ live
(their signals, read again at most every 0.5 s).

- Until the first reading: *Loading Wi-Fi…*. Then *Networking Is Not
  Available* (NetworkManager is not running), *No Wi-Fi Adapter Found*
  (no managed device of type Wi-Fi), or the page below.
- *Wi-Fi*: `WirelessEnabled`. When `WirelessHardwareEnabled` is false the
  switch is insensitive: "Turned off by a hardware switch".
- *Airplane Mode*, as in the shell's quick settings: on while Wi-Fi is off
  and no BlueZ adapter is powered; turning it on turns Wi-Fi, mobile
  broadband (`WwanEnabled`) and every adapter off, turning it off turns
  Wi-Fi and the adapters on.
- *Wi-Fi Adapter*: with several Wi-Fi devices, a choice of which one the
  page shows (by interface name); everything below is that device's.
- *Visible Networks*: one row per network name the device sees (hidden
  networks without a name are left out), the connected one first, then
  by strength. Signal icon (as the shell's status area), name, "Connected"
  / "Connecting…" / "Saved", a lock when secured; screen readers hear
  "Good signal, Secured" as the row's description. Saved networks have a
  *Details* button. Activating a row: the connected network opens its
  details; a saved one is activated (`ActivateConnection` with the
  access point); an open one is joined with `AddAndActivateConnection({},
  device, ap)`; WEP, WPA/WPA2 and WPA3 Personal ask for the password first
  ("Connect to NAME", *Connect* enabled once the password fits: 8–63
  characters or 64 hex digits for WPA) and join with `key-mgmt`
  `wpa-psk` / `sae` / WEP keys; enterprise networks give a toast that
  they cannot be joined here yet. The page asks for a scan
  (`RequestScan`) when it is shown and every 20 s while it is.
- *Details* (shared with the Network panel, `ui/netedit.rs`): while
  connected, signal, security, band, link speed, IPv4/IPv6 addresses,
  default route, DNS and hardware address; *Share Network* (below);
  *Connect Automatically* (`connection.autoconnect`), *Metered Connection*
  (`connection.metered` 1 / 2); IPv4 and IPv6: *Method* (Automatic,
  Manual, Link-Local Only, Disabled), and as they apply *Addresses*
  (`a.b.c.d/prefix`, comma separated; /24 or /64 when left out),
  *Gateway*, *Automatic DNS* (`ignore-auto-dns` inverted), *DNS Servers*,
  *Routes* (`destination/prefix gateway`, comma separated, into
  `route-data`), *Use Only for Resources on This Network*
  (`never-default`). *Apply* checks the fields (the first wrong one is
  marked invalid, named and focused), then `Update`s the connection with
  its secrets fetched first (`GetSecrets`, as an update drops secrets it
  leaves out) and activates it again when it was active. *Forget
  Network…* (confirmed) deletes it.
- *Share Network* / *Show QR Code*: a dialog with a QR code in the
  `WIFI:S:…;T:WPA|SAE|WEP|nopass;P:…;;` form phones read, the name,
  password (from `GetSecrets`) and security.
- *Saved Networks*: the saved Wi-Fi connections (hotspots left out),
  each opening its details.
- *Connect to Hidden Network*: name, security (None, WPA/WPA2 Personal,
  WPA3 Personal) and password; `AddAndActivateConnection` with
  `802-11-wireless.hidden`.
- *Wi-Fi Hotspot*: network name (the host name, or the hotspot's
  previous name) and a random 10-character password (GNOME Settings'
  alphabet of lower-case letters and digits without look-alikes, drawn
  from `/dev/urandom` with rejection sampling); turning it on
  creates (or updates and activates) a connection with `mode` `ap`,
  `wpa-psk` and `ipv4.method` `shared`, warning that it disconnects the
  current network. While a hotspot runs the network list is replaced by a
  *Hotspot* group with its name, password, QR code and *Turn Off*
  (`DeactivateConnection`).
- Mutating calls allow interactive authorization (polkit may ask);
  refusals are not reported, other errors are toasts ("Could not connect
  to NAME: …").
- Tested against a fake NetworkManager on a private bus (`nm.rs`,
  `netconf.rs`); not yet tried with real Wi-Fi hardware. Not yet:
  enterprise (802.1X) networks, editing a saved network's password or
  security, choosing a hotspot's band.

#### Network
Wired connections and VPNs from NetworkManager (as for Wi-Fi, followed
live), and the network proxy.

- Until the first reading: *Loading Network…*. Without NetworkManager a
  *Connections* group says it is not running; the proxy still works.
- One *Wired* group per managed Ethernet device ("Wired (enp3s0)" when
  there are several): a row with the state ("Connected — 1000 Mb/s",
  "Connecting…", "Cable unplugged", "Off"), *Details* and a switch named
  after the group. On activates the device's first profile (or, without
  one, `AddAndActivateConnection({}, device, /)`); off is
  `Device.Disconnect`. The switch is insensitive without a cable.
  Profiles are the saved `802-3-ethernet` connections bound to the
  interface or to none; with several, each is listed ("In use" with a
  check, or activated when chosen) with its *Details*. *Add Profile* saves
  a new one ("Wired Profile N", bound to the interface, not connecting by
  itself) and opens its details.
- *Details* is the shared editor described under Wi-Fi (addresses, DNS,
  routes, metered, connect automatically), plus *Name*; *Remove
  Profile…* deletes it.
- *VPN*: saved `vpn` and `wireguard` connections, each with its state
  (Connected, Connecting…, Off), *Details* (type — WireGuard, OpenVPN,
  OpenConnect… —, addresses, the shared editor, *Remove VPN…*) and a
  switch (`ActivateConnection(vpn, /, /)` / `DeactivateConnection`). The
  *+* button ("Import VPN From File…") takes a `.conf` or `.ovpn` file:
  WireGuard files (`[Interface]` / `[Peer]`, `wg-quick` format) are turned
  into a `wireguard` connection here (`netconf::wireguard`: keys,
  endpoint, allowed IPs, keepalive, addresses and DNS, the interface named
  after the file, not connecting by itself) and saved with
  `AddConnection`; other files go to `nmcli connection import type openvpn
  file …` (the OpenVPN plugin's importer).
- *Network Proxy*: *Mode* (`org.gnome.system.proxy mode`: Off = `none`,
  Manual, Automatic = `auto`); Automatic shows *Configuration URL*
  (`autoconfig-url`, applied with its button); Manual shows host and port
  for HTTP, HTTPS, FTP and SOCKS (`org.gnome.system.proxy.{http,https,ftp,socks}`
  `host` / `port`) and *Ignore Hosts* (`ignore-hosts`, comma separated).
  The compat schema has these keys.
- Tested against the fake NetworkManager (`ui/network.rs`, `nm.rs`,
  `netconf.rs`). Not yet: creating a VPN by hand and editing a VPN's
  plugin settings (gateway, credentials: NetworkManager's per-plugin
  editors), importing files other than WireGuard and OpenVPN, 802.1X on
  wired connections; the `nmcli` import path has not been tried against a
  real NetworkManager.

#### Users
AccountsService (`org.freedesktop.Accounts` on the system bus:
`FindUserById` for this process's uid, `ListCachedUsers`, the users'
`org.freedesktop.Accounts.User` properties and setters, `CreateUser`,
`DeleteUser`), followed live through its signals (`accounts.rs`). Every
change is sent with interactive authorization allowed, so polkit (the
shell's agent) may ask for a password.

- Until the first reading: *Loading Users…*; without the service, *Users
  Are Not Available* / "The accounts service is not running."
- The picture (`IconFile`, else initials of the name) is a button,
  "Change Picture": a dialog with the pictures in `/usr/share/pixmaps/faces`
  (each named "Use Picture “name”") and *Picture From a File* (a file
  chooser for images); the choice goes to `SetIconFile` (AccountsService
  copies it).
- *Your Account*: *Name* (`RealName`, applied with its button),
  *Password* / *Change…* (new password twice, at least 8 characters, a
  hint below saying what is wrong; hashed here with the system's
  `crypt(3)`, SHA-512 with a random salt, and sent with
  `SetPassword(hash, "")` — the plain password never leaves the process),
  *Automatic Login* (`SetAutomaticLogin`), *Language* (*System Default*
  = empty, or one of the installed UTF-8 locales from `locale -a`,
  `SetLanguage`).
- *Other Users* (shown to administrators, `AccountType` 1): the other
  non-system users with their pictures and "Administrator" / "Standard".
  Until unlocked, the group says "Unlock to add, remove or change other
  users." and has *Unlock…*: polkit `CheckAuthorization` for
  `org.freedesktop.accounts.user-administration` (subject: this
  connection's bus name, user interaction allowed). Unlocked, rows open a
  dialog: *Account Type*, *Automatic Login*, *Password* (as above, set by
  the administrator), *Parental Controls* (opens `malcontent-control
  --user=NAME` when installed), *Remove User…* (confirmed: *Keep Files* or
  *Delete Files*, `DeleteUser(uid, remove)`). *Add User…* (+): *Full
  Name*, *Username* (made from the full name until edited; lower case
  letters, digits, `-` and `_`, starting with a letter, at most 32;
  marked invalid otherwise), *Account Type*, and *Set a Password Now*
  (else `SetPasswordMode(1)`: they choose one at first login).
- Tested against a fake AccountsService and polkit on a private bus
  (`accounts.rs`, including that only a hash that verifies arrives). Not
  yet: taking a picture with the camera, fingerprint login (fprintd),
  changing one's own password by giving the current one (polkit asks
  instead).

#### Login Screen
Helix's own panel (GNOME Settings has none; GDM is configured by hand):
the Helix greeter's settings for everyone, `/etc/helix/greeter.toml`
(helixdesktop/spec `components/greeter.md`, "Configuration").
- **Shown only** when helix-greeter is installed (`/usr/bin/helix-greeter`
  and `/usr/lib/systemd/system/greetd.service.d/helix-greeter.conf` are
  files) and greetd is the display manager: systemd's
  `GetUnit("display-manager.service")` on the system bus, whose
  `org.freedesktop.systemd1.Unit.Id` is `greetd.service` (no `systemctl`;
  systemd is asked only when both files are there). Otherwise the panel
  and its row are hidden and search does not find it. The check runs on
  a worker thread when the window is made (the row appears when it
  answers), so switching display managers and reopening Settings is
  enough. Keywords: *login, greeter, lock, banner, user list, sign in,
  display manager, greetd, accent, clock*.
- The page description says the settings are everyone's, need an
  administrator and apply the next time the login screen starts.
- **Values** are read from the file directly (it is world-readable) when
  the panel is built, after every change, and when a file monitor sees
  it change. A missing file, a file that is not valid TOML, not a regular
  file or larger than 1 MiB, and keys of the wrong type show the
  defaults. A broken file also shows a banner ("The login screen's
  settings file can't be read. Fix /etc/helix/greeter.toml to change
  them here.") and the controls are insensitive: the helper would leave
  such a file alone (exit 2) rather than drop the administrator's
  content.
  - *Users* › *Show User List* (`user-list`, default on): "When off, the
    login screen asks for a username".
  - *Banner* › *Show a Banner* and *Banner Message* (`banner`): the
    entry is sensitive while the switch is on; switching it off keeps the
    text in the entry but removes the key; an empty or blank text
    removes it too (the helper refuses an empty banner). Checked as the
    helper checks it: at most 200 characters (Unicode scalar values), no
    control characters (line breaks and tabs included) and no
    bidirectional overrides or isolates (U+202A–U+202E, U+2066–U+2069);
    a problem is the group's description ("The banner is too long: 203
    characters, at most 200"…), the entry gets the `error` style, the
    message is announced once typing settles, and *Apply…* is
    insensitive. Only a banner being changed is checked: one already in
    the file (written by hand) blocks no other change.
  - *Accent Color* (`accent`): Appearance's nine swatches (named for
    assistive technology and in tooltips); unset or an unknown name shows
    *Blue*, as the greeter draws it.
  - *Clock* › *Clock Format* (`clock-format`): *Automatic* (no key: the
    system's `clock-format` or the language, as the greeter decides),
    *24-hour* (`24h`), *12-hour* (`12h`); an unknown value shows
    *Automatic*.
  - *Displays* › *Use This Layout for the Login Screen*: the Displays
    panel's action (below), here too when `helix-login-monitors` is
    installed. It applies at once through its own helper.
- **Changes are staged.** The controls show what is wanted; while that
  differs from the file (compared as the greeter sees it: an unset user
  list is a shown one, so switching it off and on again is no change), an
  action bar at the bottom says "Changes not applied yet" with *Discard*
  (the controls show the file again) and *Apply…* (suggested; accessible
  names "Discard Login Screen Changes", "Apply Login Screen Changes";
  tooltip "Needs an administrator"). *Apply…* runs `pkexec
  /usr/libexec/helix/helix-login-config` once for the whole batch, with
  no arguments, the changed keys on its standard input as TOML (only
  `user-list`, `banner`, `accent`, `clock-format` that changed, and
  `remove = ["banner", "clock-format"]` for keys to delete; a key is
  never both), so one administrator prompt covers all the edits (polkit
  keeps the authorization for a while, `auth_admin_keep`). While it runs
  the bar says "Waiting for an administrator…" and its buttons are
  insensitive. Then: exit 0: toast "Saved. The login screen will use
  these settings the next time it starts." and the file is read again;
  pkexec 126 (the prompt dismissed): nothing is said, the edits stay;
  127 (not authorized, authentication failed): toast "Could not change
  the login screen: not authorized", the edits stay; anything else (1
  invalid input, 2 the file could not be read or written): toast "Could
  not change the login screen settings: <the helper's reason>", the edits
  stay and the file is read again. Settings never writes the file
  itself. The helper is the authority: Settings checks the same limits
  only to say what is wrong at once.
- Without `helix-login-config` (or `pkexec`) a banner says the settings
  can't be changed here and the controls are insensitive.
- A change to the file made elsewhere is shown at once when nothing is
  staged; with staged edits they are kept and compared with the new file.
- Tested: reading (the spec's example, empty, wrong types, unknown
  values, broken and missing files, a directory), banner rules, the
  changes sent for every key (the spec's example input), the helper run
  through a fake `pkexec` (arguments, input, every exit status), each
  visibility condition with fake files and a fake systemd answer, and the
  systemd query against a fake systemd on a private bus. Looked at in a
  headless helix-comp session with fake files (`HELIX_SETTINGS_TEST_ROOT`,
  debug builds only) and a fake systemd (`fakes::serve_fakes` with
  `HELIX_FAKE_DISPLAY_MANAGER`): the panel, staged edits, *Apply…* with
  a fake helper (input `user-list = false`, `accent = "red"`,
  `remove = ["banner"]`), a too-long banner, a broken file, and the panel
  hidden under GDM (search "login" finds only Users).

#### Apps
- *Default Apps*: *Web*, *Mail*, *Calendar*, *Music*, *Video*, *Photos*,
  each a choice of the apps GIO lists for its first content type
  (`x-scheme-handler/http`, `x-scheme-handler/mailto`, `text/calendar`,
  `audio/mpeg`, `video/mp4`, `image/jpeg`), showing GIO's default.
  Choosing one makes it the default (`set_as_default_for_type`, i.e. the
  user's `mimeapps.list`) for that type and the category's related types
  it supports (Web also https, HTML and XHTML; Music, Video and Photos
  the common formats; `defaults.rs`).
- *Removable Media* (`org.gnome.desktop.media-handling`): *Never Ask or
  Start Apps* (`autorun-never`; the rows below are insensitive while on)
  and, for CD Audio, DVD Video, Music Player, Photos and Software
  (`x-content/…` types), *Ask What to Do* (in none of the lists), *Do
  Nothing* (`autorun-x-content-ignore`), *Open Folder*
  (`autorun-x-content-open-folder`) or *Open APP* (one of the apps for the
  type: `autorun-x-content-start-app`, and that app made GIO's default for
  the type). A type is in at most one list. Follows changes made
  elsewhere.
- *Installed Apps*: the apps GIO shows (`should_show`), by name, with
  their icons; the list follows `GAppInfoMonitor`. Activating one opens
  a dialog:
  - its name, icon and description;
  - *Permissions*: a switch for each permission the portals' permission
    store (`org.freedesktop.impl.portal.PermissionStore`, session bus)
    has an entry for, for the app id (desktop id without `.desktop`):
    Run in Background (`background`/`background`), Notifications
    (`notifications`/`notification`), Location (`location`/`location`,
    `EXACT` / `NONE`, its time stamp kept), Camera, Microphone, Speakers
    (`devices`/…), Screenshots (`screenshot`/`screenshot`); others `yes` /
    `no`. Changed with `SetPermission`. Without entries: "This app has not
    asked for any permissions yet."; without the store, that it is not
    running. The dialog follows the store's `Changed` signal;
  - *Notifications* (`org.gnome.desktop.notifications.application`
    `enable` for the app, when it is one of `application-children`, the
    same switch as the Notifications panel);
  - *Search in the Overview*, for apps with a shell search provider
    (`gnome-shell/search-providers/*.ini` naming its desktop id), the
    inverse of its entry in `org.gnome.desktop.search-providers
    disabled`;
  - *Opens*: the file types it is GIO's default for (by description),
    *Reset File Types* (forgets the user's choices for them), and the link
    schemes it handles (`x-scheme-handler/…`);
  - for Flatpak apps (desktop file under
    `<installation>/exports/share/applications/`): *Installed Size* (the
    active deployment, `<installation>/app/<id>/current/active`, links
    resolved), *App Data* (the size of `~/.var/app/<id>`), both counted on
    a worker thread, and *Uninstall* (after a confirmation,
    `flatpak uninstall --noninteractive --user|--system <id>` for the
    installation it came from; the system installation asks for
    authorization through polkit; its data is kept; a toast says how it
    went);
  - *Open in Software* (`appstream://<id>`; a toast when nothing handles
    it), where updates and uninstalling are; *Reset Permissions*
    (`DeletePermission` for each of its entries: the portals ask again).
- Tested against a fake permission store on a private bus
  (`permissions.rs`) and the media and type logic in unit tests
  (`defaults.rs`: also the Flatpak installation, installed size through
  the deployment links, and the uninstall command). The uninstall command
  itself has not been run in tests (it would change the host's
  installations). Not yet: per-app storage for other kinds of apps,
  file-type-level choices other than reset.

#### Displays
`org.gnome.Mutter.DisplayConfig` on the session bus (served by helix-comp;
helixdesktop/spec `contracts/dbus/org.gnome.Mutter.DisplayConfig.md`):
`GetCurrentState`, `ApplyMonitorsConfig`, `ApplyMonitorsConfigAllowed`,
followed live through its signals (`MonitorsChanged`) and it coming or
going (`displays.rs`).

- Until the first reading: *Loading Displays…*; without the service,
  *Displays Are Not Available* / "The display service is not running."
  When `ApplyMonitorsConfigAllowed` is false the controls are insensitive
  and say changing the displays is not allowed in this session.
- Changes are collected first: while the edited configuration differs
  from the current one, a bar under the page says "The display settings
  changed." with *Reset* and *Apply*. *Apply* calls
  `ApplyMonitorsConfig` with method verify, then persistent; the shell
  then asks whether to keep the change and reverts it when not answered
  (helix-comp's confirmation). A refused configuration is a toast with
  the reason. A change from elsewhere replaces what was being edited.
- *Arrangement* (with two or more monitors): *Display Mode* — *Join
  Displays* (each monitor its own logical monitor, side by side, keeping
  what it had), *Mirror* (one logical monitor showing all at the largest
  resolution they all have; a toast when there is none), *Single Display*
  (only the one chosen in *Display* on, the built-in one first). When
  joined: the arrangement area and *Primary Display* ("Holds the top bar
  and new windows"). Monitors are numbered "1. Name" in the order the
  service lists them.
- The arrangement area draws each logical monitor at its logical size
  (mode ÷ scale, turned for portrait orientations), the selected one
  outlined. Dragging moves a monitor; when let go it snaps to the nearest
  place where it touches another without overlapping (`displays::snap`),
  and the whole layout moves so its top-left corner is 0, 0. Keyboard:
  the area takes focus (named "Display Arrangement"), Space selects the
  next monitor, the arrow keys move the selected one by its own width or
  height (snapping the same way); each move is announced ("1. Built-in
  display at 0, 0") and kept as the area's description.
- *Display* group (titled with the monitor's name when there is one;
  with several, *Display* chooses which one): *Orientation* (Landscape,
  Portrait Right = transform 1, Portrait Left = 3, Landscape (Flipped) =
  2), *Resolution* ("1920 × 1080 (16:9)", largest first; mirrored
  monitors offer only resolutions they share and follow the choice with
  their fastest rate), *Refresh Rate* ("60.00 Hz", fastest first;
  "Variable (up to N Hz)" for modes whose `refresh-rate-mode` is
  `variable`), *Scale* (the mode's supported scales, fractional ones
  included, as "125 %"; all logical monitors together when
  `global-scale-required`), *Adjust for TV* (underscanning, shown only for
  monitors that report `is-underscanning`). Changing a size keeps the
  monitors touching (they are laid side by side again when they would
  overlap or drift apart).
- *Login Screen* › *Use This Layout for the Login Screen* (shown when
  `/usr/libexec/helix/helix-login-monitors` and `pkexec` are installed;
  also on the Login Screen panel): the layout shown now, as one
  remembered configuration for the monitors connected now (virtual
  screen-cast monitors left out), goes to `pkexec helix-login-monitors`
  on its standard input (helixdesktop/spec `components/greeter.md`,
  "Setting the login screen's layout"). Toasts: "The login screen will
  use this layout"; nothing for a dismissed prompt (pkexec 126); "Could
  not change the login screen: not authorized" (127); "Could not use
  this layout for the login screen: <reason>" otherwise.
- *Night Light* (`org.gnome.settings-daemon.plugins.color`): *Night
  Light* (`night-light-enabled`), *Schedule* (*Sunset to Sunrise* /
  *Manual Schedule*: `night-light-schedule-automatic`), *From* and *To*
  for a manual schedule (hour and minute spin buttons, named "From,
  Hours"…, written as hours to `night-light-schedule-from` / `-to`),
  *Color Temperature* (`night-light-temperature`, 1700 K "Warmer" …
  4700 K "Cooler"). The compat schema has these keys.
- Tested against a fake DisplayConfig on a private bus (two monitors,
  underscanning, a stale serial), the layout logic in unit tests, and
  against helix-comp's own DisplayConfig in a headless session on a
  private bus (`displays::tests::live_helix_comp`: read, verify, apply a
  fractional scale temporarily and read it back). Not yet tried with
  several real monitors.

#### Sound
The sound server's PulseAudio interface through `pactl` (as the shell's
volume keys use it; PipeWire serves it with pipewire-pulse): `pactl -f
json list sinks|sources|sink-inputs|cards`, `get-default-sink/source`,
and the `set-*` commands (`sound.rs`). The page follows `pactl subscribe`
(sink, source, card and server events, read again at most every
0.15 s).

- Until the first reading: *Loading Sound…*; without `pactl` or a
  server, *Sound Is Not Available* / "No sound server is running."
- *Output*: *Output Device* (the outputs by description; choosing one is
  `set-default-sink`), *Volume* with a mute toggle ("Mute Output") and a
  slider ("Output Volume", value text "N %") to 100 %, or 150 % with
  over-amplification (a "100 %" mark then), *Balance* (left … right, for
  outputs with front-left and front-right channels: the far channel is
  turned down; left and right channels of every row — front, rear,
  side — follow it), *Fade* (rear … front, for outputs with front and
  rear channels: the far row is turned down; centre and subwoofer follow
  the front row only when faded to the rear), *Output Port* (the
  device's ports — speakers, headphones, line out —, the plugged-in ones
  and the one in use, `set-sink-port`; shown when there are two or
  more), *Configuration* (the card's available profiles, by
  priority — HDMI or analogue, A2DP or HFP —, `set-card-profile`; shown
  when there are two or more), *Test Speakers* (a dialog with one button
  per channel; each plays a 0.6 s tone on that channel alone with
  `pacat --channel-map=<position>`).
- *Input*: device, *Input Port* (`set-source-port`), volume and mute as
  for output (monitors of outputs are not inputs), *Input Level* (a level bar fed by `parec` of the input,
  20 readings a second, falling back slowly; it runs only while the page
  is on screen), *Configuration*.
- *Apps*: one row per playing stream (event sounds, speaker tests and
  meters left out), the app's name, what it plays, a mute toggle ("Mute
  APP") and a volume slider ("Volume of APP"); "No apps are playing
  sound." when there are none.
- *Sounds*: *Over-Amplification* (`org.gnome.desktop.sound
  allow-volume-above-100-percent`), *System Sounds* (`event-sounds`),
  *System Sounds Volume* (a slider, "System Sounds Volume", 0–100 %,
  dimmed while system sounds are off; see below), *Alert Sound*: *Default* (the theme's bell) or one of the sounds in
  `sounds/gnome/default/alerts` of the data directories (by file name).
  Choosing one writes a sound theme `__custom` in
  `$XDG_DATA_HOME/sounds` that inherits from the previous theme and links
  `bell-terminal.oga` and `bell-window-system.oga` to the file, sets
  `theme-name` to it and plays the sound; *Default* removes the theme and
  restores the inherited name.
- The system sounds volume is PulseAudio's stream-restore entry
  `sink-input-by-media-role:event`, which GNOME Settings and pavucontrol
  set through libpulse's ext-stream-restore. `pactl` cannot reach that
  extension, so the panel goes one level down: pipewire-pulse keeps those
  entries in PipeWire's `route-settings` metadata (WirePlumber makes it,
  stores what is written there and applies it to each new event sound),
  under `restore.stream.Output/Audio.media.role:Notification`
  (pipewire-pulse's name for the `event` role). The panel reads it with
  `pw-metadata -n route-settings 0`, follows it with `pw-metadata -m`,
  and writes the entry pipewire-pulse would write: `{ "mute": …,
  "volumes": [ v, v ], "channels": [ "FL", "FR" ] }`, linear amplitudes
  (v = slider³, PulseAudio's cubic scale; the mute flag is kept as it
  was). Stereo rather than mono because WirePlumber applies the volumes
  to a new sound by channel position, and a mono entry leaves the right
  channel of a stereo event sound silent. GNOME Settings and pavucontrol
  read the same entry, and the panel shows theirs (any layout; the
  loudest channel). No entry is 100 %. Event sounds playing at that
  moment are set too (`set-sink-input-volume`), as PulseAudio's
  stream-restore does. Without the metadata object (no WirePlumber, or
  PulseAudio itself) or without `pw-metadata`, the row is hidden.
- Slider changes go out one at a time (only the newest waiting one is
  kept), and readings leave a slider alone for 0.8 s after the user
  moved it.
- Tested against a private PipeWire with pipewire-pulse in a temporary
  runtime directory, with virtual outputs and input (`sound.rs`), and
  against the fake audio session of `crates/helix-test-audio` (a private
  PipeWire with null speakers, a 5.1 sink and a microphone, and a minimal
  linking policy; no hardware): the speaker test is recorded from the
  5.1 sink's monitor and sounds on its channel only, for each of the six
  channels, and the input level stays at zero until a test source plays
  into the microphone, then rises. Fade, ports and profiles are tested on
  `pactl` output (null devices have neither ports nor cards). The system
  sounds volume is tested against the fake audio session's
  `route-settings` metadata (set from the panel, read back, followed when
  another program writes a mono entry as pipewire-pulse does for
  libpulse, and when it is removed), on `pw-metadata` output, and as
  unavailable on a PipeWire without the metadata object. It was also
  checked by hand against a private PipeWire with WirePlumber (hardware
  monitors off) and pipewire-pulse: libpulse's ext-stream-restore (from
  a ctypes script) writes and reads the same key and values, an event
  sound started afterwards gets the volume, one playing is set at once,
  and WirePlumber keeps it in its `stream-properties` state.

#### Bluetooth
BlueZ (`org.bluez` on the system bus: the object manager at `/`,
`org.bluez.Adapter1`, `Device1`, `Battery1`). The panel uses the first
adapter (by object path, normally `hci0`).

- Until the first reading the page shows *Loading Bluetooth…*. Then one
  of: *Bluetooth Is Not Available* (BlueZ is not running), *No Bluetooth
  Adapter Found*, *Bluetooth Is Turned Off by a Hardware Switch* (every
  Bluetooth radio in `/sys/class/rfkill` is hard-blocked and the adapter
  is off), or the list below. The page follows BlueZ live (any signal it
  sends, and it coming or going, read again at most every 0.25 s).
- *Bluetooth*: a switch for the adapter's `Powered`. Turning it on while
  a radio is soft-blocked first lifts the block (an `RFKILL_OP_CHANGE_ALL`
  event for Bluetooth written to `/dev/rfkill`). Its subtitle: "Turn on to
  connect devices and receive files" when off, "Visible to nearby devices
  as “NAME” while this page is open" when discoverable.
- *Visible Name*: an entry row with an apply button for the adapter's
  `Alias`.
- While the page is on screen and the adapter is on, the adapter is made
  `Discoverable` (only if it was not; set back when the page goes away)
  and searches for devices (`StartDiscovery`, `StopDiscovery` when the
  page goes away).
- *Devices*: paired devices by name, each with its kind's icon and a
  status ("Connected", "Not connected", with "· Battery N %" when
  connected and BlueZ reports a battery). Activating one opens a dialog:
  *Connection* (switch: `Connect` / `Disconnect`), *Type* (from BlueZ's
  `Icon`: Headset, Keyboard, Phone…), *Address*, *Battery*, *Send Files…*
  (the shell's `org.helixdesktop.BluetoothSendTo.SendFiles(address, [])`, which
  asks for the files and shows progress; a toast says sending needs the
  Helix shell when it is not there) and *Forget…* (confirmed, then
  `Adapter1.RemoveDevice`). The dialog follows the device and closes when
  it goes away.
- *Nearby Devices*: unpaired devices that have a name (BlueZ's made-up
  alias from the address does not count), with a spinner while
  searching. Activating one pairs it: "Pairing…" while `Pair()` runs (the
  shell's pairing agent asks for codes, so the call has no time limit),
  then `Trusted` and `Connect()`; a failure gives a toast "Could not pair
  with NAME: reason" ("Pairing with NAME was cancelled" when refused),
  success is announced ("Paired with NAME").
- *Received Files*: files other devices send are saved in the Downloads
  folder by the shell (helixdesktop/spec `contracts/dbus/org.bluez.obex.md`);
  *Open* shows the folder.
- Errors of user actions are toasts ("Could not turn Bluetooth on: …");
  polkit or user refusals are not reported.
- Tested against a fake BlueZ and a fake shell on a private bus
  (`bluez.rs`); not yet tried with a real adapter.
- Not yet: choosing between several adapters, the per-device details
  of input devices (mouse and keyboard settings links), device-specific
  audio profiles (those belong in Sound).

#### Printers
CUPS's printers, their jobs and default options, as in GNOME Settings'
Printers panel. Reading goes to CUPS over IPP (`CUPS_SERVER`, else
`/run/cups/cups.sock`, else `localhost:631`; `cups.rs`): CUPS-Get-Printers,
CUPS-Get-Default, Get-Jobs, Get-Printer-Attributes and Print-Job. Changes
go through cups-pk-helper (`org.opensuse.CupsPkHelper.Mechanism` at `/` on
the system bus, every call interactive so polkit can ask): `PrinterSetDefault`,
`PrinterSetEnabled`, `PrinterAdd` (model `everywhere`, IPP Everywhere),
`PrinterDelete`, `PrinterAddOptionDefault`, `JobCancelPurge` (not
purged), `JobSetHoldUntil` (`indefinite` / `no-hold`) and `DevicesGet`.

- Until the first reading the page shows *Loading Printers…*. Then one of:
  *Printing Is Not Installed* (no `/usr/sbin/cupsd`), *Printing Is Not
  Available* (CUPS does not answer), or the list. CUPS sends plain clients
  no change signals: the page reads it again every 3 s while it or a
  printer's dialog is on screen, and when it is shown.
- *Printers*, with *Add Printer…* in the header: each printer by its
  description (`printer-info`, else its queue name), with a status
  ("Ready", "Printing", "Paused", or the first state reason people can
  act on: out of paper, paper jam, out of or low on toner or ink, door or
  cover open, offline, connecting; `-report` reasons are not shown), "·
  Location" when it has one, and "Default" on the default printer. "No
  printers are set up yet." when there are none.
- Activating a printer opens its dialog: *Status*, *Model*, *Location*,
  *Address* (the device URI); *Default Printer* (*Make Default*, "Default"
  and insensitive when it is); *Paused* (a switch: `PrinterSetEnabled`);
  *Default Options*, a row for each of paper size (`media`, PWG names
  shown as their size: "A4", "Letter"), two-sided (`sides`), color
  (`print-color-mode`) and quality (`print-quality`: Draft, Normal,
  High) that offers more than one choice, set as the printer's default
  for that option; *Jobs*, the jobs not yet completed with their state
  (Waiting, Held, Printing, Stopped) and owner, each with *Hold* / *Resume*
  and *Cancel Job* buttons ("No jobs are waiting." when there are none);
  *Test Page* (*Print*: CUPS's own `/usr/share/cups/data/testprint`, typed
  by CUPS, then a toast "Test page sent to NAME"); *Remove Printer*
  (confirmed, then `PrinterDelete`). The dialog follows the printer and
  closes when it goes away.
- *Add Printer*: *Printers Found*, the printers CUPS finds within 10 s
  (`DevicesGet`) that need no driver (`ipp://`, `ipps://` and `dnssd://`
  URIs: network printers and IPP-over-USB ones) and are not set up yet,
  with a spinner while it looks ("No other printers were found. Make sure
  the printer is on and connected." when none); and *Add by Address*: a
  host name or IP address (`ipp://HOST/ipp/print`) or an `ipp://` /
  `ipps://` URI. Adding names the queue after the printer (characters
  CUPS does not allow become `_`, unique among the queues), says "Adding
  NAME…" and then "Added NAME".
- Errors of user actions are toasts ("Could not pause NAME: …"); polkit or
  user refusals are not reported. Without cups-pk-helper, changes say
  "Changing printers needs cups-pk-helper, which is not installed".
- Tested against a fake IPP server and a fake cups-pk-helper on a private
  bus (`cups.rs`); also against a private, scratch `cupsd` with a virtual
  IPP Everywhere printer (`ippeveprinter`), and through the real
  cups-pk-helper on a private bus with polkit faked to allow
  (`against_a_real_cups`, `through_the_real_helper`, both `--ignored`).
  Not yet tried with a real printer.
- Not yet: printers that need a driver (a PPD to choose: plain `usb://`
  ones and older network printers), classes, sharing, renaming, ink
  levels (`marker-levels`), and jobs of all users needing authentication
  to cancel (cups-pk-helper asks polkit for those).

#### Date & Time
- Reads `org.freedesktop.timedate1` (`Timezone`, `NTP`, `CanNTP`,
  `NTPSynchronized`) on a worker thread. A change of the sync status on
  a later reading ("Synchronized with network time servers") is announced.
  When timedated cannot be reached the rows say *Unavailable* and a toast
  says so once.
- *Automatic Date & Time* → `SetNTP(b, interactive=true)` (insensitive when
  `CanNTP` is false). *Automatic Time Zone* →
  `org.gnome.desktop.datetime automatic-timezone`.
- *Date & Time* row shows the current time in the system time zone, updated
  every second, in the selected clock format. Setting the clock manually
  (`SetTime`) is not offered yet.
- *Time Zone* opens a searchable dialog of time zones (from
  `ListTimezones`, falling back to `/usr/share/zoneinfo/zone1970.tab`),
  titled the way people say them — "Berlin, Germany", "Buenos Aires,
  Argentina" (country from `zone.tab` + `iso3166.tab`; zones without a
  country keep their id) — with the id as subtitle, sorted by title and
  searchable by either;
  choosing one calls `SetTimezone(tz, interactive=true)`. Errors (e.g. polkit
  denial) are shown as a toast and the displayed value is reloaded.
- *Time Format* 24-hour / AM/PM (`interface clock-format`).
- Clock & Calendar: *Week Day*, *Date*, *Seconds*
  (`interface clock-show-weekday` / `clock-show-date` /
  `clock-show-seconds`), *Week Numbers*
  (`org.gnome.desktop.calendar show-weekdate`).

#### About
- *Device Name* (`org.freedesktop.hostname1` `PrettyHostname`, else
  `Hostname`) in an entry row (read-only, with a toast and tooltip saying
  why, when hostnamed cannot be reached); applying calls `SetPrettyHostname(name, true)`
  and `SetStaticHostname(static, true)` where *static* is the name
  lower-cased, spaces/underscores → `-`, other characters outside
  `[a-z0-9-]` dropped, repeated `-` collapsed, leading/trailing `-` trimmed,
  at most 64 characters (empty → `localhost`).
- Hardware model (`HardwareVendor` + `HardwareModel`, falling back to
  `/sys/class/dmi/id/{sys_vendor,product_name}`), memory (`MemTotal` of
  `/proc/meminfo`), processor (`model name` of `/proc/cpuinfo` × logical
  CPUs), disk capacity (`filesystem::size` of `/`), OS name (`PRETTY_NAME`
  of `/etc/os-release`, else `NAME VERSION`), OS type (64-bit / 32-bit),
  desktop (`XDG_CURRENT_DESKTOP`), windowing system (Wayland / X11 from the
  GDK display), kernel (`/proc/sys/kernel/osrelease`).
- *Copy* puts all of it on the clipboard as text and confirms with a
  "System information copied" toast (which screen readers speak).

### D-Bus
- All D-Bus traffic uses `zbus` (blocking API) on worker threads; results
  return to the GTK thread over `async-channel`. Missing services leave the
  affected rows showing *Unavailable*.
- Mutating system calls (`SetTimezone`, `SetNTP`, `SetPrettyHostname`,
  `SetStaticHostname`) are issued only in response to an explicit user
  action, with `interactive = true` so polkit can prompt.

### Shortcuts
`Ctrl+F` search, `Escape` close search, `Ctrl+Q` quit, `Ctrl+W` close
window, `F1` about, `Ctrl+?` Keyboard Shortcuts window (also in the
primary menu). The window is built from a table
(`helix_app_common::shortcuts`) that a unit test checks against the
registered accelerators.

### Command line
- `helix-settings-app [PANEL]` opens (or raises the running instance on)
  the given panel; ids and aliases as in the registry table. A page name
  (`agent-access`, `agent-activity`) opens its panel on that page.
- `helix-settings-app --list` prints `id<TAB>title<TAB>available|unavailable`
  for every panel (`login-screen` unavailable when it is hidden), then
  every page (unavailable when its schema is not installed), and exits.
- A hidden panel's name (`login-screen` on a system without the Helix
  greeter) opens the default panel; asked for before the check below has
  answered, the panel opens once it says the panel is shown.
- Unknown panel names print an error with the list of ids and exit with
  status 2.
- `-h` / `--help` prints usage.

### Follow-up panels
- Not listed yet: Region & Language, Online Accounts, Sharing,
  Search, Color, Wacom, power mode (`org.freedesktop.UPower.PowerProfiles`).

## Decisions
- Login Screen: edits are staged and sent with one *Apply…* rather than
  unlocking first (as Users does): the file is world-readable, so the
  controls can always show and take edits, and the administrator prompt
  comes once, for exactly the changes being made, from one helper run.
  An *Unlock* button would need a second privileged step (polkit's
  authorization is per helper run through `pkexec`) and would leave the
  user unsure what had been written.
- Style cards: no third *Light* card; match GNOME with *Default* and *Dark*
  only (`prefer-light` set elsewhere shows as *Default* and is kept).
- Focus is kept across list rebuilds (`helix_app_common::focus`): Input sources: Move Up / Move Down keep focus on the moved source's button (so repeated presses keep moving it; at the top or bottom focus stays in the row), Remove moves focus to the neighbouring source's Remove button, or to *Add Input Source…* when no source is left.
- Per-row buttons name their row for assistive technology: "Move English
  (US) Up", "Remove English (US)"; an extension's buttons "Show Log for X",
  "Restart X" and "Change Shortcut for Command (X)".
