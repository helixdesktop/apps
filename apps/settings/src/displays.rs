//! Monitors through `org.gnome.Mutter.DisplayConfig` on the session bus
//! (served by helix-comp; helixdesktop/spec
//! `contracts/dbus/org.gnome.Mutter.DisplayConfig.md`), and the layout
//! logic of the Displays panel (pure). Blocking zbus; worker threads only.

use std::collections::HashMap;

use zbus::blocking::Connection;
use zbus::zvariant::OwnedValue;

use crate::bus::{self, get, Obj, Props, Result};

pub const NAME: &str = "org.gnome.Mutter.DisplayConfig";
const DC: Obj<'static> = Obj::new(NAME, "/org/gnome/Mutter/DisplayConfig", NAME);

/// `ApplyMonitorsConfig` methods.
pub const VERIFY: u32 = 0;
pub const PERSISTENT: u32 = 2;

/// A monitor's connector, vendor, product and serial.
pub type Spec = (String, String, String, String);
pub type WireMode = (String, i32, i32, f64, f64, Vec<f64>, Props);
pub type WireMonitor = (Spec, Vec<WireMode>, Props);
pub type WireLogical = (i32, i32, f64, u32, bool, Vec<Spec>, Props);
/// `GetCurrentState`'s reply.
pub type State = (u32, Vec<WireMonitor>, Vec<WireLogical>, Props);

/// `GetCurrentState` as it comes.
pub fn current_state(conn: &Connection) -> Result<State> {
    DC.call(conn, "GetCurrentState", &())
}

#[derive(Debug, Clone, PartialEq)]
pub struct Mode {
    pub id: String,
    pub width: i32,
    pub height: i32,
    pub refresh: f64,
    pub scales: Vec<f64>,
    pub preferred_scale: f64,
    pub preferred: bool,
    pub current: bool,
    /// A variable refresh rate mode (`refresh-rate-mode` `variable`):
    /// `refresh` is then its highest rate.
    pub variable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Monitor {
    pub connector: String,
    pub name: String,
    pub builtin: bool,
    pub modes: Vec<Mode>,
    /// `None` when it cannot underscan.
    pub underscan: Option<bool>,
}

impl Monitor {
    pub fn current(&self) -> Option<&Mode> {
        self.modes.iter().find(|m| m.current)
    }

    pub fn preferred(&self) -> Option<&Mode> {
        self.modes
            .iter()
            .find(|m| m.preferred)
            .or(self.modes.first())
    }

    pub fn mode(&self, id: &str) -> Option<&Mode> {
        self.modes.iter().find(|m| m.id == id)
    }

    /// Resolutions, largest first, without repeats.
    pub fn resolutions(&self) -> Vec<(i32, i32)> {
        let mut r: Vec<(i32, i32)> = self.modes.iter().map(|m| (m.width, m.height)).collect();
        r.sort_by(|a, b| (b.0 * b.1).cmp(&(a.0 * a.1)).then(b.0.cmp(&a.0)));
        r.dedup();
        r
    }

    /// Modes of one resolution, fastest first.
    pub fn rates(&self, w: i32, h: i32) -> Vec<&Mode> {
        let mut r: Vec<&Mode> = self
            .modes
            .iter()
            .filter(|m| m.width == w && m.height == h)
            .collect();
        r.sort_by(|a, b| b.refresh.total_cmp(&a.refresh));
        r
    }
}

/// One monitor's place in a configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct Output {
    pub connector: String,
    pub mode: String,
    pub underscan: bool,
}

/// A logical monitor: one place on the desktop showing one monitor, or
/// several mirroring each other.
#[derive(Debug, Clone, PartialEq)]
pub struct Logical {
    pub x: i32,
    pub y: i32,
    pub scale: f64,
    pub transform: u32,
    pub primary: bool,
    pub outputs: Vec<Output>,
}

/// A whole configuration, as read or as about to be applied.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Config {
    pub serial: u32,
    pub monitors: Vec<Monitor>,
    pub logical: Vec<Logical>,
    pub global_scale: bool,
}

pub fn read(conn: &Connection) -> Result<Config> {
    let (serial, monitors, logical, props) = current_state(conn)?;
    let monitors: Vec<Monitor> = monitors
        .into_iter()
        .map(|(spec, modes, p)| Monitor {
            connector: spec.0.clone(),
            name: get::<String>(&p, "display-name")
                .filter(|n| !n.is_empty())
                .unwrap_or(spec.0),
            builtin: get(&p, "is-builtin").unwrap_or(false),
            underscan: get(&p, "is-underscanning"),
            modes: modes
                .into_iter()
                .map(
                    |(id, width, height, refresh, preferred_scale, scales, mp)| Mode {
                        id,
                        width,
                        height,
                        refresh,
                        scales,
                        preferred_scale,
                        preferred: get(&mp, "is-preferred").unwrap_or(false),
                        current: get(&mp, "is-current").unwrap_or(false),
                        variable: get::<String>(&mp, "refresh-rate-mode").as_deref()
                            == Some("variable"),
                    },
                )
                .collect(),
        })
        .collect();
    let logical = logical
        .into_iter()
        .map(|(x, y, scale, transform, primary, specs, _)| Logical {
            x,
            y,
            scale,
            transform,
            primary,
            outputs: specs
                .into_iter()
                .filter_map(|s| {
                    let m = monitors.iter().find(|m| m.connector == s.0)?;
                    Some(Output {
                        connector: s.0.clone(),
                        mode: m.current().map(|c| c.id.clone()).unwrap_or_default(),
                        underscan: m.underscan.unwrap_or(false),
                    })
                })
                .collect(),
        })
        .collect();
    Ok(Config {
        serial,
        monitors,
        logical,
        global_scale: get(&props, "global-scale-required").unwrap_or(false),
    })
}

/// Whether changing the configuration is allowed at all.
pub fn allowed(conn: &Connection) -> bool {
    DC.props(conn)
        .ok()
        .and_then(|p| get::<bool>(&p, "ApplyMonitorsConfigAllowed"))
        .unwrap_or(true)
}

/// Applies `config` (`method`: [`VERIFY`] or [`PERSISTENT`]; the shell asks
/// the user to keep a persistent change and reverts it otherwise).
pub fn apply(conn: &Connection, config: &Config, method: u32) -> Result<()> {
    type WireOut = (String, String, HashMap<String, OwnedValue>);
    let logical: Vec<(i32, i32, f64, u32, bool, Vec<WireOut>)> = config
        .logical
        .iter()
        .map(|l| {
            (
                l.x,
                l.y,
                l.scale,
                l.transform,
                l.primary,
                l.outputs
                    .iter()
                    .map(|o| {
                        let mut p = HashMap::new();
                        let can = config
                            .monitors
                            .iter()
                            .find(|m| m.connector == o.connector)
                            .is_some_and(|m| m.underscan.is_some());
                        if can {
                            p.insert("underscanning".to_owned(), OwnedValue::from(o.underscan));
                        }
                        (o.connector.clone(), o.mode.clone(), p)
                    })
                    .collect(),
            )
        })
        .collect();
    DC.run(
        conn,
        "ApplyMonitorsConfig",
        &(
            config.serial,
            method,
            logical,
            HashMap::<String, OwnedValue>::new(),
        ),
    )
}

/// Calls `changed` whenever the monitors change, or the service comes or
/// goes.
pub fn watch(conn: &Connection, changed: impl FnMut() -> bool) {
    bus::watch(conn, &[NAME], changed)
}

// --- Layout (pure) ---

/// The size a logical monitor takes on the desktop.
pub fn logical_size(config: &Config, l: &Logical) -> (i32, i32) {
    let Some(o) = l.outputs.first() else {
        return (0, 0);
    };
    let Some(m) = config
        .monitors
        .iter()
        .find(|m| m.connector == o.connector)
        .and_then(|m| m.mode(&o.mode))
    else {
        return (0, 0);
    };
    let (w, h) = (
        (m.width as f64 / l.scale).round() as i32,
        (m.height as f64 / l.scale).round() as i32,
    );
    if l.transform % 2 == 1 {
        (h, w)
    } else {
        (w, h)
    }
}

type Rect = (i32, i32, i32, i32);

fn rect(config: &Config, l: &Logical) -> Rect {
    let (w, h) = logical_size(config, l);
    (l.x, l.y, w, h)
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

fn touches(a: Rect, b: Rect) -> bool {
    let x = a.0 <= b.0 + b.2 && b.0 <= a.0 + a.2;
    let y = a.1 <= b.1 + b.3 && b.1 <= a.1 + a.3;
    x && y && !overlaps(a, b)
}

/// Moves logical monitor `i` to the nearest place next to the others
/// (touching one, overlapping none), then moves everything so the
/// top-left corner is at 0, 0.
pub fn snap(config: &mut Config, i: usize) {
    let n = config.logical.len();
    if n > 1 && i < n {
        let me = rect(config, &config.logical[i]);
        let others: Vec<Rect> = (0..n)
            .filter(|j| *j != i)
            .map(|j| rect(config, &config.logical[j]))
            .collect();
        let mut best: Option<(i64, i32, i32)> = None;
        for o in &others {
            // Each side of each other monitor, sliding along it.
            let candidates = [
                (o.0 - me.2, me.1.clamp(o.1 - me.3, o.1 + o.3)),
                (o.0 + o.2, me.1.clamp(o.1 - me.3, o.1 + o.3)),
                (me.0.clamp(o.0 - me.2, o.0 + o.2), o.1 - me.3),
                (me.0.clamp(o.0 - me.2, o.0 + o.2), o.1 + o.3),
            ];
            for (x, y) in candidates {
                let r = (x, y, me.2, me.3);
                if others.iter().any(|q| overlaps(r, *q)) || !others.iter().any(|q| touches(r, *q))
                {
                    continue;
                }
                let d =
                    (x - me.0) as i64 * (x - me.0) as i64 + (y - me.1) as i64 * (y - me.1) as i64;
                if best.is_none_or(|b| d < b.0) {
                    best = Some((d, x, y));
                }
            }
        }
        if let Some((_, x, y)) = best {
            config.logical[i].x = x;
            config.logical[i].y = y;
        }
    }
    normalize(config);
}

/// Moves everything so the top-left corner is at 0, 0.
pub fn normalize(config: &mut Config) {
    let min_x = config.logical.iter().map(|l| l.x).min().unwrap_or(0);
    let min_y = config.logical.iter().map(|l| l.y).min().unwrap_or(0);
    for l in &mut config.logical {
        l.x -= min_x;
        l.y -= min_y;
    }
}

/// Places the logical monitors side by side, left to right in their
/// current order, tops aligned.
pub fn side_by_side(config: &mut Config) {
    let mut order: Vec<usize> = (0..config.logical.len()).collect();
    order.sort_by_key(|i| (config.logical[*i].x, config.logical[*i].y));
    let mut x = 0;
    for i in order {
        let (w, _) = logical_size(config, &config.logical[i]);
        config.logical[i].x = x;
        config.logical[i].y = 0;
        x += w;
    }
}

/// How the monitors are used together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrangement {
    /// One desktop across all monitors.
    Join,
    /// The same picture on all.
    Mirror,
    /// Only one monitor on.
    Single,
}

impl Config {
    pub fn monitor(&self, connector: &str) -> Option<&Monitor> {
        self.monitors.iter().find(|m| m.connector == connector)
    }

    pub fn arrangement(&self) -> Arrangement {
        let on: usize = self.logical.iter().map(|l| l.outputs.len()).sum();
        if self.logical.len() == 1 && on > 1 {
            Arrangement::Mirror
        } else if on == 1 && self.monitors.len() > 1 {
            Arrangement::Single
        } else {
            Arrangement::Join
        }
    }

    /// The logical monitor showing `connector`.
    pub fn logical_of(&self, connector: &str) -> Option<usize> {
        self.logical
            .iter()
            .position(|l| l.outputs.iter().any(|o| o.connector == connector))
    }

    /// Every monitor on, each its own logical monitor at its current (or
    /// preferred) mode, side by side.
    pub fn join(&mut self) {
        let old = self.logical.clone();
        let mut logical = Vec::new();
        for m in &self.monitors {
            let was = old
                .iter()
                .find(|l| l.outputs.iter().any(|o| o.connector == m.connector));
            let Some(mode) = m.current().or(m.preferred()) else {
                continue;
            };
            logical.push(Logical {
                x: was.map_or(i32::MAX, |l| l.x),
                y: was.map_or(0, |l| l.y),
                scale: was.map_or(mode.preferred_scale.max(1.0), |l| l.scale),
                transform: was.map_or(0, |l| l.transform),
                primary: was.is_some_and(|l| l.primary),
                outputs: vec![Output {
                    connector: m.connector.clone(),
                    mode: mode.id.clone(),
                    underscan: m.underscan.unwrap_or(false),
                }],
            });
        }
        if !logical.iter().any(|l| l.primary) {
            if let Some(l) = logical.first_mut() {
                l.primary = true;
            }
        }
        self.logical = logical;
        side_by_side(self);
    }

    /// All monitors showing the same picture at the largest resolution
    /// they share; `false` when they share none.
    pub fn mirror(&mut self) -> bool {
        let Some(first) = self.monitors.first() else {
            return false;
        };
        let common: Vec<(i32, i32)> = first
            .resolutions()
            .into_iter()
            .filter(|r| self.monitors.iter().all(|m| m.resolutions().contains(r)))
            .collect();
        let Some(&(w, h)) = common.first() else {
            return false;
        };
        let outputs: Vec<Output> = self
            .monitors
            .iter()
            .filter_map(|m| {
                Some(Output {
                    connector: m.connector.clone(),
                    mode: m.rates(w, h).first()?.id.clone(),
                    underscan: m.underscan.unwrap_or(false),
                })
            })
            .collect();
        let scale = self.logical.first().map_or(1.0, |l| l.scale);
        let scales = self.monitors[0].rates(w, h)[0].scales.clone();
        let scale = if scales.iter().any(|s| (s - scale).abs() < 1e-6) {
            scale
        } else {
            1.0
        };
        self.logical = vec![Logical {
            x: 0,
            y: 0,
            scale,
            transform: 0,
            primary: true,
            outputs,
        }];
        true
    }

    /// Only `connector` on.
    pub fn single(&mut self, connector: &str) {
        let Some(m) = self.monitor(connector).cloned() else {
            return;
        };
        let Some(mode) = m.current().or(m.preferred()).cloned() else {
            return;
        };
        let was = self.logical_of(connector).map(|i| self.logical[i].clone());
        // Keep the mode it shows now, if it is on.
        let output = was
            .as_ref()
            .and_then(|l| l.outputs.iter().find(|o| o.connector == connector).cloned())
            .unwrap_or(Output {
                connector: connector.into(),
                mode: mode.id.clone(),
                underscan: m.underscan.unwrap_or(false),
            });
        self.logical = vec![Logical {
            x: 0,
            y: 0,
            scale: was
                .as_ref()
                .map_or(mode.preferred_scale.max(1.0), |l| l.scale),
            transform: was.as_ref().map_or(0, |l| l.transform),
            primary: true,
            outputs: vec![output],
        }];
    }

    pub fn set_primary(&mut self, index: usize) {
        for (i, l) in self.logical.iter_mut().enumerate() {
            l.primary = i == index;
        }
    }

    /// Sets the mode of every monitor in logical monitor `i` (mirrored
    /// monitors take the same resolution and their fastest rate for it),
    /// keeping the scale if the new mode offers it, and lays the others
    /// out again.
    pub fn set_mode(&mut self, i: usize, connector: &str, mode_id: &str) {
        let Some(mode) = self
            .monitor(connector)
            .and_then(|m| m.mode(mode_id))
            .cloned()
        else {
            return;
        };
        let monitors = self.monitors.clone();
        let Some(l) = self.logical.get_mut(i) else {
            return;
        };
        for o in &mut l.outputs {
            if o.connector == connector {
                o.mode = mode.id.clone();
            } else if let Some(m) = monitors.iter().find(|m| m.connector == o.connector) {
                if let Some(r) = m.rates(mode.width, mode.height).first() {
                    o.mode = r.id.clone();
                }
            }
        }
        if !mode.scales.iter().any(|s| (s - l.scale).abs() < 1e-6) {
            l.scale = mode.preferred_scale.max(1.0);
        }
        self.relayout();
    }

    pub fn set_scale(&mut self, i: usize, scale: f64) {
        if self.global_scale {
            for l in &mut self.logical {
                l.scale = scale;
            }
        } else if let Some(l) = self.logical.get_mut(i) {
            l.scale = scale;
        }
        self.relayout();
    }

    pub fn set_transform(&mut self, i: usize, transform: u32) {
        if let Some(l) = self.logical.get_mut(i) {
            l.transform = transform;
        }
        self.relayout();
    }

    /// After a size change, monitors may overlap or float apart: keep the
    /// order, place them side by side again when they do.
    fn relayout(&mut self) {
        let rects: Vec<Rect> = self.logical.iter().map(|l| rect(self, l)).collect();
        let bad = (0..rects.len()).any(|i| {
            (0..rects.len()).any(|j| i != j && overlaps(rects[i], rects[j]))
                || (rects.len() > 1
                    && !(0..rects.len()).any(|j| i != j && touches(rects[i], rects[j])))
        });
        if bad {
            side_by_side(self);
        }
        normalize(self);
    }

    /// The scales logical monitor `i` can use (those its first monitor's
    /// mode offers).
    pub fn scales(&self, i: usize) -> Vec<f64> {
        self.logical
            .get(i)
            .and_then(|l| l.outputs.first())
            .and_then(|o| self.monitor(&o.connector)?.mode(&o.mode))
            .map(|m| m.scales.clone())
            .unwrap_or_else(|| vec![1.0])
    }
}

/// "1920 × 1080 (16:9)".
pub fn resolution_label(w: i32, h: i32) -> String {
    fn gcd(a: i32, b: i32) -> i32 {
        if b == 0 {
            a
        } else {
            gcd(b, a % b)
        }
    }
    let g = gcd(w, h).max(1);
    let (a, b) = (w / g, h / g);
    // 8:5 is said 16:10; odd ratios are left out.
    let aspect = match (a, b) {
        (8, 5) => Some("16:10".to_string()),
        (64, 27) | (43, 18) | (12, 5) => Some("21:9".to_string()),
        (a, b) if a <= 21 && b <= 10 => Some(format!("{a}:{b}")),
        _ => None,
    };
    match aspect {
        Some(r) => format!("{w} × {h} ({r})"),
        None => format!("{w} × {h}"),
    }
}

/// "60.00 Hz", or "Variable (up to 144.00 Hz)".
pub fn rate_label(m: &Mode) -> String {
    let hz = format!("{} Hz", helix_app_common::i18n::format_number(m.refresh, 2));
    if m.variable {
        helix_app_common::trf!("Variable (up to {rate})", rate = hz)
    } else {
        hz
    }
}

/// "125 %".
pub fn scale_label(s: f64) -> String {
    format!("{} %", (s * 100.0).round())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::bus::testbus;
    use std::sync::{Arc, Mutex};
    use zbus::object_server::SignalEmitter;

    pub type Log = Arc<Mutex<Vec<String>>>;
    type WireConfig = (i32, i32, f64, u32, bool, Vec<(String, String, Props)>);

    fn props(pairs: Vec<(&str, OwnedValue)>) -> Props {
        pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
    }

    fn mode(w: i32, h: i32, hz: f64, current: bool, preferred: bool) -> WireMode {
        let scales = if w >= 2560 {
            vec![1.0, 1.25, 1.5, 2.0]
        } else {
            vec![1.0, 1.25]
        };
        (
            format!("{w}x{h}@{hz:.3}"),
            w,
            h,
            hz,
            1.0,
            scales,
            props(vec![
                ("is-current", OwnedValue::from(current)),
                ("is-preferred", OwnedValue::from(preferred)),
            ]),
        )
    }

    pub struct FakeDisplayConfig {
        pub serial: u32,
        pub log: Log,
    }

    #[zbus::interface(name = "org.gnome.Mutter.DisplayConfig")]
    impl FakeDisplayConfig {
        fn get_current_state(&self) -> State {
            let spec =
                |c: &str, p: &str| (c.to_owned(), "HLX".to_owned(), p.to_owned(), "0".to_owned());
            let laptop = (
                spec("eDP-1", "Panel"),
                vec![
                    mode(2560, 1600, 60.0, true, true),
                    mode(1920, 1200, 60.0, false, false),
                ],
                props(vec![
                    ("is-builtin", OwnedValue::from(true)),
                    (
                        "display-name",
                        OwnedValue::from(zbus::zvariant::Str::from("Built-in display")),
                    ),
                ]),
            );
            let tv = (
                spec("HDMI-1", "TV"),
                vec![
                    mode(1920, 1080, 60.0, true, true),
                    mode(1920, 1080, 50.0, false, false),
                    mode(1280, 720, 60.0, false, false),
                    mode(1920, 1200, 60.0, false, false),
                ],
                props(vec![
                    (
                        "display-name",
                        OwnedValue::from(zbus::zvariant::Str::from("Living Room TV")),
                    ),
                    ("is-underscanning", OwnedValue::from(false)),
                ]),
            );
            (
                self.serial,
                vec![laptop, tv],
                vec![
                    (
                        0,
                        0,
                        1.25,
                        0,
                        true,
                        vec![spec("eDP-1", "Panel")],
                        Props::new(),
                    ),
                    (
                        2048,
                        0,
                        1.0,
                        0,
                        false,
                        vec![spec("HDMI-1", "TV")],
                        Props::new(),
                    ),
                ],
                props(vec![("global-scale-required", OwnedValue::from(false))]),
            )
        }

        fn apply_monitors_config(
            &self,
            serial: u32,
            method: u32,
            logical: Vec<WireConfig>,
            _props: Props,
        ) -> zbus::fdo::Result<()> {
            if serial != self.serial {
                return Err(zbus::fdo::Error::InvalidArgs("stale serial".into()));
            }
            let text: Vec<String> = logical
                .iter()
                .map(|(x, y, s, t, p, outs)| {
                    let outs: Vec<String> = outs
                        .iter()
                        .map(|(c, m, p)| {
                            let u: Option<bool> = get(p, "underscanning");
                            format!("{c}:{m}{}", u.map_or(String::new(), |u| format!(":u={u}")))
                        })
                        .collect();
                    format!(
                        "{x},{y} s{s} t{t}{} [{}]",
                        if *p { " primary" } else { "" },
                        outs.join(" ")
                    )
                })
                .collect();
            self.log
                .lock()
                .unwrap()
                .push(format!("Apply {method} {}", text.join("; ")));
            Ok(())
        }

        #[zbus(signal)]
        pub async fn monitors_changed(e: &SignalEmitter<'_>) -> zbus::Result<()>;

        #[zbus(property)]
        fn apply_monitors_config_allowed(&self) -> bool {
            true
        }
    }

    /// A fake DisplayConfig with a laptop panel and a TV, on `conn`.
    pub fn serve(conn: &Connection, log: &Log) {
        conn.object_server()
            .at(
                "/org/gnome/Mutter/DisplayConfig",
                FakeDisplayConfig {
                    serial: 7,
                    log: log.clone(),
                },
            )
            .unwrap();
        conn.request_name(NAME).unwrap();
    }

    #[test]
    fn against_a_fake_display_config() {
        let Some(bus) = testbus::private_bus() else {
            return;
        };
        let app = testbus::connect(&bus);
        assert_eq!(read(&app), Err(bus::Error::Missing));
        let log: Log = Arc::default();
        let changes = testbus::count_changes(&bus, &[NAME]);
        let service = testbus::service(&bus);
        serve(&service, &log);
        let mut c = read(&app).unwrap();
        assert!(allowed(&app));
        assert_eq!(c.serial, 7);
        assert_eq!(c.monitors[0].name, "Built-in display");
        assert_eq!(c.monitors[1].underscan, Some(false));
        assert_eq!(c.arrangement(), Arrangement::Join);
        assert_eq!(logical_size(&c, &c.logical[0]), (2048, 1280));
        assert_eq!(
            c.monitors[1].resolutions(),
            [(1920, 1200), (1920, 1080), (1280, 720)]
        );

        // The live signal is followed.
        let before = changes.load(std::sync::atomic::Ordering::SeqCst);
        let iface = service
            .object_server()
            .interface::<_, FakeDisplayConfig>("/org/gnome/Mutter/DisplayConfig")
            .unwrap();
        zbus::block_on(FakeDisplayConfig::monitors_changed(iface.signal_emitter())).unwrap();
        assert!(testbus::eventually(|| changes
            .load(std::sync::atomic::Ordering::SeqCst)
            > before));

        // Verify, then apply: the TV at 720p and underscanning, primary.
        apply(&app, &c, VERIFY).unwrap();
        c.set_mode(1, "HDMI-1", "1280x720@60.000");
        c.logical[1].outputs[0].underscan = true;
        c.set_primary(1);
        apply(&app, &c, PERSISTENT).unwrap();
        // Mirror at the shared resolution.
        assert!(c.mirror());
        apply(&app, &c, PERSISTENT).unwrap();
        c.single("HDMI-1");
        apply(&app, &c, PERSISTENT).unwrap();
        // A stale serial is refused.
        c.serial = 1;
        assert!(matches!(
            apply(&app, &c, PERSISTENT),
            Err(bus::Error::Other(_))
        ));
        assert_eq!(
            *log.lock().unwrap(),
            [
                "Apply 0 0,0 s1.25 t0 primary [eDP-1:2560x1600@60.000]; 2048,0 s1 t0 [HDMI-1:1920x1080@60.000:u=false]",
                "Apply 2 0,0 s1.25 t0 [eDP-1:2560x1600@60.000]; 2048,0 s1 t0 primary [HDMI-1:1280x720@60.000:u=true]",
                "Apply 2 0,0 s1.25 t0 primary [eDP-1:1920x1200@60.000 HDMI-1:1920x1200@60.000:u=false]",
                "Apply 2 0,0 s1.25 t0 primary [HDMI-1:1920x1200@60.000:u=false]",
            ]
        );
    }

    fn two() -> Config {
        let m = |c: &str, w: i32, h: i32| Monitor {
            connector: c.into(),
            name: c.into(),
            builtin: false,
            underscan: None,
            modes: vec![Mode {
                id: format!("{w}x{h}"),
                width: w,
                height: h,
                refresh: 60.0,
                scales: vec![1.0, 2.0],
                preferred_scale: 1.0,
                preferred: true,
                current: true,
                variable: false,
            }],
        };
        let l = |c: &str, x: i32, y: i32, w: i32, h: i32| Logical {
            x,
            y,
            scale: 1.0,
            transform: 0,
            primary: c == "A",
            outputs: vec![Output {
                connector: c.into(),
                mode: format!("{w}x{h}"),
                underscan: false,
            }],
        };
        Config {
            serial: 1,
            monitors: vec![m("A", 1920, 1080), m("B", 1280, 1024)],
            logical: vec![l("A", 0, 0, 1920, 1080), l("B", 1920, 0, 1280, 1024)],
            global_scale: false,
        }
    }

    #[test]
    fn layout() {
        // Dropped overlapping A: B snaps to the nearest free edge.
        let mut c = two();
        c.logical[1].x = 1500;
        c.logical[1].y = 900;
        snap(&mut c, 1);
        assert_eq!(
            (c.logical[1].x, c.logical[1].y),
            (1500, 1080),
            "below, nearest"
        );
        // Dropped far to the left: it ends left of A, and all moves to 0,0.
        let mut c = two();
        c.logical[1].x = -3000;
        c.logical[1].y = 100;
        snap(&mut c, 1);
        assert_eq!(
            (c.logical[0].x, c.logical[1].x, c.logical[1].y),
            (1280, 0, 100)
        );
        // Rotating B keeps them touching.
        let mut c = two();
        c.set_transform(1, 1);
        assert_eq!(logical_size(&c, &c.logical[1]), (1024, 1280));
        // A larger scale shrinks A; B moves along.
        let mut c = two();
        c.set_scale(0, 2.0);
        assert_eq!(
            (c.logical[1].x, logical_size(&c, &c.logical[0])),
            (960, (960, 540))
        );
        // Join from single.
        let mut c = two();
        c.single("B");
        assert_eq!(c.arrangement(), Arrangement::Single);
        c.join();
        assert_eq!(c.arrangement(), Arrangement::Join);
        assert_eq!(c.logical.len(), 2);
        assert!(c.logical.iter().filter(|l| l.primary).count() == 1);
        // No shared resolution: no mirroring.
        assert!(!two().mirror());
    }

    /// Against a real helix-comp (headless) on a private session bus:
    /// `HELIX_DISPLAYCONFIG_LIVE_TEST=1 … --ignored live_helix_comp`, run by
    /// a script that started that compositor; never on a user's session.
    #[test]
    #[ignore]
    fn live_helix_comp() {
        if std::env::var("HELIX_DISPLAYCONFIG_LIVE_TEST").as_deref() != Ok("1") {
            eprintln!("needs HELIX_DISPLAYCONFIG_LIVE_TEST=1 and a private bus");
            return;
        }
        let conn = Connection::session().unwrap();
        let mut c = read(&conn).unwrap();
        eprintln!("{c:#?}");
        assert!(!c.monitors.is_empty());
        assert!(allowed(&conn), "ApplyMonitorsConfigAllowed");
        apply(&conn, &c, VERIFY).unwrap();
        // A scale the mode offers, applied for now only (no dialog).
        let scale = c
            .scales(0)
            .into_iter()
            .find(|s| *s > 1.0)
            .expect("a larger scale");
        c.set_scale(0, scale);
        apply(&conn, &c, VERIFY).unwrap();
        apply(&conn, &c, 1).unwrap();
        let after = read(&conn).unwrap();
        assert_eq!(after.logical[0].scale, scale);
        assert_ne!(after.serial, c.serial);
        // A mode it does not have is refused.
        c.serial = after.serial;
        c.logical[0].outputs[0].mode = "1x1@1.000".into();
        assert!(apply(&conn, &c, VERIFY).is_err());
    }

    #[test]
    fn labels() {
        assert_eq!(resolution_label(1920, 1080), "1920 × 1080 (16:9)");
        assert_eq!(resolution_label(2560, 1600), "2560 × 1600 (16:10)");
        assert_eq!(resolution_label(1366, 768), "1366 × 768");
        assert_eq!(scale_label(1.25), "125 %");
        let mut m = two().monitors[0].modes[0].clone();
        assert_eq!(rate_label(&m), "60.00 Hz");
        m.variable = true;
        assert_eq!(rate_label(&m), "Variable (up to 60.00 Hz)");
    }
}
