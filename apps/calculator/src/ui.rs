//! GTK user interface.

use adw::prelude::*;
use gtk::{gio, glib};
use helix_app_common::Labelled;
use helix_app_common::Prefs;
use serde::{Deserialize, Serialize};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use crate::engine::{to_signed, AngleUnit, Context, Mode, Value};
use crate::financial;
use crate::format::{self, NumberFormat, Options};
use helix_app_common::i18n::{n_, tr};
use helix_app_common::trf;

pub const APP_ID: &str = "org.helixdesktop.Calculator";
const SCHEMA: &str = "org.gnome.calculator";
const MAX_HISTORY: usize = 100;

/// The part of the keypad shown in a narrow window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    Numbers,
    Functions,
    Bits,
}

impl Pane {
    fn label(self) -> &'static str {
        match self {
            Pane::Numbers => tr("Numbers"),
            Pane::Functions => tr("Functions"),
            Pane::Bits => tr("Bits"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonMode {
    Basic,
    Advanced,
    Financial,
    Programming,
}

impl ButtonMode {
    const ALL: [ButtonMode; 4] = [
        ButtonMode::Basic,
        ButtonMode::Advanced,
        ButtonMode::Financial,
        ButtonMode::Programming,
    ];
    fn nick(self) -> &'static str {
        match self {
            ButtonMode::Basic => "basic",
            ButtonMode::Advanced => "advanced",
            ButtonMode::Financial => "financial",
            ButtonMode::Programming => "programming",
        }
    }
    fn label(self) -> &'static str {
        match self {
            ButtonMode::Basic => tr("Basic"),
            ButtonMode::Advanced => tr("Advanced"),
            ButtonMode::Financial => tr("Financial"),
            ButtonMode::Programming => tr("Programming"),
        }
    }
    fn from_nick(s: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|m| m.nick() == s)
            .unwrap_or(ButtonMode::Basic)
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct HistoryEntry {
    expression: String,
    result: String,
}

struct State {
    ctx: Context,
    prefs: Prefs,
    mode: ButtonMode,
    options: Options,
    history: Vec<HistoryEntry>,
    just_evaluated: bool,
    /// The last successful evaluation, for Ctrl+Z.
    undo: Option<Undo>,
}

/// What an evaluation replaced: the expression and the context (`ans`,
/// variables) before it, and the result it left in the entry.
struct Undo {
    expression: String,
    result: String,
    ctx: Context,
}

impl State {
    fn sync_ctx_mode(&mut self) {
        self.ctx.mode = match self.mode {
            ButtonMode::Programming => Mode::Integer {
                bits: self.options.word_size,
                base: self.options.base,
            },
            _ => Mode::Real,
        };
        if self.mode != ButtonMode::Programming {
            self.options.base = 10;
        } else {
            self.options.base = self.prefs.get::<i32>("base", 10) as u32;
        }
    }

    fn load_options(prefs: &Prefs) -> Options {
        let word = prefs.get::<i32>("word-size", 64) as u32;
        Options {
            format: NumberFormat::from_nick(&prefs.get_enum_nick("number-format", "automatic")),
            accuracy: prefs.get::<i32>("accuracy", 9).clamp(0, 15) as usize,
            thousands: prefs.get("show-thousands", false),
            show_zeroes: prefs.get("show-zeroes", false),
            base: 10,
            word_size: if [8, 16, 32, 64].contains(&word) {
                word
            } else {
                64
            },
            sep: crate::locale::Separators::current(),
        }
    }
}

fn history_path() -> std::path::PathBuf {
    helix_app_common::data_dir("helix-calculator").join("history.json")
}

fn load_history() -> Vec<HistoryEntry> {
    std::fs::read(history_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn save_history(h: &[HistoryEntry]) {
    if let Ok(json) = serde_json::to_vec_pretty(h) {
        let _ = std::fs::write(history_path(), json);
    }
}

/// What a button does.
#[derive(Clone)]
enum Act {
    Insert(&'static str),
    Eval,
    Clear,
    Backspace,
    Financial(usize),
}

struct Key {
    label: &'static str,
    act: Act,
    tooltip: Option<&'static str>,
    class: Option<&'static str>,
}

/// The accessible name of a keypad key: its tooltip without the bracketed
/// shortcut hint ("Square root [Ctrl+R]" → "Square root"), a spelled-out
/// name for bare symbols, or `None` when the visible label (a digit or
/// letter) already is the name.
fn key_name(label: &str, tooltip: Option<&str>) -> Option<String> {
    if let Some(t) = tooltip {
        let name = match t.rfind(" [") {
            Some(i) if t.ends_with(']') => &t[..i],
            _ => t,
        };
        return Some(name.trim().to_string());
    }
    let spelled = match label {
        "(" => tr("Open parenthesis"),
        ")" => tr("Close parenthesis"),
        _ => return None,
    };
    Some(spelled.to_string())
}

fn k(label: &'static str, insert: &'static str) -> Key {
    Key {
        label,
        act: Act::Insert(insert),
        tooltip: None,
        class: None,
    }
}

fn kt(label: &'static str, insert: &'static str, tooltip: &'static str) -> Key {
    Key {
        label,
        act: Act::Insert(insert),
        tooltip: Some(tooltip),
        class: None,
    }
}

/// The locale's decimal separator, as the decimal key shows and types it.
fn decimal_str() -> &'static str {
    static DECIMAL: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    DECIMAL.get_or_init(|| {
        Box::leak(
            crate::locale::Separators::current()
                .decimal
                .to_string()
                .into_boxed_str(),
        )
    })
}

fn basic_keys(programming: bool) -> Vec<Vec<Key>> {
    let op = |label, ins, tip| Key {
        label,
        act: Act::Insert(ins),
        tooltip: Some(tip),
        class: Some("calc-op"),
    };
    vec![
        vec![
            k("7", "7"),
            k("8", "8"),
            k("9", "9"),
            op("÷", "÷", tr("Divide [/]")),
            Key {
                label: "⌫",
                act: Act::Backspace,
                tooltip: Some(tr("Backspace")),
                class: None,
            },
            Key {
                label: "C",
                act: Act::Clear,
                tooltip: Some(tr("Clear [Escape]")),
                class: Some("destructive-action"),
            },
        ],
        vec![
            k("4", "4"),
            k("5", "5"),
            k("6", "6"),
            op("×", "×", tr("Multiply [*]")),
            k("(", "("),
            k(")", ")"),
        ],
        vec![
            k("1", "1"),
            k("2", "2"),
            k("3", "3"),
            op("−", "−", tr("Subtract [-]")),
            kt("x²", "²", tr("Square [Ctrl+2]")),
            kt("√", "√", tr("Square root [Ctrl+R]")),
        ],
        vec![
            k("0", "0"),
            if programming {
                kt("mod", " mod ", tr("Modulus divide"))
            } else {
                kt(decimal_str(), decimal_str(), tr("Decimal point"))
            },
            if programming {
                kt("x!", "!", tr("Factorial"))
            } else {
                kt("%", "%", tr("Percentage [%]"))
            },
            op("+", "+", tr("Add [+]")),
            Key {
                label: "=",
                act: Act::Eval,
                tooltip: Some(tr("Calculate result [Enter]")),
                class: Some("suggested-action"),
            },
        ],
    ]
}

fn advanced_keys() -> Vec<Vec<Key>> {
    vec![
        vec![
            kt("sin", "sin ", tr("Sine")),
            kt("cos", "cos ", tr("Cosine")),
            kt("tan", "tan ", tr("Tangent")),
            kt("xʸ", "^", tr("Exponent [^]")),
        ],
        vec![
            kt("sin⁻¹", "sin⁻¹ ", tr("Inverse sine")),
            kt("cos⁻¹", "cos⁻¹ ", tr("Inverse cosine")),
            kt("tan⁻¹", "tan⁻¹ ", tr("Inverse tangent")),
            kt("x⁻¹", "⁻¹", tr("Inverse")),
        ],
        vec![
            kt("ln", "ln ", tr("Natural logarithm")),
            kt("log", "log ", tr("Logarithm")),
            kt("eˣ", "e^", tr("Exponential")),
            kt("x!", "!", tr("Factorial [!]")),
        ],
        vec![
            kt("π", "π", tr("Pi [Ctrl+P]")),
            kt("e", "e", tr("Euler's number")),
            kt("|x|", "|", tr("Absolute value [|]")),
            kt("mod", " mod ", tr("Modulus divide")),
        ],
        vec![
            kt("ans", "ans", tr("Last answer")),
            kt("×10ʸ", "×10^", tr("Scientific exponent")),
            kt("∛", "∛", tr("Cube root")),
            kt("rand", "rand", tr("Random number")),
        ],
    ]
}

fn programming_keys() -> Vec<Vec<Key>> {
    vec![
        vec![
            k("A", "A"),
            k("B", "B"),
            kt("and", " and ", tr("Boolean AND")),
            kt("<<", "<<", tr("Shift left")),
        ],
        vec![
            k("C", "C"),
            k("D", "D"),
            kt("or", " or ", tr("Boolean OR")),
            kt(">>", ">>", tr("Shift right")),
        ],
        vec![
            k("E", "E"),
            k("F", "F"),
            kt("xor", " xor ", tr("Boolean exclusive OR")),
            kt("not", "not ", tr("Boolean NOT")),
        ],
        vec![
            kt("ones", "ones ", tr("One's complement")),
            kt("twos", "twos ", tr("Two's complement")),
            kt("xʸ", "^", tr("Exponent")),
            kt("ans", "ans", tr("Last answer")),
        ],
    ]
}

fn financial_keys() -> Vec<Vec<Key>> {
    let f = |i: usize| Key {
        label: financial::FUNCTIONS[i].name,
        act: Act::Financial(i),
        tooltip: Some(tr(financial::FUNCTIONS[i].title)),
        class: None,
    };
    vec![
        vec![f(0), f(1), f(2)],
        vec![f(3), f(4), f(5)],
        vec![f(6), f(7), f(8)],
        vec![
            f(9),
            kt("%", "%", tr("Percentage")),
            kt("xʸ", "^", tr("Exponent")),
        ],
    ]
}

/// How a keyboard shortcut is delivered.
#[derive(Clone, Copy, Debug, PartialEq)]
enum How {
    /// A window accelerator for the action (runs before the focused widget).
    Accel(&'static str),
    /// A window shortcut for the action that runs only if the focused
    /// widget does not use the key (Escape still closes popovers).
    Bubble(&'static str),
    /// Typed into, or handled by, the expression entry; no action.
    Entry(&'static str),
}

impl How {
    fn trigger(self) -> &'static str {
        match self {
            How::Accel(t) | How::Bubble(t) | How::Entry(t) => t,
        }
    }
}

/// A keyboard shortcut: the single table that registers the
/// accelerators, builds the Keyboard Shortcuts dialog and backs the
/// hints in the keypad tooltips.
struct Shortcut {
    section: &'static str,
    title: &'static str,
    /// Detailed action name; empty for [`How::Entry`].
    action: &'static str,
    how: How,
}

const fn sc(
    section: &'static str,
    title: &'static str,
    action: &'static str,
    how: How,
) -> Shortcut {
    Shortcut {
        section,
        title,
        action,
        how,
    }
}

const SHORTCUTS: &[Shortcut] = &[
    sc(
        n_("Calculation"),
        n_("Calculate result"),
        "",
        How::Entry("Return"),
    ),
    sc(
        n_("Calculation"),
        n_("Clear"),
        "win.clear",
        How::Bubble("Escape"),
    ),
    sc(
        n_("Calculation"),
        n_("Undo last calculation"),
        "",
        How::Entry("<Control>z"),
    ),
    sc(n_("Calculation"), n_("Add"), "", How::Entry("plus")),
    sc(n_("Calculation"), n_("Subtract"), "", How::Entry("minus")),
    sc(
        n_("Calculation"),
        n_("Multiply"),
        "",
        How::Entry("asterisk"),
    ),
    sc(n_("Calculation"), n_("Divide"), "", How::Entry("slash")),
    sc(
        n_("Calculation"),
        n_("Exponent"),
        "",
        How::Entry("asciicircum"),
    ),
    sc(
        n_("Calculation"),
        n_("Percentage"),
        "",
        How::Entry("percent"),
    ),
    sc(n_("Calculation"), n_("Factorial"), "", How::Entry("exclam")),
    sc(
        n_("Calculation"),
        n_("Absolute value"),
        "",
        How::Entry("bar"),
    ),
    sc(
        n_("Calculation"),
        n_("Square"),
        "win.insert::²",
        How::Accel("<Control>2"),
    ),
    sc(
        n_("Calculation"),
        n_("Square root"),
        "win.insert::√",
        How::Accel("<Control>r"),
    ),
    sc(
        n_("Calculation"),
        n_("Pi"),
        "win.insert::π",
        How::Accel("<Control>p"),
    ),
    sc(
        n_("Modes"),
        n_("Basic mode"),
        "win.mode::basic",
        How::Accel("<Alt>1"),
    ),
    sc(
        n_("Modes"),
        n_("Advanced mode"),
        "win.mode::advanced",
        How::Accel("<Alt>2"),
    ),
    sc(
        n_("Modes"),
        n_("Financial mode"),
        "win.mode::financial",
        How::Accel("<Alt>3"),
    ),
    sc(
        n_("Modes"),
        n_("Programming mode"),
        "win.mode::programming",
        How::Accel("<Alt>4"),
    ),
    sc(
        n_("General"),
        n_("Preferences"),
        "win.preferences",
        How::Accel("<Control>comma"),
    ),
    sc(
        n_("General"),
        n_("Keyboard shortcuts"),
        "win.shortcuts",
        How::Accel("<Control>question"),
    ),
    sc(
        n_("General"),
        n_("Close window"),
        "window.close",
        How::Accel("<Control>w"),
    ),
    sc(
        n_("General"),
        n_("Quit"),
        "app.quit",
        How::Accel("<Control>q"),
    ),
];

/// How a GTK trigger is written for people: "<Control>comma" → "Ctrl+,".
#[cfg(test)]
fn shortcut_label(trigger: &str) -> String {
    let mut rest = trigger;
    let mut out = String::new();
    while let Some(after) = rest.strip_prefix('<') {
        let Some(end) = after.find('>') else { break };
        out.push_str(match &after[..end] {
            "Control" | "Primary" => "Ctrl+",
            "Alt" => "Alt+",
            "Shift" => "Shift+",
            "Super" => "Super+",
            other => other,
        });
        rest = &after[end + 1..];
    }
    let key = match rest {
        "Return" => "Enter".to_string(),
        "comma" => ",".into(),
        "question" => "?".into(),
        "plus" => "+".into(),
        "minus" => "-".into(),
        "asterisk" => "*".into(),
        "slash" => "/".into(),
        "asciicircum" => "^".into(),
        "percent" => "%".into(),
        "exclam" => "!".into(),
        "bar" => "|".into(),
        k if k.chars().count() == 1 => k.to_uppercase(),
        k => k.to_string(),
    };
    out + &key
}

/// A keypad's function grid (if the mode has one) and number grid.
type KeypadParts = (Option<gtk::Grid>, gtk::Grid);

#[derive(Clone)]
struct Ui {
    window: adw::ApplicationWindow,
    entry: gtk::Entry,
    error: gtk::Label,
    history: gtk::ListBox,
    history_scroll: gtk::ScrolledWindow,
    keypad: gtk::Stack,
    prog_bar: gtk::Box,
    bits: gtk::Grid,
    /// The bit panel's toggles with the bit each one stands for.
    bit_buttons: Rc<RefCell<Vec<(gtk::ToggleButton, u32)>>>,
    /// Bits per row of the bit panel (16, or 8 when narrow).
    bits_per_row: Rc<Cell<u32>>,
    /// Each keypad's function grid (if any) and number grid.
    keypad_parts: Rc<RefCell<Vec<KeypadParts>>>,
    /// Narrow layout (below 500sp): one keypad part at a time.
    narrow: Rc<Cell<bool>>,
    /// The part shown in the narrow layout.
    pane: Rc<Cell<Pane>>,
    pane_switcher: gtk::Box,
    pane_buttons: [gtk::ToggleButton; 3],
    digit_buttons: Rc<RefCell<Vec<(gtk::Button, u32)>>>,
    state: Rc<RefCell<State>>,
}

pub fn run(initial_expr: Option<String>) -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    helix_app_common::install_standard_actions(
        &app,
        tr("Calculator"),
        tr("Perform arithmetic, scientific, financial and programming calculations"),
    );
    let initial = RefCell::new(initial_expr);
    app.connect_startup(|_| load_css());
    app.connect_activate(move |app| {
        let ui = build(app);
        if let Some(e) = initial.borrow_mut().take() {
            ui.entry.set_text(&e);
            ui.entry.set_position(-1);
        }
        ui.window.present();
    });
    app.run_with_args(&[std::env::args().next().unwrap_or_default()])
}

/// Where bit `bit` of a `bits`-wide word goes in the bit panel with
/// `per_row` bits per row, most significant first: (row, column).
fn bit_cell(bit: u32, bits: u32, per_row: u32) -> (u32, u32) {
    ((bits - 1 - bit) / per_row, per_row - 1 - (bit % per_row))
}

fn load_css() {
    let css = gtk::CssProvider::new();
    css.load_from_string(
        // The display and the keypad are display text, in em so they follow
        // the text size.
        ".calc-entry { font-size: 2em; min-height: 56px; }
         .calc-keypad button { min-width: 52px; min-height: 40px; font-size: 1.2em; }
         .calc-narrow .calc-keypad button { min-width: 32px; padding-left: 2px; padding-right: 2px; }
         .calc-short .calc-keypad button { min-height: 28px; padding-top: 0; padding-bottom: 0; }
         .calc-short .calc-entry { min-height: 40px; }
         .calc-op { font-weight: bold; }
         .calc-history-result { font-weight: bold; }
         .calc-bit { min-width: 22px; min-height: 28px; padding: 0; font-family: monospace; }",
    );
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
}

fn build(app: &adw::Application) -> Ui {
    let prefs = Prefs::new(SCHEMA);
    let options = State::load_options(&prefs);
    let mode = ButtonMode::from_nick(&prefs.get_enum_nick("button-mode", "basic"));
    let ctx = Context {
        angle: AngleUnit::from_nick(&prefs.get_enum_nick("angle-units", "degrees")),
        sep: crate::locale::Separators::current(),
        ..Context::default()
    };
    let mut state = State {
        ctx,
        prefs: prefs.clone(),
        mode,
        options,
        history: load_history(),
        just_evaluated: false,
        undo: None,
    };
    state.sync_ctx_mode();
    let state = Rc::new(RefCell::new(state));

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(tr("Calculator"))
        .default_width(420)
        .default_height(560)
        .width_request(360)
        .height_request(294)
        .build();
    {
        use helix_app_common::window_state::{persist, SizeKeys, Store};
        persist(
            &window,
            Store::settings(
                &prefs,
                SizeKeys::Pair("window-size"),
                "window-maximized",
                "helix-calculator",
            ),
        );
    }

    // Header bar.
    let header = adw::HeaderBar::new();
    let modes = gtk::DropDown::from_strings(&ButtonMode::ALL.map(|m| m.label()));
    modes.set_selected(ButtonMode::ALL.iter().position(|m| *m == mode).unwrap_or(0) as u32);
    modes.set_tooltip_text(Some(tr("Calculator mode")));
    helix_app_common::set_accessible_label(&modes, tr("Calculator mode"));
    header.set_title_widget(Some(&modes));
    let menu = gio::Menu::new();
    let section = gio::Menu::new();
    section.append(Some(tr("_Clear History")), Some("win.clear-history"));
    menu.append_section(None, &section);
    let section = gio::Menu::new();
    section.append(Some(tr("_Preferences")), Some("win.preferences"));
    section.append(
        Some(helix_app_common::i18n::tr(
            helix_app_common::shortcuts::MENU_LABEL,
        )),
        Some("win.shortcuts"),
    );
    section.append(Some(tr("_About Calculator")), Some("app.about"));
    menu.append_section(None, &section);
    let menu_button = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .primary(true)
        .tooltip_text(tr("Main Menu"))
        .build()
        .labelled(tr("Main Menu"));
    header.pack_end(&menu_button);

    // History.
    let history = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .css_classes(["navigation-sidebar"])
        .build();
    let history_scroll = gtk::ScrolledWindow::builder()
        .child(&history)
        .vexpand(true)
        .hscrollbar_policy(gtk::PolicyType::Never)
        .min_content_height(80)
        .build();

    // Entry.
    let entry = gtk::Entry::builder()
        .css_classes(["calc-entry"])
        .xalign(1.0)
        .build();
    entry.set_input_hints(gtk::InputHints::NO_EMOJI);
    helix_app_common::set_accessible_label(&entry, tr("Expression"));
    let error = gtk::Label::builder()
        .xalign(1.0)
        .css_classes(["error", "caption"])
        .wrap(true)
        // Breaks inside a long token too, so it never widens the window.
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .visible(false)
        .build();

    // Programming bar: base + word size + bit panel.
    let prog_bar = gtk::Box::new(gtk::Orientation::Vertical, 6);
    let prog_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    let base_dd =
        gtk::DropDown::from_strings(&[tr("Binary"), tr("Octal"), tr("Decimal"), tr("Hexadecimal")]);
    let bases = [2u32, 8, 10, 16];
    base_dd.set_selected(
        bases
            .iter()
            .position(|b| *b == state.borrow().prefs.get::<i32>("base", 10) as u32)
            .unwrap_or(2) as u32,
    );
    base_dd.set_tooltip_text(Some(tr("Number base")));
    helix_app_common::set_accessible_label(&base_dd, tr("Number base"));
    let word_dd = gtk::DropDown::from_strings(&["8-bit", "16-bit", "32-bit", "64-bit"]);
    let words = [8u32, 16, 32, 64];
    word_dd.set_selected(
        words
            .iter()
            .position(|b| *b == options.word_size)
            .unwrap_or(3) as u32,
    );
    word_dd.set_tooltip_text(Some(tr("Word size")));
    helix_app_common::set_accessible_label(&word_dd, tr("Word size"));
    prog_row.append(&base_dd);
    prog_row.append(&word_dd);
    let bits = gtk::Grid::builder()
        .column_spacing(0)
        .row_spacing(0)
        .halign(gtk::Align::Center)
        .build();
    prog_bar.append(&prog_row);
    // Scrolls when a 64-bit word at 8 bits per row is taller than the
    // window (narrow layout).
    let bits_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .child(&bits)
        .build();
    prog_bar.append(&bits_scroll);

    let keypad = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .build();

    // Narrow windows show one part of the keypad at a time, chosen here.
    let pane_switcher = gtk::Box::builder()
        .css_classes(["linked"])
        .halign(gtk::Align::Center)
        .visible(false)
        .build();
    let pane_buttons = [Pane::Numbers, Pane::Functions, Pane::Bits].map(|p| {
        gtk::ToggleButton::builder()
            .label(p.label())
            .active(p == Pane::Numbers)
            .build()
    });
    for b in &pane_buttons[1..] {
        b.set_group(Some(&pane_buttons[0]));
    }
    for b in &pane_buttons {
        pane_switcher.append(b);
    }

    let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
    content.set_margin_start(12);
    content.set_margin_end(12);
    content.set_margin_bottom(12);
    content.append(&history_scroll);
    content.append(&entry);
    content.append(&error);
    content.append(&pane_switcher);
    content.append(&prog_bar);
    content.append(&keypad);

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    // Scrolls only where even the short layout does not fit (Programming
    // mode's base and word size row at 360×294).
    toolbar.set_content(Some(
        &gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&content)
            .build(),
    ));
    window.set_content(Some(&toolbar));

    let ui = Ui {
        window: window.clone(),
        entry: entry.clone(),
        error,
        history,
        history_scroll,
        keypad: keypad.clone(),
        prog_bar,
        bits,
        bit_buttons: Rc::default(),
        bits_per_row: Rc::new(Cell::new(16)),
        keypad_parts: Rc::default(),
        narrow: Rc::default(),
        pane: Rc::new(Cell::new(Pane::Numbers)),
        pane_switcher: pane_switcher.clone(),
        pane_buttons: pane_buttons.clone(),
        digit_buttons: Rc::default(),
        state: state.clone(),
    };

    for m in ButtonMode::ALL {
        keypad.add_named(&ui.build_keypad(m), Some(m.nick()));
    }
    ui.apply_mode();
    ui.rebuild_history();
    for (b, pane) in ui
        .pane_buttons
        .iter()
        .zip([Pane::Numbers, Pane::Functions, Pane::Bits])
    {
        let ui = ui.clone();
        b.connect_toggled(move |b| {
            if b.is_active() && ui.pane.replace(pane) != pane {
                ui.update_layout();
            }
        });
    }
    // Narrow windows show one keypad part at a time; short ones (down to
    // the suite's 294 px) drop the history and use lower keys. Only one
    // breakpoint applies at a time (the last matching one), so narrow and
    // short together have their own.
    let narrow = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 500sp").expect("valid condition"),
    );
    let short = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-height: 520sp").expect("valid condition"),
    );
    let narrow_short = adw::Breakpoint::new(
        adw::BreakpointCondition::parse("max-width: 500sp and max-height: 520sp")
            .expect("valid condition"),
    );
    for bp in [&narrow, &narrow_short] {
        for on in [true, false] {
            let ui = ui.clone();
            let handler = move |_: &adw::Breakpoint| {
                if ui.narrow.replace(on) != on {
                    ui.update_layout();
                }
            };
            if on {
                bp.connect_apply(handler);
            } else {
                bp.connect_unapply(handler);
            }
        }
    }
    for bp in [&short, &narrow_short] {
        bp.add_setter(&ui.history_scroll, "visible", Some(&false.to_value()));
        bp.add_setter(
            &content,
            "css-classes",
            Some(&glib::StrV::from(vec!["calc-short"]).to_value()),
        );
    }
    window.add_breakpoint(narrow);
    window.add_breakpoint(short);
    window.add_breakpoint(narrow_short);

    // Signals.
    {
        let ui = ui.clone();
        modes.connect_selected_notify(move |dd| {
            ui.set_mode(ButtonMode::ALL[dd.selected() as usize])
        });
    }
    {
        let ui = ui.clone();
        base_dd.connect_selected_notify(move |dd| {
            let base = bases[dd.selected() as usize];
            ui.state.borrow().prefs.set("base", base as i32);
            ui.convert_entry_base(base);
        });
    }
    {
        let ui = ui.clone();
        word_dd.connect_selected_notify(move |dd| {
            let bits = words[dd.selected() as usize];
            {
                let mut s = ui.state.borrow_mut();
                s.prefs.set("word-size", bits as i32);
                s.options.word_size = bits;
                s.sync_ctx_mode();
            }
            ui.rebuild_bits();
        });
    }
    {
        let ui = ui.clone();
        entry.connect_activate(move |_| ui.evaluate());
    }
    {
        let ui = ui.clone();
        entry.connect_changed(move |_| {
            ui.clear_error();
            ui.refresh_bits();
        });
    }
    {
        // After a result, typing an operator continues from it instead of
        // replacing the selected result.
        let ui = ui.clone();
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(move |_, key, _, mods| {
            let ctrl_only = mods
                & (gtk::gdk::ModifierType::CONTROL_MASK
                    | gtk::gdk::ModifierType::ALT_MASK
                    | gtk::gdk::ModifierType::SHIFT_MASK)
                == gtk::gdk::ModifierType::CONTROL_MASK;
            if ctrl_only && matches!(key, gtk::gdk::Key::z | gtk::gdk::Key::Z) && ui.undo() {
                return glib::Propagation::Stop;
            }
            if mods
                .intersects(gtk::gdk::ModifierType::CONTROL_MASK | gtk::gdk::ModifierType::ALT_MASK)
                || helix_app_common::ime::is_composing(&ui.entry)
            {
                return glib::Propagation::Proceed;
            }
            // In a decimal-comma locale, both `.` and `,` type the
            // locale's decimal separator.
            let decimal_key = matches!(
                key,
                gtk::gdk::Key::period
                    | gtk::gdk::Key::comma
                    | gtk::gdk::Key::KP_Decimal
                    | gtk::gdk::Key::KP_Separator
            );
            if decimal_key && ui.state.borrow().options.sep.comma() {
                let programming = ui.state.borrow().mode == ButtonMode::Programming;
                if !programming {
                    ui.insert(decimal_str());
                    return glib::Propagation::Stop;
                }
            }
            if let Some(c) = key.to_unicode() {
                if ui.state.borrow().just_evaluated && "+-*/^×÷−!%²³".contains(c) {
                    ui.entry.set_position(-1);
                }
                ui.state.borrow_mut().just_evaluated = false;
            }
            glib::Propagation::Proceed
        });
        entry.add_controller(keys);
        helix_app_common::ime::track_composition(&entry);
    }

    // Window actions.
    let mode_action = gio::SimpleAction::new("mode", Some(glib::VariantTy::STRING));
    {
        let modes = modes.clone();
        mode_action.connect_activate(move |_, p| {
            if let Some(nick) = p.and_then(|p| p.get::<String>()) {
                let m = ButtonMode::from_nick(&nick);
                modes
                    .set_selected(ButtonMode::ALL.iter().position(|x| *x == m).unwrap_or(0) as u32);
            }
        });
    }
    window.add_action(&mode_action);
    let clear = gio::SimpleAction::new("clear-history", None);
    {
        let ui = ui.clone();
        clear.connect_activate(move |_, _| {
            ui.state.borrow_mut().history.clear();
            save_history(&[]);
            ui.rebuild_history();
        });
    }
    window.add_action(&clear);
    let prefs_action = gio::SimpleAction::new("preferences", None);
    {
        let ui = ui.clone();
        prefs_action.connect_activate(move |_, _| ui.show_preferences());
    }
    window.add_action(&prefs_action);
    let shortcuts = gio::SimpleAction::new("shortcuts", None);
    {
        let win = window.clone();
        shortcuts.connect_activate(move |_, _| show_shortcuts(&win));
    }
    window.add_action(&shortcuts);
    let insert = gio::SimpleAction::new("insert", Some(glib::VariantTy::STRING));
    {
        let ui = ui.clone();
        insert.connect_activate(move |_, p| {
            if ui.window.visible_dialog().is_some() {
                return;
            }
            if let Some(text) = p.and_then(|p| p.get::<String>()) {
                ui.insert(&text);
                ui.entry.grab_focus_without_selecting();
            }
        });
    }
    window.add_action(&insert);
    let clear_entry = gio::SimpleAction::new("clear", None);
    {
        let ui = ui.clone();
        clear_entry.connect_activate(move |_, _| {
            if ui.window.visible_dialog().is_none() {
                ui.activate(&Act::Clear);
            }
        });
    }
    window.add_action(&clear_entry);
    let mut bubble = Vec::new();
    for s in SHORTCUTS {
        match s.how {
            How::Accel(t) => app.set_accels_for_action(s.action, &[t]),
            How::Bubble(t) => bubble.push((t, s.action)),
            How::Entry(_) => {}
        }
    }
    helix_app_common::add_bubble_shortcuts(&window, &bubble);

    entry.grab_focus();
    ui
}

/// The Keyboard Shortcuts window, built from [`SHORTCUTS`] by
/// `helix_app_common::shortcuts`.
fn show_shortcuts(win: &adw::ApplicationWindow) {
    use helix_app_common::shortcuts::Item;
    let mut groups: Vec<(&str, Vec<Item>)> = Vec::new();
    for s in SHORTCUTS {
        let item = Item::Keys(s.title, s.how.trigger());
        match groups.iter_mut().find(|(title, _)| *title == s.section) {
            Some((_, items)) => items.push(item),
            None => groups.push((s.section, vec![item])),
        }
    }
    let shortcuts = helix_app_common::shortcuts::window_from(
        groups
            .iter()
            .map(|(title, items)| (*title, items.as_slice())),
    );
    shortcuts.set_transient_for(Some(win));
    shortcuts.present();
}

impl Ui {
    fn build_keypad(&self, mode: ButtonMode) -> gtk::Widget {
        let outer = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        outer.add_css_class("calc-keypad");
        outer.set_homogeneous(false);
        let extra = match mode {
            ButtonMode::Basic => None,
            ButtonMode::Advanced => Some(advanced_keys()),
            ButtonMode::Financial => Some(financial_keys()),
            ButtonMode::Programming => Some(programming_keys()),
        };
        let extra = extra.map(|rows| self.grid(rows, mode));
        if let Some(g) = &extra {
            outer.append(g);
        }
        let basic = self.grid(basic_keys(mode == ButtonMode::Programming), mode);
        outer.append(&basic);
        self.keypad_parts.borrow_mut().push((extra, basic));
        outer.into()
    }

    fn grid(&self, rows: Vec<Vec<Key>>, mode: ButtonMode) -> gtk::Grid {
        let grid = gtk::Grid::builder()
            .row_spacing(6)
            .column_spacing(6)
            .row_homogeneous(true)
            .column_homogeneous(true)
            .hexpand(true)
            .vexpand(true)
            .build();
        for (r, row) in rows.into_iter().enumerate() {
            for (c, key) in row.into_iter().enumerate() {
                let b = gtk::Button::with_label(key.label);
                b.set_tooltip_text(key.tooltip);
                // The label is often a symbol ("÷", "x²", "⌫"); name the
                // operation instead.
                if let Some(name) = key_name(key.label, key.tooltip) {
                    helix_app_common::set_accessible_label(&b, &name);
                }
                if let Some(class) = key.class {
                    b.add_css_class(class);
                }
                b.set_focus_on_click(false);
                if mode == ButtonMode::Programming {
                    if let Act::Insert(s) = key.act {
                        if s.len() == 1 && s.chars().all(|c| c.is_ascii_hexdigit()) {
                            let d = s.chars().next().unwrap().to_digit(16).unwrap();
                            self.digit_buttons.borrow_mut().push((b.clone(), d));
                        }
                    }
                }
                let (cols, rows_span) = if matches!(key.act, Act::Eval) {
                    (2, 1)
                } else {
                    (1, 1)
                };
                let ui = self.clone();
                let act = key.act.clone();
                b.connect_clicked(move |_| ui.activate(&act));
                grid.attach(&b, c as i32, r as i32, cols, rows_span);
            }
        }
        grid
    }

    fn activate(&self, act: &Act) {
        match act {
            Act::Insert(text) => self.insert(text),
            Act::Eval => self.evaluate(),
            Act::Clear => {
                self.entry.set_text("");
                self.state.borrow_mut().just_evaluated = false;
            }
            Act::Backspace => {
                self.state.borrow_mut().just_evaluated = false;
                if let Some((a, b)) = self.entry.selection_bounds() {
                    self.entry.delete_text(a, b);
                } else {
                    let p = self.entry.position();
                    if p > 0 {
                        self.entry.delete_text(p - 1, p);
                    }
                }
            }
            Act::Financial(i) => self.financial_dialog(*i),
        }
        self.entry.grab_focus_without_selecting();
    }

    fn insert(&self, text: &str) {
        let continues =
            text.starts_with([' ', '+', '−', '×', '÷', '^', '!', '%', '²', '⁻', '<', '>']);
        let just = std::mem::take(&mut self.state.borrow_mut().just_evaluated);
        if just && !continues {
            self.entry.set_text("");
        }
        if let Some((a, b)) = self.entry.selection_bounds() {
            if !(just && continues) {
                self.entry.delete_text(a, b);
                self.entry.set_position(a);
            }
        }
        let mut pos = if just && continues {
            self.entry.text().chars().count() as i32
        } else {
            self.entry.position()
        };
        self.entry.insert_text(text, &mut pos);
        self.entry.set_position(pos);
    }

    fn evaluate(&self) {
        let text = self.entry.text().to_string();
        if text.trim().is_empty() {
            return;
        }
        let (result, before) = {
            let mut s = self.state.borrow_mut();
            let before = s.ctx.clone();
            let r = s.ctx.evaluate(&text).map(|v| format::format(v, &s.options));
            (r, before)
        };
        match result {
            Ok(formatted) => {
                {
                    let mut s = self.state.borrow_mut();
                    s.history.push(HistoryEntry {
                        expression: text.clone(),
                        result: formatted.clone(),
                    });
                    let excess = s.history.len().saturating_sub(MAX_HISTORY);
                    s.history.drain(..excess);
                    save_history(&s.history);
                    s.undo = Some(Undo {
                        expression: text.clone(),
                        result: formatted.clone(),
                        ctx: before,
                    });
                }
                self.append_history_row(self.state.borrow().history.last().unwrap());
                self.entry.set_text(&formatted);
                self.entry.select_region(0, -1);
                self.state.borrow_mut().just_evaluated = true;
                helix_app_common::announce(
                    &self.entry,
                    &spoken_result(&text, &formatted),
                    helix_app_common::Urgency::Polite,
                );
            }
            Err(e) => {
                let message = e.to_string();
                self.error.set_label(&message);
                self.error.set_visible(true);
                helix_app_common::set_input_error(&self.entry, Some(&self.error), Some(&message));
                if let Some(p) = e.position() {
                    self.entry.set_position(p as i32);
                }
            }
        }
    }

    /// Undoes the last evaluation while its result is still in the entry:
    /// restores the expression and the previous `ans` and variables.
    /// Returns false when there is nothing to undo (the entry's own text
    /// undo then applies).
    fn undo(&self) -> bool {
        let undo = {
            let mut s = self.state.borrow_mut();
            let current = self.entry.text();
            match s.undo.take() {
                Some(u) if u.result == current.as_str() => u,
                other => {
                    s.undo = other;
                    return false;
                }
            }
        };
        {
            let mut s = self.state.borrow_mut();
            s.ctx = undo.ctx;
            s.just_evaluated = false;
        }
        self.entry.set_text(&undo.expression);
        self.entry.set_position(-1);
        true
    }

    /// Clears an error shown by [`Ui::evaluate`] (the entry was edited).
    fn clear_error(&self) {
        if !self.error.is_visible() {
            return;
        }
        self.error.set_visible(false);
        helix_app_common::set_input_error(&self.entry, None, None);
    }

    fn rebuild_history(&self) {
        self.history.remove_all();
        let entries = self.state.borrow().history.clone();
        for e in &entries {
            self.append_history_row(e);
        }
    }

    fn append_history_row(&self, e: &HistoryEntry) {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        // Read as one unit ("12×3+4 equals 40") when the row is focused;
        // its buttons keep their own names.
        let list_row = gtk::ListBoxRow::builder()
            .child(&row)
            .activatable(false)
            .build();
        list_row.update_property(&[gtk::accessible::Property::Label(&spoken_result(
            &e.expression,
            &e.result,
        ))]);
        // A long expression or result is cut at its start, so it never
        // widens the window (#198) and its end stays in view; the row's
        // accessible name keeps the whole text.
        let ellipsized = |text: &str| {
            gtk::Label::builder()
                .label(text)
                .ellipsize(gtk::pango::EllipsizeMode::Start)
                .xalign(1.0)
                .build()
        };
        let expr = gtk::Button::builder()
            .child(&ellipsized(&e.expression))
            .css_classes(["flat"])
            .hexpand(true)
            .halign(gtk::Align::End)
            .tooltip_text(tr("Insert expression"))
            .build();
        let eq = gtk::Label::new(Some("="));
        let res = gtk::Button::builder()
            .child(&ellipsized(&e.result))
            .css_classes(["flat", "calc-history-result"])
            .tooltip_text(tr("Insert result"))
            .build();
        for (b, text) in [(&expr, e.expression.clone()), (&res, e.result.clone())] {
            let ui = self.clone();
            b.connect_clicked(move |_| {
                ui.state.borrow_mut().just_evaluated = false;
                let mut pos = ui.entry.position();
                ui.entry.insert_text(&text, &mut pos);
                ui.entry.set_position(pos);
                ui.entry.grab_focus_without_selecting();
            });
        }
        row.append(&expr);
        row.append(&eq);
        row.append(&res);
        self.history.append(&list_row);
        // Scroll to the newest entry once laid out.
        let adj = self.history_scroll.vadjustment();
        glib::idle_add_local_once(move || adj.set_value(adj.upper()));
    }

    fn set_mode(&self, mode: ButtonMode) {
        {
            let mut s = self.state.borrow_mut();
            if s.mode == mode {
                return;
            }
            s.mode = mode;
            s.prefs.set_enum_nick("button-mode", mode.nick());
            s.sync_ctx_mode();
            // Results carried across modes keep their numeric value.
            let bits = s.options.word_size;
            s.ctx.ans = match (s.ctx.ans, mode) {
                (Value::Real(x), ButtonMode::Programming) => {
                    Value::Int(x.trunc() as i64 as u64 & crate::engine::mask(bits))
                }
                (Value::Int(v), m) if m != ButtonMode::Programming => {
                    Value::Real(to_signed(v, bits) as f64)
                }
                (v, _) => v,
            };
        }
        self.entry.set_text("");
        self.apply_mode();
    }

    fn apply_mode(&self) {
        let mode = self.state.borrow().mode;
        self.keypad.set_visible_child_name(mode.nick());
        self.prog_bar.set_visible(mode == ButtonMode::Programming);
        self.update_digit_sensitivity();
        self.update_layout();
        self.rebuild_bits();
    }

    /// Wide: each mode's function and number grids side by side, the bit
    /// panel above. Narrow: a Numbers / Functions (/ Bits) switcher and
    /// only that part, with smaller buttons and 8 bits per row.
    fn update_layout(&self) {
        let mode = self.state.borrow().mode;
        let narrow = self.narrow.get();
        let mut pane = self.pane.get();
        if pane == Pane::Bits && mode != ButtonMode::Programming {
            pane = Pane::Numbers;
        }
        if mode == ButtonMode::Basic {
            pane = Pane::Numbers;
        }
        self.pane.set(pane);
        for (b, p) in self
            .pane_buttons
            .iter()
            .zip([Pane::Numbers, Pane::Functions, Pane::Bits])
        {
            if p == pane && !b.is_active() {
                b.set_active(true);
            }
        }
        self.pane_buttons[2].set_visible(mode == ButtonMode::Programming);
        self.pane_switcher
            .set_visible(narrow && mode != ButtonMode::Basic);
        if narrow {
            self.window.add_css_class("calc-narrow");
        } else {
            self.window.remove_css_class("calc-narrow");
        }
        for (extra, basic) in self.keypad_parts.borrow().iter() {
            if let Some(g) = extra {
                g.set_visible(!narrow || pane == Pane::Functions);
            }
            basic.set_visible(!narrow || pane == Pane::Numbers);
        }
        self.keypad.set_visible(!narrow || pane != Pane::Bits);
        let show_bits = !narrow || pane == Pane::Bits;
        self.bits.set_visible(show_bits);
        if let Some(scroll) = self.bits.ancestor(gtk::ScrolledWindow::static_type()) {
            scroll.set_visible(show_bits);
            // The bits take the room the keypad leaves.
            self.prog_bar.set_vexpand(narrow && pane == Pane::Bits);
            scroll.set_vexpand(narrow && pane == Pane::Bits);
        }
        let per_row = if narrow { 8 } else { 16 };
        if self.bits_per_row.replace(per_row) != per_row {
            self.rebuild_bits();
        }
    }

    fn update_digit_sensitivity(&self) {
        let base = self.state.borrow().options.base;
        for (b, d) in self.digit_buttons.borrow().iter() {
            b.set_sensitive(*d < base);
        }
    }

    fn convert_entry_base(&self, base: u32) {
        // Re-express the current value (if the entry holds one) in the new base.
        let text = self.entry.text().to_string();
        let value = {
            let mut s = self.state.borrow_mut();
            let v = if text.trim().is_empty() {
                None
            } else {
                s.ctx.clone().evaluate(&text).ok()
            };
            s.options.base = base;
            s.sync_ctx_mode();
            v
        };
        if let Some(v) = value {
            let s = self.state.borrow();
            self.entry.set_text(&format::format(
                v,
                &Options {
                    thousands: false,
                    ..s.options
                },
            ));
        }
        self.update_digit_sensitivity();
    }

    fn current_int(&self) -> Option<u64> {
        let s = self.state.borrow();
        let text = self.entry.text();
        if text.trim().is_empty() {
            return Some(0);
        }
        match s.ctx.clone().evaluate(&text) {
            Ok(Value::Int(v)) => Some(v),
            _ => None,
        }
    }

    fn rebuild_bits(&self) {
        while let Some(child) = self.bits.first_child() {
            self.bits.remove(&child);
        }
        self.bit_buttons.borrow_mut().clear();
        let bits = self.state.borrow().options.word_size;
        if self.state.borrow().mode != ButtonMode::Programming {
            return;
        }
        // 16 (narrow: 8) bits per row, MSB first, with a gap between
        // nibbles.
        let per_row = self.bits_per_row.get().min(bits);
        for bit in (0..bits).rev() {
            let (row, col) = bit_cell(bit, bits, per_row);
            let b = gtk::ToggleButton::builder()
                .label("0")
                .css_classes(["flat", "calc-bit"])
                .tooltip_text(trf!("Bit {bit}", bit = bit))
                .build();
            // "Bit 5, toggle button, pressed": the pressed state is the
            // bit's value.
            helix_app_common::set_accessible_label(&b, &trf!("Bit {bit}", bit = bit));
            if col % 4 == 0 && col > 0 {
                // A gap before each nibble.
                b.set_margin_start(8);
            }
            let ui = self.clone();
            b.connect_clicked(move |_| ui.toggle_bit(bit));
            self.bits.attach(&b, col as i32, row as i32 * 2, 1, 1);
            if bit % 4 == 0 {
                // The toggles carry their numbers; the captions are for the eye.
                let idx = gtk::Label::builder()
                    .label(bit.to_string())
                    .css_classes(["dim-label", "caption", "numeric"])
                    .accessible_role(gtk::AccessibleRole::Presentation)
                    .build();
                self.bits.attach(&idx, col as i32, row as i32 * 2 + 1, 1, 1);
            }
            self.bit_buttons.borrow_mut().push((b, bit));
        }
        self.refresh_bits();
    }

    fn refresh_bits(&self) {
        if self.state.borrow().mode != ButtonMode::Programming {
            return;
        }
        let value = self.current_int();
        for (b, bit) in self.bit_buttons.borrow().iter() {
            let on = value.is_some_and(|v| v >> bit & 1 == 1);
            b.set_label(if on { "1" } else { "0" });
            // Only "clicked" changes the value, so this does not recurse.
            b.set_active(on);
            b.set_sensitive(value.is_some());
        }
    }

    fn toggle_bit(&self, bit: u32) {
        let Some(v) = self.current_int() else { return };
        let v = v ^ (1u64 << bit);
        let text = {
            let s = self.state.borrow();
            format::format(
                Value::Int(v & crate::engine::mask(s.options.word_size)),
                &Options {
                    thousands: false,
                    ..s.options
                },
            )
        };
        self.entry.set_text(&text);
        self.entry.set_position(-1);
    }

    fn financial_dialog(&self, i: usize) {
        let f = &financial::FUNCTIONS[i];
        let dialog = adw::AlertDialog::builder().heading(tr(f.title)).build();
        let group = adw::PreferencesGroup::new();
        let rows: Vec<adw::EntryRow> = f
            .params
            .iter()
            .map(|p| {
                let row = adw::EntryRow::builder().title(tr(p)).build();
                group.add(&row);
                row
            })
            .collect();
        dialog.set_extra_child(Some(&group));
        dialog.add_responses(&[("cancel", tr("_Cancel")), ("calculate", tr("C_alculate"))]);
        dialog.set_response_appearance("calculate", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("calculate"));
        dialog.set_close_response("cancel");
        let ui = self.clone();
        dialog.connect_response(None, move |_, resp| {
            if resp != "calculate" {
                return;
            }
            let args: Vec<String> = rows
                .iter()
                .map(|r| {
                    let t = r.text().trim().to_string();
                    if t.is_empty() {
                        "0".into()
                    } else {
                        t
                    }
                })
                .collect();
            ui.entry
                .set_text(&format!("{}({})", f.name, args.join("; ")));
            ui.evaluate();
        });
        dialog.present(Some(&self.window));
    }

    fn show_preferences(&self) {
        let dialog = adw::PreferencesDialog::builder()
            .title(tr("Preferences"))
            .build();
        let page = adw::PreferencesPage::new();
        let group = adw::PreferencesGroup::builder()
            .title(tr("Results"))
            .build();
        let opts = self.state.borrow().options;
        let angle = self.state.borrow().ctx.angle;

        let format_row = adw::ComboRow::builder()
            .title(tr("Number _Format"))
            .use_underline(true)
            .model(&gtk::StringList::new(&NumberFormat::ALL.map(|f| f.label())))
            .build();
        format_row.set_selected(
            NumberFormat::ALL
                .iter()
                .position(|f| *f == opts.format)
                .unwrap_or(0) as u32,
        );
        let accuracy = adw::SpinRow::with_range(0.0, 15.0, 1.0);
        accuracy.set_title(tr("_Decimal Places"));
        accuracy.set_use_underline(true);
        accuracy.set_value(opts.accuracy as f64);
        let thousands = adw::SwitchRow::builder()
            .title(tr("_Thousands Separators"))
            .use_underline(true)
            .active(opts.thousands)
            .build();
        let zeroes = adw::SwitchRow::builder()
            .title(tr("_Trailing Zeroes"))
            .use_underline(true)
            .active(opts.show_zeroes)
            .build();
        group.add(&format_row);
        group.add(&accuracy);
        group.add(&thousands);
        group.add(&zeroes);

        let group2 = adw::PreferencesGroup::builder()
            .title(tr("Calculation"))
            .build();
        let angles = [AngleUnit::Degrees, AngleUnit::Radians, AngleUnit::Gradians];
        let angle_row = adw::ComboRow::builder()
            .title(tr("_Angle Units"))
            .use_underline(true)
            .model(&gtk::StringList::new(&[
                tr("Degrees"),
                tr("Radians"),
                tr("Gradians"),
            ]))
            .build();
        angle_row.set_selected(angles.iter().position(|a| *a == angle).unwrap_or(0) as u32);
        group2.add(&angle_row);

        page.add(&group);
        page.add(&group2);
        dialog.add(&page);

        let ui = self.clone();
        let update = move |f: &dyn Fn(&mut State)| {
            f(&mut ui.state.borrow_mut());
        };
        let u = update.clone();
        format_row.connect_selected_notify(move |r| {
            let f = NumberFormat::ALL[r.selected() as usize];
            u(&|s| {
                s.options.format = f;
                s.prefs.set_enum_nick("number-format", f.nick());
            });
        });
        let u = update.clone();
        accuracy.connect_value_notify(move |r| {
            let v = r.value() as usize;
            u(&|s| {
                s.options.accuracy = v;
                s.prefs.set("accuracy", v as i32);
            });
        });
        let u = update.clone();
        thousands.connect_active_notify(move |r| {
            let v = r.is_active();
            u(&|s| {
                s.options.thousands = v;
                s.prefs.set("show-thousands", v);
            });
        });
        let u = update.clone();
        zeroes.connect_active_notify(move |r| {
            let v = r.is_active();
            u(&|s| {
                s.options.show_zeroes = v;
                s.prefs.set("show-zeroes", v);
            });
        });
        let u = update;
        angle_row.connect_selected_notify(move |r| {
            let a = angles[r.selected() as usize];
            u(&|s| {
                s.ctx.angle = a;
                s.prefs.set_enum_nick("angle-units", a.nick());
            });
        });
        dialog.present(Some(&self.window));
    }
}

/// How a calculation is spoken: "12×3+4 equals 40".
fn spoken_result(expression: &str, result: &str) -> String {
    trf!("{} equals {}", expression.trim(), result)
}

#[cfg(test)]
mod key_name_tests {
    use super::*;

    #[test]
    fn bits_are_laid_out_msb_first() {
        // 16-bit word, 16 per row: bit 15 first, bit 0 last.
        assert_eq!(bit_cell(15, 16, 16), (0, 0));
        assert_eq!(bit_cell(0, 16, 16), (0, 15));
        // 64-bit word, 8 per row: bit 63 starts row 0, bit 0 ends row 7.
        assert_eq!(bit_cell(63, 64, 8), (0, 0));
        assert_eq!(bit_cell(0, 64, 8), (7, 7));
        assert_eq!(bit_cell(8, 64, 8), (6, 7));
    }

    #[test]
    fn names_drop_shortcut_hints() {
        assert_eq!(
            key_name("√", Some("Square root [Ctrl+R]")).as_deref(),
            Some("Square root")
        );
        assert_eq!(
            key_name("C", Some("Clear [Escape]")).as_deref(),
            Some("Clear")
        );
        assert_eq!(
            key_name("⌫", Some("Backspace")).as_deref(),
            Some("Backspace")
        );
        assert_eq!(key_name("(", None).as_deref(), Some("Open parenthesis"));
        assert_eq!(key_name("7", None), None);
        assert_eq!(key_name("A", None), None);
    }

    #[test]
    fn shortcut_labels() {
        assert_eq!(shortcut_label("<Control>comma"), "Ctrl+,");
        assert_eq!(shortcut_label("<Alt>1"), "Alt+1");
        assert_eq!(shortcut_label("<Control>r"), "Ctrl+R");
        assert_eq!(shortcut_label("Return"), "Enter");
        assert_eq!(shortcut_label("Escape"), "Escape");
        assert_eq!(shortcut_label("slash"), "/");
    }

    /// No two shortcuts share a key, and the keys the entry needs for
    /// editing (Select All, clipboard) are not taken; F1 is not About.
    #[test]
    fn shortcuts_do_not_conflict() {
        let mut seen = std::collections::HashSet::new();
        for s in SHORTCUTS {
            let t = s.how.trigger().to_lowercase();
            assert!(seen.insert(t.clone()), "{t} is used twice");
            assert_eq!(s.action.is_empty(), matches!(s.how, How::Entry(_)));
        }
        for reserved in ["<control>a", "<control>c", "<control>v", "<control>x", "f1"] {
            assert!(!seen.contains(reserved), "{reserved} is taken");
        }
    }

    /// Every shortcut a keypad tooltip shows ("Square [Ctrl+2]") exists.
    #[test]
    fn tooltip_hints_are_real_shortcuts() {
        let labels: Vec<String> = SHORTCUTS
            .iter()
            .map(|s| shortcut_label(s.how.trigger()))
            .collect();
        let all = basic_keys(false)
            .into_iter()
            .chain(basic_keys(true))
            .chain(advanced_keys())
            .chain(programming_keys())
            .chain(financial_keys())
            .flatten();
        for key in all {
            let Some(tip) = key.tooltip else { continue };
            if let (Some(a), true) = (tip.rfind(" ["), tip.ends_with(']')) {
                let hint = &tip[a + 2..tip.len() - 1];
                assert!(labels.iter().any(|l| l == hint), "{tip}: no {hint}");
            }
        }
    }

    #[test]
    fn results_are_spoken_as_one_phrase() {
        assert_eq!(spoken_result(" 12×3+4 ", "40"), "12×3+4 equals 40");
    }

    /// Every key whose label is not a plain digit / letter / word gets a
    /// name, and no name contains a shortcut hint.
    #[test]
    fn every_symbol_key_is_named() {
        let all = basic_keys(false)
            .into_iter()
            .chain(basic_keys(true))
            .chain(advanced_keys())
            .chain(programming_keys())
            .chain(financial_keys())
            .flatten();
        for key in all {
            let plain = key.label.chars().all(|c| c.is_ascii_alphanumeric());
            let name = key_name(key.label, key.tooltip);
            if !plain {
                assert!(name.is_some(), "{} has no name", key.label);
            }
            if let Some(n) = name {
                assert!(!n.contains('[') && !n.contains("Ctrl"), "{n}");
                assert!(!n.is_empty());
            }
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait_for(what: &str, done: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(20);
        let ctx = glib::MainContext::default();
        while !done() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            if !ctx.iteration(false) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    /// Runs the main loop for a few frames, so the window is laid out
    /// again.
    fn settle() {
        let until = Instant::now() + Duration::from_millis(300);
        let ctx = glib::MainContext::default();
        while Instant::now() < until {
            if !ctx.iteration(false) {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn min_width(w: &impl IsA<gtk::Widget>) -> i32 {
        w.measure(gtk::Orientation::Horizontal, -1).0
    }

    /// A long expression or result never widens the window (#198: adding
    /// up `584+455+…+3` pushed the keypad past the right edge): the
    /// window's minimum width stays what it is with an empty display, and
    /// the newest history row, the entry and the keypad stay inside it.
    #[test]
    fn long_expressions_do_not_widen_the_window() {
        // History and window state go to a scratch home.
        let home =
            std::env::temp_dir().join(format!("helix-calculator-test-{}", std::process::id()));
        for (var, dir) in [
            ("XDG_DATA_HOME", "data"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_STATE_HOME", "state"),
            ("XDG_CACHE_HOME", "cache"),
        ] {
            std::env::set_var(var, home.join(dir));
        }
        let Some(_display) = helix_app_common::testing::headless_gtk() else {
            eprintln!("no headless GTK display (gtk4-broadwayd); skipping");
            return;
        };
        adw::init().unwrap();
        let app = adw::Application::builder()
            .application_id("org.helixdesktop.Calculator.Test")
            .flags(gio::ApplicationFlags::NON_UNIQUE)
            .build();
        app.register(gio::Cancellable::NONE).unwrap();
        load_css();
        let ui = build(&app);
        ui.window.set_default_size(360, 600);
        ui.window.present();
        let content = ui.window.content().unwrap();
        // The narrow layout applies once the window has its width.
        wait_for("the narrow layout", || {
            ui.window.width() == 360 && min_width(&content) <= 360
        });
        settle();
        let empty = min_width(&content);
        let long = format!("{}+1", "9".repeat(200));
        for expr in ["584+455+79+64+48+42+38+25+13+14+8+4+3", long.as_str()] {
            ui.entry.set_text(expr);
            ui.evaluate();
            let result = ui.state.borrow().history.last().unwrap().result.clone();
            assert_eq!(
                ui.entry.text(),
                result,
                "{expr}: the result is in the entry"
            );
            let now = min_width(&content);
            assert!(
                now <= empty,
                "{expr}: the window needs {now} px, not {empty}"
            );

            // Laid out at the window's width, the history row, the entry
            // and the keypad end inside the window.
            let width = ui.window.width() as f32;
            let row = ui.history.last_child().unwrap();
            wait_for("the new history row", || row.width() > 0);
            settle();
            for (what, w) in [
                ("history row", row.clone()),
                ("entry", ui.entry.clone().upcast()),
                ("keypad", ui.keypad.clone().upcast()),
            ] {
                let b = w.compute_bounds(&ui.window).unwrap();
                assert!(
                    b.x() >= 0.0 && b.x() + b.width() <= width,
                    "{expr}: the {what} spans {}..{} of {width}",
                    b.x(),
                    b.x() + b.width()
                );
            }
            ui.entry.set_text("");
        }
        // Nor does an error that quotes a long name.
        ui.entry.set_text(&"x".repeat(200));
        ui.evaluate();
        assert!(ui.error.is_visible());
        let now = min_width(&content);
        assert!(now <= empty, "an error needs {now} px, not {empty}");
        ui.window.destroy();
        let _ = std::fs::remove_dir_all(&home);
    }
}
