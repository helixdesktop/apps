//! Printers through CUPS: reading printers, their jobs and options over
//! IPP (RFC 8010/8011, with CUPS's own operations), and changing them
//! through cups-pk-helper (`org.opensuse.CupsPkHelper.Mechanism` on the
//! system bus), which asks polkit, as GNOME Settings does. Blocking;
//! worker threads only.
//!
//! IPP goes to CUPS's local socket (`CUPS_SERVER`, else
//! `/run/cups/cups.sock`, else `localhost:631`); every function takes the
//! [`Server`], so tests run against a fake IPP server.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::time::Duration;

use helix_app_common::i18n::{n_, tr, trc};
use zbus::blocking::Connection;

use crate::bus::{self, Obj, Result};

const PK_HELPER: Obj<'static> = Obj::new(
    "org.opensuse.CupsPkHelper.Mechanism",
    "/",
    "org.opensuse.CupsPkHelper.Mechanism",
);

/// Where CUPS's own test page is.
pub const TEST_PAGE: &str = "/usr/share/cups/data/testprint";
/// The scheduler, to tell "not installed" from "not running".
pub const CUPSD: &str = "/usr/sbin/cupsd";

/// How to reach the CUPS scheduler.
#[derive(Debug, Clone, PartialEq)]
pub enum Server {
    Socket(std::path::PathBuf),
    Tcp(String),
}

impl Server {
    /// The scheduler of this system, as CUPS's own tools find it.
    pub fn local() -> Self {
        if let Some(s) = std::env::var_os("CUPS_SERVER") {
            let s = s.to_string_lossy().into_owned();
            if s.starts_with('/') {
                return Server::Socket(s.into());
            }
            return Server::Tcp(if s.contains(':') {
                s
            } else {
                format!("{s}:631")
            });
        }
        let sock = std::path::Path::new("/run/cups/cups.sock");
        if sock.exists() {
            Server::Socket(sock.into())
        } else {
            Server::Tcp("localhost:631".into())
        }
    }
}

// ----- IPP encoding --------------------------------------------------------

mod tag {
    pub const OPERATION: u8 = 0x01;
    pub const JOB: u8 = 0x02;
    pub const END: u8 = 0x03;
    pub const PRINTER: u8 = 0x04;
    pub const INTEGER: u8 = 0x21;
    pub const BOOLEAN: u8 = 0x22;
    pub const ENUM: u8 = 0x23;
    pub const BEG_COLLECTION: u8 = 0x34;
    pub const TEXT_LANG: u8 = 0x35;
    pub const NAME_LANG: u8 = 0x36;
    pub const END_COLLECTION: u8 = 0x37;
    // Written by the tests' fake server only.
    #[cfg(test)]
    pub const TEXT: u8 = 0x41;
    pub const NAME: u8 = 0x42;
    pub const KEYWORD: u8 = 0x44;
    pub const URI: u8 = 0x45;
    pub const CHARSET: u8 = 0x47;
    pub const LANGUAGE: u8 = 0x48;
    pub const MIME: u8 = 0x49;
    #[cfg(test)]
    pub const MEMBER_NAME: u8 = 0x4A;
}

/// IPP operations used here.
pub mod op {
    pub const PRINT_JOB: u16 = 0x0002;
    pub const GET_JOBS: u16 = 0x000A;
    pub const GET_PRINTER_ATTRIBUTES: u16 = 0x000B;
    pub const CUPS_GET_DEFAULT: u16 = 0x4001;
    pub const CUPS_GET_PRINTERS: u16 = 0x4002;
}

/// One attribute value.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i32),
    Bool(bool),
    Text(String),
    /// Anything else (dates, resolutions, ranges, collections): kept raw.
    Other,
}

impl Value {
    fn text(&self) -> Option<&str> {
        match self {
            Value::Text(t) => Some(t),
            _ => None,
        }
    }
}

/// One attribute group of a request or response.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Group {
    pub tag: u8,
    pub attrs: Vec<(String, Vec<Value>)>,
}

impl Group {
    pub fn get(&self, name: &str) -> Option<&[Value]> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_slice())
    }
    pub fn text(&self, name: &str) -> String {
        self.get(name)
            .and_then(|v| v.first())
            .and_then(Value::text)
            .unwrap_or_default()
            .to_owned()
    }
    pub fn texts(&self, name: &str) -> Vec<String> {
        self.get(name)
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.text().map(str::to_owned))
            .collect()
    }
    pub fn int(&self, name: &str) -> Option<i32> {
        match self.get(name)?.first()? {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.get(name)?.first()? {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

/// An IPP message: a request (`code` is the operation) or a response
/// (`code` is the status).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Message {
    pub code: u16,
    pub request_id: u32,
    pub groups: Vec<Group>,
}

impl Message {
    pub fn groups(&self, tag: u8) -> impl Iterator<Item = &Group> {
        self.groups.iter().filter(move |g| g.tag == tag)
    }
}

/// A request's operation attributes, built in order.
pub struct Request {
    op: u16,
    attrs: Vec<(u8, String, Vec<Vec<u8>>)>,
}

impl Request {
    pub fn new(op: u16) -> Self {
        let mut r = Request {
            op,
            attrs: Vec::new(),
        };
        r.add(tag::CHARSET, "attributes-charset", ["utf-8"]);
        r.add(tag::LANGUAGE, "attributes-natural-language", ["en"]);
        r
    }
    fn add<'a>(&mut self, t: u8, name: &str, values: impl IntoIterator<Item = &'a str>) {
        let values = values.into_iter().map(|v| v.as_bytes().to_vec()).collect();
        self.attrs.push((t, name.to_owned(), values));
    }
    pub fn uri(mut self, name: &str, v: &str) -> Self {
        self.add(tag::URI, name, [v]);
        self
    }
    pub fn name(mut self, name: &str, v: &str) -> Self {
        self.add(tag::NAME, name, [v]);
        self
    }
    pub fn keyword(mut self, name: &str, v: &str) -> Self {
        self.add(tag::KEYWORD, name, [v]);
        self
    }
    pub fn keywords(mut self, name: &str, v: &[&str]) -> Self {
        self.add(tag::KEYWORD, name, v.iter().copied());
        self
    }
    pub fn mime(mut self, name: &str, v: &str) -> Self {
        self.add(tag::MIME, name, [v]);
        self
    }
    pub fn encode(&self, request_id: u32) -> Vec<u8> {
        let mut out = vec![2, 0];
        out.extend_from_slice(&self.op.to_be_bytes());
        out.extend_from_slice(&request_id.to_be_bytes());
        out.push(tag::OPERATION);
        for (t, name, values) in &self.attrs {
            for (i, v) in values.iter().enumerate() {
                out.push(*t);
                let n = if i == 0 { name.as_bytes() } else { &[] };
                out.extend_from_slice(&(n.len() as u16).to_be_bytes());
                out.extend_from_slice(n);
                out.extend_from_slice(&(v.len() as u16).to_be_bytes());
                out.extend_from_slice(v);
            }
        }
        out.push(tag::END);
        out
    }
}

/// Decodes an IPP message (request or response); `None` if it is cut
/// short or malformed. Collections are skipped (their values are
/// [`Value::Other`]).
pub fn decode(data: &[u8]) -> Option<(Message, usize)> {
    let mut p = 0;
    let take = |p: &mut usize, n: usize| -> Option<&[u8]> {
        let s = data.get(*p..*p + n)?;
        *p += n;
        Some(s)
    };
    let u16_at = |p: &mut usize| -> Option<u16> {
        let b = take(p, 2)?;
        Some(u16::from_be_bytes([b[0], b[1]]))
    };
    take(&mut p, 2)?;
    let code = u16_at(&mut p)?;
    let id = take(&mut p, 4)?;
    let request_id = u32::from_be_bytes([id[0], id[1], id[2], id[3]]);
    let mut msg = Message {
        code,
        request_id,
        groups: Vec::new(),
    };
    let mut depth = 0usize;
    loop {
        let t = *take(&mut p, 1)?.first()?;
        if t == tag::END && depth == 0 {
            return Some((msg, p));
        }
        if t < 0x10 {
            msg.groups.push(Group {
                tag: t,
                attrs: Vec::new(),
            });
            continue;
        }
        let n = u16_at(&mut p)? as usize;
        let name = String::from_utf8_lossy(take(&mut p, n)?).into_owned();
        let l = u16_at(&mut p)? as usize;
        let raw = take(&mut p, l)?;
        if t == tag::BEG_COLLECTION {
            depth += 1;
        }
        if depth > 0 {
            if t == tag::END_COLLECTION {
                depth -= 1;
            }
            // The collection itself is one opaque value of its attribute.
            if t == tag::BEG_COLLECTION && depth == 1 {
                push(&mut msg, name, Value::Other);
            }
            continue;
        }
        let value = match t {
            tag::INTEGER | tag::ENUM if l == 4 => {
                Value::Int(i32::from_be_bytes([raw[0], raw[1], raw[2], raw[3]]))
            }
            tag::BOOLEAN if l == 1 => Value::Bool(raw[0] != 0),
            tag::TEXT_LANG | tag::NAME_LANG => {
                // language length, language, text length, text
                let ll = u16::from_be_bytes([*raw.first()?, *raw.get(1)?]) as usize;
                let rest = raw.get(2 + ll..)?;
                let tl = u16::from_be_bytes([*rest.first()?, *rest.get(1)?]) as usize;
                Value::Text(String::from_utf8_lossy(rest.get(2..2 + tl)?).into_owned())
            }
            0x30 | 0x40..=0x49 => Value::Text(String::from_utf8_lossy(raw).into_owned()),
            _ => Value::Other,
        };
        push(&mut msg, name, value);
    }
}

fn push(msg: &mut Message, name: String, value: Value) {
    let Some(group) = msg.groups.last_mut() else {
        return;
    };
    if name.is_empty() {
        if let Some((_, values)) = group.attrs.last_mut() {
            values.push(value);
        }
    } else {
        group.attrs.push((name, vec![value]));
    }
}

/// Encodes a response, for tests and the fake server.
#[cfg(test)]
pub fn encode_response(status: u16, request_id: u32, groups: &[Group]) -> Vec<u8> {
    let mut out = vec![2, 0];
    out.extend_from_slice(&status.to_be_bytes());
    out.extend_from_slice(&request_id.to_be_bytes());
    out.push(tag::OPERATION);
    for (name, t, v) in [
        ("attributes-charset", tag::CHARSET, "utf-8"),
        ("attributes-natural-language", tag::LANGUAGE, "en"),
    ] {
        out.push(t);
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(v.len() as u16).to_be_bytes());
        out.extend_from_slice(v.as_bytes());
    }
    for g in groups {
        out.push(g.tag);
        for (name, values) in &g.attrs {
            for (i, v) in values.iter().enumerate() {
                let (t, bytes) = match v {
                    Value::Int(n) => (tag::INTEGER, n.to_be_bytes().to_vec()),
                    Value::Bool(b) => (tag::BOOLEAN, vec![u8::from(*b)]),
                    Value::Text(s) => (tag::TEXT, s.as_bytes().to_vec()),
                    Value::Other => (0x31, vec![0; 11]),
                };
                out.push(t);
                let n = if i == 0 { name.as_bytes() } else { &[] };
                out.extend_from_slice(&(n.len() as u16).to_be_bytes());
                out.extend_from_slice(n);
                out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
                out.extend_from_slice(&bytes);
            }
        }
    }
    out.push(tag::END);
    out
}

// ----- Transport -----------------------------------------------------------

trait Stream: Read + Write {}
impl<T: Read + Write> Stream for T {}

fn open(server: &Server) -> std::io::Result<Box<dyn Stream>> {
    let timeout = Some(Duration::from_secs(15));
    Ok(match server {
        Server::Socket(path) => {
            let s = std::os::unix::net::UnixStream::connect(path)?;
            s.set_read_timeout(timeout)?;
            s.set_write_timeout(timeout)?;
            Box::new(s)
        }
        Server::Tcp(addr) => {
            let s = std::net::TcpStream::connect(addr)?;
            s.set_read_timeout(timeout)?;
            s.set_write_timeout(timeout)?;
            Box::new(s)
        }
    })
}

/// Sends `request` (plus a document) to `resource` and returns the
/// response. CUPS not answering is [`bus::Error::Missing`].
pub fn send(
    server: &Server,
    resource: &str,
    request: &Request,
    document: &[u8],
) -> Result<Message> {
    let mut body = request.encode(1);
    body.extend_from_slice(document);
    let mut s = open(server).map_err(|_| bus::Error::Missing)?;
    let head = format!(
        "POST {resource} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let io = |e: std::io::Error| bus::Error::Other(e.to_string());
    s.write_all(head.as_bytes()).map_err(io)?;
    s.write_all(&body).map_err(io)?;
    s.flush().map_err(io)?;
    let mut r = BufReader::new(s);
    let mut status = String::new();
    r.read_line(&mut status).map_err(io)?;
    let code: u16 = status
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let (mut length, mut chunked) = (None, false);
    loop {
        let mut line = String::new();
        if r.read_line(&mut line).map_err(io)? == 0 {
            break;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim();
            match k.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = v.parse::<usize>().ok(),
                "transfer-encoding" => chunked = v.eq_ignore_ascii_case("chunked"),
                _ => {}
            }
        }
    }
    let mut data = Vec::new();
    if chunked {
        loop {
            let mut size = String::new();
            r.read_line(&mut size).map_err(io)?;
            let n = usize::from_str_radix(size.trim().split(';').next().unwrap_or("0"), 16)
                .unwrap_or(0);
            if n == 0 {
                break;
            }
            let mut chunk = vec![0; n];
            r.read_exact(&mut chunk).map_err(io)?;
            data.extend_from_slice(&chunk);
            let mut crlf = [0; 2];
            r.read_exact(&mut crlf).map_err(io)?;
        }
    } else if let Some(n) = length {
        data.resize(n, 0);
        r.read_exact(&mut data).map_err(io)?;
    } else {
        r.read_to_end(&mut data).map_err(io)?;
    }
    if code != 200 {
        return Err(bus::Error::Other(format!("HTTP {code}")));
    }
    decode(&data)
        .map(|(m, _)| m)
        .ok_or_else(|| bus::Error::Other("bad IPP response".into()))
}

const NOT_FOUND: u16 = 0x0406;

fn ipp_error(m: &Message) -> bus::Error {
    let message = m
        .groups(tag::OPERATION)
        .map(|g| g.text("status-message"))
        .find(|s| !s.is_empty());
    match m.code {
        0x0401 | 0x0403 => bus::Error::Denied,
        _ => bus::Error::Other(message.unwrap_or_else(|| format!("IPP status {:#06x}", m.code))),
    }
}

fn success(m: &Message) -> bool {
    m.code < 0x0100
}

/// The user the requests are made for.
fn user() -> String {
    std::env::var("USER").unwrap_or_else(|_| "anonymous".into())
}

fn printer_uri(name: &str) -> String {
    format!("ipp://localhost/printers/{}", urlencode(name))
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}

// ----- Reading -------------------------------------------------------------

/// A printer's state (`printer-state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrinterState {
    Idle,
    Printing,
    Stopped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Printer {
    pub name: String,
    /// The name people see (`printer-info`, else the queue name).
    pub info: String,
    pub location: String,
    pub model: String,
    pub device_uri: String,
    pub state: PrinterState,
    /// `printer-state-reasons` without "none".
    pub reasons: Vec<String>,
    pub message: String,
    pub accepting: bool,
    pub jobs: i32,
}

impl Printer {
    pub fn title(&self) -> &str {
        if self.info.is_empty() {
            &self.name
        } else {
            &self.info
        }
    }

    /// What the printer is doing, for its row: the most important
    /// state reason when there is one.
    pub fn status(&self) -> String {
        if let Some(reason) = self.reasons.iter().find_map(|r| reason_label(r)) {
            return reason.to_owned();
        }
        match self.state {
            PrinterState::Stopped => tr("Paused").to_owned(),
            PrinterState::Printing => tr("Printing").to_owned(),
            PrinterState::Idle => tr("Ready").to_owned(),
        }
    }
}

/// Labels for the state reasons people can act on (RFC 8011 5.4.12,
/// PWG 5100.9), without the "-error"/"-warning"/"-report" suffix.
const REASONS: &[(&str, &str)] = &[
    ("media-empty", n_("Out of paper")),
    ("media-needed", n_("Out of paper")),
    ("media-jam", n_("Paper jam")),
    ("toner-empty", n_("Out of toner")),
    ("marker-supply-empty", n_("Out of ink")),
    ("toner-low", n_("Low on toner")),
    ("marker-supply-low", n_("Low on ink")),
    ("door-open", n_("Door open")),
    ("cover-open", n_("Cover open")),
    ("offline", n_("Offline")),
    ("connecting-to-device", n_("Connecting…")),
    ("paused", n_("Paused")),
];

fn reason_label(reason: &str) -> Option<&'static str> {
    let base = ["-error", "-warning", "-report"]
        .iter()
        .fold(reason, |r, s| r.strip_suffix(s).unwrap_or(r));
    // Reports are informational only.
    if reason.ends_with("-report") {
        return None;
    }
    REASONS.iter().find(|(k, _)| *k == base).map(|(_, l)| tr(l))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct State {
    pub printers: Vec<Printer>,
    pub default: Option<String>,
}

const PRINTER_ATTRS: &[&str] = &[
    "printer-name",
    "printer-info",
    "printer-location",
    "printer-make-and-model",
    "printer-state",
    "printer-state-reasons",
    "printer-state-message",
    "printer-is-accepting-jobs",
    "device-uri",
    "queued-job-count",
];

fn printer_of(g: &Group) -> Printer {
    Printer {
        name: g.text("printer-name"),
        info: g.text("printer-info"),
        location: g.text("printer-location"),
        model: g.text("printer-make-and-model"),
        device_uri: g.text("device-uri"),
        state: match g.int("printer-state") {
            Some(4) => PrinterState::Printing,
            Some(5) => PrinterState::Stopped,
            _ => PrinterState::Idle,
        },
        reasons: g
            .texts("printer-state-reasons")
            .into_iter()
            .filter(|r| r != "none")
            .collect(),
        message: g.text("printer-state-message"),
        accepting: g.bool("printer-is-accepting-jobs").unwrap_or(true),
        jobs: g.int("queued-job-count").unwrap_or(0),
    }
}

/// The printers and the default one.
pub fn read(server: &Server) -> Result<State> {
    let r = Request::new(op::CUPS_GET_PRINTERS)
        .name("requesting-user-name", &user())
        .keywords("requested-attributes", PRINTER_ATTRS);
    let m = send(server, "/", &r, &[])?;
    let mut printers: Vec<Printer> = if success(&m) {
        m.groups(tag::PRINTER)
            .map(printer_of)
            .filter(|p| !p.name.is_empty())
            .collect()
    } else if m.code == NOT_FOUND {
        Vec::new()
    } else {
        return Err(ipp_error(&m));
    };
    printers.sort_by_key(|p| p.title().to_lowercase());
    let d = Request::new(op::CUPS_GET_DEFAULT)
        .name("requesting-user-name", &user())
        .keywords("requested-attributes", &["printer-name"]);
    let dm = send(server, "/", &d, &[])?;
    let default = success(&dm)
        .then(|| {
            dm.groups(tag::PRINTER)
                .map(|g| g.text("printer-name"))
                .find(|n| !n.is_empty())
        })
        .flatten();
    Ok(State { printers, default })
}

/// A job's state (`job-state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Pending,
    Held,
    Printing,
    Stopped,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    pub id: i32,
    pub name: String,
    pub user: String,
    pub state: JobState,
}

impl Job {
    pub fn status(&self) -> &'static str {
        match self.state {
            JobState::Pending => tr("Waiting"),
            JobState::Held => tr("Held"),
            JobState::Printing => tr("Printing"),
            JobState::Stopped => tr("Stopped"),
        }
    }
}

/// The jobs of `printer` that have not finished.
pub fn jobs(server: &Server, printer: &str) -> Result<Vec<Job>> {
    let r = Request::new(op::GET_JOBS)
        .uri("printer-uri", &printer_uri(printer))
        .name("requesting-user-name", &user())
        .keyword("which-jobs", "not-completed")
        .keywords(
            "requested-attributes",
            &[
                "job-id",
                "job-name",
                "job-state",
                "job-originating-user-name",
            ],
        );
    let m = send(server, "/", &r, &[])?;
    if m.code == NOT_FOUND {
        return Ok(Vec::new());
    }
    if !success(&m) {
        return Err(ipp_error(&m));
    }
    Ok(m.groups(tag::JOB)
        .filter_map(|g| {
            Some(Job {
                id: g.int("job-id")?,
                name: g.text("job-name"),
                user: g.text("job-originating-user-name"),
                state: match g.int("job-state")? {
                    3 => JobState::Pending,
                    4 => JobState::Held,
                    5 => JobState::Printing,
                    6 => JobState::Stopped,
                    _ => return None,
                },
            })
        })
        .collect())
}

/// A printer option people choose a default for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptionKind {
    PaperSize,
    TwoSided,
    Color,
    Quality,
}

impl OptionKind {
    pub const ALL: [OptionKind; 4] = [
        OptionKind::PaperSize,
        OptionKind::TwoSided,
        OptionKind::Color,
        OptionKind::Quality,
    ];

    /// The IPP attribute (its `-supported` and `-default` forms are read,
    /// the option of this name is set).
    pub fn attribute(self) -> &'static str {
        match self {
            OptionKind::PaperSize => "media",
            OptionKind::TwoSided => "sides",
            OptionKind::Color => "print-color-mode",
            OptionKind::Quality => "print-quality",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            OptionKind::PaperSize => tr("Paper Size"),
            OptionKind::TwoSided => tr("Two-Sided"),
            OptionKind::Color => tr("Color"),
            OptionKind::Quality => tr("Quality"),
        }
    }
}

/// One option's choices and default, as values (IPP keywords, or the
/// enum's number for quality) with their labels.
#[derive(Debug, Clone, PartialEq)]
pub struct Choices {
    pub kind: OptionKind,
    pub values: Vec<String>,
    pub default: Option<String>,
}

/// The options of `printer` that offer more than one choice.
pub fn options(server: &Server, printer: &str) -> Result<Vec<Choices>> {
    let mut wanted = Vec::new();
    for k in OptionKind::ALL {
        wanted.push(format!("{}-supported", k.attribute()));
        wanted.push(format!("{}-default", k.attribute()));
    }
    let wanted: Vec<&str> = wanted.iter().map(String::as_str).collect();
    let r = Request::new(op::GET_PRINTER_ATTRIBUTES)
        .uri("printer-uri", &printer_uri(printer))
        .name("requesting-user-name", &user())
        .keywords("requested-attributes", &wanted);
    let m = send(server, "/", &r, &[])?;
    if !success(&m) {
        return Err(ipp_error(&m));
    }
    let Some(g) = m.groups(tag::PRINTER).next() else {
        return Ok(Vec::new());
    };
    let as_string = |v: &Value| match v {
        Value::Text(t) => Some(t.clone()),
        Value::Int(i) => Some(i.to_string()),
        _ => None,
    };
    Ok(OptionKind::ALL
        .into_iter()
        .filter_map(|kind| {
            let a = kind.attribute();
            let values: Vec<String> = g
                .get(&format!("{a}-supported"))?
                .iter()
                .filter_map(as_string)
                .collect();
            let default = g
                .get(&format!("{a}-default"))
                .and_then(|v| v.first())
                .and_then(as_string);
            (values.len() > 1).then_some(Choices {
                kind,
                values,
                default,
            })
        })
        .collect())
}

/// How a choice is shown: known keywords get words, paper sizes their
/// name ("iso_a4_210x297mm" is "A4").
pub fn choice_label(kind: OptionKind, value: &str) -> String {
    let known = match (kind, value) {
        (OptionKind::TwoSided, "one-sided") => Some(tr("Off")),
        (OptionKind::TwoSided, "two-sided-long-edge") => Some(tr("Long Edge (Standard)")),
        (OptionKind::TwoSided, "two-sided-short-edge") => Some(tr("Short Edge (Flip)")),
        (OptionKind::Color, "color") => Some(tr("Color")),
        (OptionKind::Color, "monochrome") => Some(tr("Grayscale")),
        (OptionKind::Color, "auto") => Some(tr("Automatic")),
        (OptionKind::Quality, "3") => return trc("print quality", "Draft"),
        (OptionKind::Quality, "4") => return trc("print quality", "Normal"),
        (OptionKind::Quality, "5") => return trc("print quality", "High"),
        _ => None,
    };
    if let Some(l) = known {
        return l.to_owned();
    }
    if kind == OptionKind::PaperSize {
        return media_label(value);
    }
    value.to_owned()
}

/// A PWG media name's size name ("na_letter_8.5x11in" is "Letter",
/// "iso_a4_210x297mm" is "A4").
pub fn media_label(media: &str) -> String {
    let mut parts = media.split('_');
    let (Some(_class), Some(name)) = (parts.next(), parts.next()) else {
        return media.to_owned();
    };
    if parts.next().is_none() {
        return media.to_owned();
    }
    let mut c = name.chars();
    match c.next() {
        Some(_) if name.len() <= 3 => name.to_uppercase(),
        Some(first) => first
            .to_uppercase()
            .chain(c)
            .collect::<String>()
            .replace('-', " "),
        None => media.to_owned(),
    }
}

/// Prints CUPS's test page on `printer`; the job's id.
pub fn print_test_page(server: &Server, printer: &str, page: &[u8]) -> Result<i32> {
    let r = Request::new(op::PRINT_JOB)
        .uri("printer-uri", &printer_uri(printer))
        .name("requesting-user-name", &user())
        .name("job-name", tr("Test Page"))
        // CUPS works out the page's type (a PDF banner since CUPS 2.3).
        .mime("document-format", "application/octet-stream");
    let m = send(
        server,
        &format!("/printers/{}", urlencode(printer)),
        &r,
        page,
    )?;
    if !success(&m) {
        return Err(ipp_error(&m));
    }
    let id = m.groups(tag::JOB).find_map(|g| g.int("job-id"));
    Ok(id.unwrap_or(0))
}

// ----- Changing (cups-pk-helper) -------------------------------------------

/// cups-pk-helper answers with an error string ("" when it worked).
fn helper(
    conn: &Connection,
    method: &str,
    body: &(impl serde::ser::Serialize + zbus::zvariant::DynamicType),
) -> Result<()> {
    let error: String = PK_HELPER.call_interactive(conn, method, body)?;
    if error.is_empty() {
        Ok(())
    } else {
        Err(bus::Error::Other(error))
    }
}

pub fn set_default(conn: &Connection, printer: &str) -> Result<()> {
    helper(conn, "PrinterSetDefault", &(printer,))
}

/// Pauses or resumes a printer.
pub fn set_enabled(conn: &Connection, printer: &str, on: bool) -> Result<()> {
    helper(conn, "PrinterSetEnabled", &(printer, on))
}

pub fn remove(conn: &Connection, printer: &str) -> Result<()> {
    helper(conn, "PrinterDelete", &(printer,))
}

/// Adds a driverless (IPP Everywhere) printer at `uri`.
pub fn add(conn: &Connection, name: &str, uri: &str, info: &str, location: &str) -> Result<()> {
    helper(
        conn,
        "PrinterAdd",
        &(name, uri, "everywhere", info, location),
    )
}

pub fn set_option_default(
    conn: &Connection,
    printer: &str,
    option: &str,
    value: &str,
) -> Result<()> {
    helper(
        conn,
        "PrinterAddOptionDefault",
        &(printer, option, vec![value]),
    )
}

pub fn cancel_job(conn: &Connection, job: i32) -> Result<()> {
    helper(conn, "JobCancelPurge", &(job, false))
}

/// Holds a job (`hold`) or lets it print.
pub fn hold_job(conn: &Connection, job: i32, hold: bool) -> Result<()> {
    helper(
        conn,
        "JobSetHoldUntil",
        &(job, if hold { "indefinite" } else { "no-hold" }),
    )
}

/// A printer the scheduler can see but that is not set up.
#[derive(Debug, Clone, PartialEq)]
pub struct Device {
    pub uri: String,
    pub info: String,
    pub model: String,
    pub location: String,
}

impl Device {
    pub fn title(&self) -> &str {
        if !self.info.is_empty() {
            &self.info
        } else if !self.model.is_empty() {
            &self.model
        } else {
            &self.uri
        }
    }
}

/// Printers CUPS finds (network ones through DNS-SD, IPP-over-USB ones),
/// that can be set up without a driver; it takes up to `timeout` seconds.
pub fn devices(conn: &Connection, timeout: i32) -> Result<Vec<Device>> {
    let empty: Vec<&str> = Vec::new();
    let (error, found): (String, std::collections::HashMap<String, String>) =
        PK_HELPER.call_interactive(conn, "DevicesGet", &(timeout, 0i32, &empty, &empty))?;
    if !error.is_empty() {
        return Err(bus::Error::Other(error));
    }
    Ok(parse_devices(&found))
}

/// cups-pk-helper's devices: `device-uri:N`, `device-info:N` and so on.
pub fn parse_devices(found: &std::collections::HashMap<String, String>) -> Vec<Device> {
    let mut by: BTreeMap<u32, BTreeMap<&str, &str>> = BTreeMap::new();
    for (k, v) in found {
        if let Some((key, n)) = k.rsplit_once(':') {
            if let Ok(n) = n.parse::<u32>() {
                by.entry(n).or_default().insert(key, v);
            }
        }
    }
    let mut out: Vec<Device> = by
        .values()
        .filter_map(|d| {
            let uri = d.get("device-uri")?.to_string();
            if !driverless(&uri) {
                return None;
            }
            Some(Device {
                uri,
                info: d.get("device-info").unwrap_or(&"").to_string(),
                model: d
                    .get("device-make-and-model")
                    .filter(|m| **m != "Unknown")
                    .unwrap_or(&"")
                    .to_string(),
                location: d.get("device-location").unwrap_or(&"").to_string(),
            })
        })
        .collect();
    out.dedup_by(|a, b| a.uri == b.uri);
    out
}

/// Whether `uri` names a printer that speaks IPP itself (set up with
/// IPP Everywhere, no driver to choose).
pub fn driverless(uri: &str) -> bool {
    ["ipp://", "ipps://", "dnssd://"]
        .iter()
        .any(|s| uri.starts_with(s))
}

/// The URI for an address someone typed: a URI as is, else IPP
/// Everywhere's usual path on that host.
pub fn uri_for_address(address: &str) -> Option<String> {
    let a = address.trim();
    if a.is_empty() || a.chars().any(char::is_whitespace) {
        return None;
    }
    if a.contains("://") {
        return driverless(a).then(|| a.to_owned());
    }
    Some(format!("ipp://{a}/ipp/print"))
}

/// A queue name for a new printer: CUPS allows up to 127 printable
/// characters other than space, `/`, `\`, `#`, `'`, `"`, `?` and `,`;
/// unique among `taken`.
pub fn queue_name(title: &str, taken: &[String]) -> String {
    let mut base: String = title
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    while base.contains("__") {
        base = base.replace("__", "_");
    }
    let base = base.trim_matches('_');
    let base: String = if base.is_empty() {
        "Printer".into()
    } else {
        base.chars().take(100).collect()
    };
    let free = |n: &str| !taken.iter().any(|t| t.eq_ignore_ascii_case(n));
    if free(&base) {
        return base;
    }
    (2..)
        .map(|i| format!("{base}_{i}"))
        .find(|n| free(n))
        .unwrap_or(base)
}

/// Whether CUPS is installed here (its scheduler), to tell "not
/// installed" from "not running".
pub fn installed() -> bool {
    std::path::Path::new(CUPSD).exists()
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::bus::testbus;
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// A fake CUPS: printers and jobs over IPP, every request logged.
    #[derive(Default)]
    pub struct Fake {
        pub printers: Vec<Group>,
        pub default: Option<String>,
        pub jobs: Vec<Group>,
        pub attributes: Group,
        pub log: Vec<(u16, Message, usize)>,
        /// Answer Get-Printers with "not found" (no printers).
        pub none: bool,
    }

    pub fn text(name: &str, v: &str) -> (String, Vec<Value>) {
        (name.into(), vec![Value::Text(v.into())])
    }

    pub fn printer(name: &str, info: &str, state: i32, reasons: &[&str]) -> Group {
        Group {
            tag: tag::PRINTER,
            attrs: vec![
                text("printer-name", name),
                text("printer-info", info),
                text("printer-location", "Office"),
                text("printer-make-and-model", "Example LaserJet"),
                ("printer-state".into(), vec![Value::Int(state)]),
                (
                    "printer-state-reasons".into(),
                    reasons.iter().map(|r| Value::Text((*r).into())).collect(),
                ),
                ("printer-is-accepting-jobs".into(), vec![Value::Bool(true)]),
                text("device-uri", &format!("ipp://{name}.local/ipp/print")),
                ("queued-job-count".into(), vec![Value::Int(0)]),
            ],
        }
    }

    /// Serves `fake` on a local port; its address.
    pub fn serve(fake: Arc<Mutex<Fake>>) -> Server {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { continue };
                let fake = fake.clone();
                std::thread::spawn(move || {
                    let mut r = BufReader::new(s.try_clone().unwrap());
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        if r.read_line(&mut line).unwrap_or(0) == 0 {
                            return;
                        }
                        if line.trim().is_empty() {
                            break;
                        }
                        if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                            length = v.trim().parse().unwrap_or(0);
                        }
                    }
                    let mut body = vec![0; length];
                    r.read_exact(&mut body).unwrap();
                    let (req, used) = decode(&body).expect("a well-formed request");
                    let answer = answer(&mut fake.lock().unwrap(), req, body.len() - used);
                    // Chunked, as CUPS answers.
                    let mut out = b"HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
                    for chunk in answer.chunks(100) {
                        out.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
                        out.extend_from_slice(chunk);
                        out.extend_from_slice(b"\r\n");
                    }
                    out.extend_from_slice(b"0\r\n\r\n");
                    let _ = s.write_all(&out);
                });
            }
        });
        Server::Tcp(addr)
    }

    fn answer(f: &mut Fake, req: Message, document: usize) -> Vec<u8> {
        let id = req.request_id;
        let code = req.code;
        f.log.push((code, req, document));
        match code {
            op::CUPS_GET_PRINTERS if f.none => encode_response(NOT_FOUND, id, &[]),
            op::CUPS_GET_PRINTERS => encode_response(0, id, &f.printers),
            op::CUPS_GET_DEFAULT => match &f.default {
                Some(d) => encode_response(
                    0,
                    id,
                    &[Group {
                        tag: tag::PRINTER,
                        attrs: vec![text("printer-name", d)],
                    }],
                ),
                None => encode_response(NOT_FOUND, id, &[]),
            },
            op::GET_JOBS => encode_response(0, id, &f.jobs),
            op::GET_PRINTER_ATTRIBUTES => {
                encode_response(0, id, std::slice::from_ref(&f.attributes))
            }
            op::PRINT_JOB => encode_response(
                0,
                id,
                &[Group {
                    tag: tag::JOB,
                    attrs: vec![("job-id".into(), vec![Value::Int(7)])],
                }],
            ),
            _ => encode_response(0x0501, id, &[]),
        }
    }

    fn fake_with(printers: Vec<Group>, default: Option<&str>) -> (Arc<Mutex<Fake>>, Server) {
        let fake = Arc::new(Mutex::new(Fake {
            printers,
            default: default.map(str::to_owned),
            ..Fake::default()
        }));
        let server = serve(fake.clone());
        (fake, server)
    }

    #[test]
    fn requests_are_well_formed_ipp() {
        let bytes = Request::new(op::GET_JOBS)
            .uri("printer-uri", "ipp://localhost/printers/a")
            .keywords("requested-attributes", &["job-id", "job-name"])
            .encode(9);
        assert_eq!(&bytes[..8], &[2, 0, 0, 0x0a, 0, 0, 0, 9]);
        let (m, used) = decode(&bytes).unwrap();
        assert_eq!(used, bytes.len());
        assert_eq!(m.code, op::GET_JOBS);
        let g = m.groups(tag::OPERATION).next().unwrap();
        // Charset and language first, as RFC 8011 requires.
        assert_eq!(g.attrs[0].0, "attributes-charset");
        assert_eq!(g.attrs[1].0, "attributes-natural-language");
        assert_eq!(g.texts("requested-attributes"), ["job-id", "job-name"]);
    }

    #[test]
    fn responses_decode_values_languages_and_collections() {
        let mut data = encode_response(0, 1, &[printer("a", "A", 3, &["none"])]);
        // Append, before the end tag: a nameWithLanguage, then a
        // collection with a member.
        data.pop();
        let mut extra = vec![tag::NAME_LANG, 0, 4];
        extra.extend_from_slice(b"user");
        let mut v = vec![0, 2];
        v.extend_from_slice(b"en");
        v.extend_from_slice(&[0, 3]);
        v.extend_from_slice(b"ann");
        extra.extend_from_slice(&(v.len() as u16).to_be_bytes());
        extra.extend_from_slice(&v);
        for (t, n, val) in [
            (tag::BEG_COLLECTION, "media-col", &b""[..]),
            (tag::MEMBER_NAME, "", b"media-size"),
            (tag::END_COLLECTION, "", b""),
        ] {
            extra.push(t);
            extra.extend_from_slice(&(n.len() as u16).to_be_bytes());
            extra.extend_from_slice(n.as_bytes());
            extra.extend_from_slice(&(val.len() as u16).to_be_bytes());
            extra.extend_from_slice(val);
        }
        extra.extend_from_slice(&[tag::INTEGER, 0, 5]);
        extra.extend_from_slice(b"after");
        extra.extend_from_slice(&[0, 4, 0, 0, 0, 42]);
        data.extend_from_slice(&extra);
        data.push(tag::END);
        let (m, _) = decode(&data).unwrap();
        let g = m.groups(tag::PRINTER).next().unwrap();
        assert_eq!(g.text("printer-name"), "a");
        assert_eq!(g.int("printer-state"), Some(3));
        assert_eq!(g.bool("printer-is-accepting-jobs"), Some(true));
        assert_eq!(g.text("user"), "ann");
        assert_eq!(g.get("media-col"), Some(&[Value::Other][..]));
        assert_eq!(g.int("after"), Some(42));
        // Cut short anywhere: no message, no panic.
        for n in 0..data.len() - 1 {
            assert!(decode(&data[..n]).is_none(), "{n}");
        }
    }

    #[test]
    fn reads_printers_and_the_default() {
        let (fake, server) = fake_with(
            vec![
                printer("office", "Office Printer", 3, &["none"]),
                printer("hall", "", 5, &["media-empty-error"]),
            ],
            Some("office"),
        );
        let s = read(&server).unwrap();
        assert_eq!(s.default.as_deref(), Some("office"));
        // Sorted by what people see; a missing info shows the queue name.
        assert_eq!(
            s.printers.iter().map(Printer::title).collect::<Vec<_>>(),
            ["hall", "Office Printer"]
        );
        assert_eq!(s.printers[0].state, PrinterState::Stopped);
        assert_eq!(s.printers[0].status(), "Out of paper");
        assert_eq!(s.printers[1].status(), "Ready");
        assert!(s.printers[1].reasons.is_empty(), "none is not a reason");
        let log = &fake.lock().unwrap().log;
        let first = &log[0].1;
        let g = first.groups(tag::OPERATION).next().unwrap();
        assert!(g
            .texts("requested-attributes")
            .contains(&"printer-state-reasons".to_owned()));
        assert!(!g.text("requesting-user-name").is_empty());
    }

    #[test]
    fn no_printers_and_no_default_are_not_errors() {
        let (fake, server) = fake_with(Vec::new(), None);
        fake.lock().unwrap().none = true;
        assert_eq!(read(&server).unwrap(), State::default());
    }

    #[test]
    fn cups_not_running_is_missing() {
        // Port 1 (tcpmux) is privileged and unused: the connection is
        // refused. (A port freed by the test could be taken by another
        // test's fake server meanwhile.)
        assert_eq!(
            read(&Server::Tcp("127.0.0.1:1".into())),
            Err(bus::Error::Missing)
        );
        assert_eq!(
            read(&Server::Socket("/nonexistent/cups.sock".into())),
            Err(bus::Error::Missing)
        );
    }

    #[test]
    fn reads_jobs() {
        let (fake, server) = fake_with(Vec::new(), None);
        fake.lock().unwrap().jobs = vec![
            Group {
                tag: tag::JOB,
                attrs: vec![
                    ("job-id".into(), vec![Value::Int(3)]),
                    text("job-name", "report.pdf"),
                    text("job-originating-user-name", "ann"),
                    ("job-state".into(), vec![Value::Int(5)]),
                ],
            },
            Group {
                tag: tag::JOB,
                attrs: vec![
                    ("job-id".into(), vec![Value::Int(4)]),
                    text("job-name", "notes"),
                    ("job-state".into(), vec![Value::Int(4)]),
                ],
            },
        ];
        let jobs = jobs(&server, "office queue").unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(
            (jobs[0].id, jobs[0].state, jobs[0].status()),
            (3, JobState::Printing, "Printing")
        );
        assert_eq!(jobs[1].state, JobState::Held);
        let (_, req, _) = &fake.lock().unwrap().log[0];
        let g = req.groups(tag::OPERATION).next().unwrap();
        assert_eq!(
            g.text("printer-uri"),
            "ipp://localhost/printers/office%20queue"
        );
        assert_eq!(g.text("which-jobs"), "not-completed");
    }

    #[test]
    fn reads_options_with_more_than_one_choice() {
        let (fake, server) = fake_with(Vec::new(), None);
        fake.lock().unwrap().attributes = Group {
            tag: tag::PRINTER,
            attrs: vec![
                (
                    "media-supported".into(),
                    vec![
                        Value::Text("iso_a4_210x297mm".into()),
                        Value::Text("na_letter_8.5x11in".into()),
                    ],
                ),
                text("media-default", "iso_a4_210x297mm"),
                (
                    "sides-supported".into(),
                    vec![Value::Text("one-sided".into())],
                ),
                (
                    "print-quality-supported".into(),
                    vec![Value::Int(3), Value::Int(4), Value::Int(5)],
                ),
                ("print-quality-default".into(), vec![Value::Int(4)]),
            ],
        };
        let o = options(&server, "office").unwrap();
        assert_eq!(
            o.iter().map(|c| c.kind).collect::<Vec<_>>(),
            [OptionKind::PaperSize, OptionKind::Quality]
        );
        assert_eq!(o[0].default.as_deref(), Some("iso_a4_210x297mm"));
        assert_eq!(o[1].values, ["3", "4", "5"]);
        assert_eq!(choice_label(OptionKind::Quality, "5"), "High");
    }

    #[test]
    fn prints_the_test_page() {
        let (fake, server) = fake_with(Vec::new(), None);
        assert_eq!(print_test_page(&server, "office", b"%!PS test").unwrap(), 7);
        let (code, req, document) = &fake.lock().unwrap().log[0];
        assert_eq!(*code, op::PRINT_JOB);
        assert_eq!(*document, 9, "the page follows the attributes");
        let g = req.groups(tag::OPERATION).next().unwrap();
        assert_eq!(g.text("document-format"), "application/octet-stream");
        assert_eq!(g.text("job-name"), "Test Page");
    }

    #[test]
    fn labels() {
        assert_eq!(media_label("iso_a4_210x297mm"), "A4");
        assert_eq!(media_label("na_letter_8.5x11in"), "Letter");
        assert_eq!(media_label("na_legal_8.5x14in"), "Legal");
        assert_eq!(media_label("custom"), "custom");
        assert_eq!(
            choice_label(OptionKind::TwoSided, "two-sided-long-edge"),
            "Long Edge (Standard)"
        );
        assert_eq!(reason_label("toner-low-warning"), Some("Low on toner"));
        assert_eq!(
            reason_label("media-empty-report"),
            None,
            "reports are not shown"
        );
        assert_eq!(reason_label("something-else"), None);
    }

    #[test]
    fn addresses_and_queue_names() {
        assert_eq!(
            uri_for_address(" 192.168.1.20 ").as_deref(),
            Some("ipp://192.168.1.20/ipp/print")
        );
        assert_eq!(
            uri_for_address("printer.local:631").as_deref(),
            Some("ipp://printer.local:631/ipp/print")
        );
        assert_eq!(
            uri_for_address("ipps://p.example/ipp/print").as_deref(),
            Some("ipps://p.example/ipp/print")
        );
        assert_eq!(
            uri_for_address("usb://Example/Printer"),
            None,
            "needs a driver"
        );
        assert_eq!(uri_for_address("two words"), None);
        assert_eq!(uri_for_address(""), None);
        let taken = vec!["Office_Printer".to_owned(), "office_printer_2".to_owned()];
        assert_eq!(queue_name("Office Printer", &taken), "Office_Printer_3");
        assert_eq!(
            queue_name("HP LaserJet  M110w (Hall)", &[]),
            "HP_LaserJet_M110w_Hall"
        );
        assert_eq!(queue_name("///", &[]), "Printer");
        assert!(queue_name(&"x".repeat(300), &[]).len() <= 127);
    }

    #[test]
    fn devices_are_grouped_and_need_no_driver() {
        let found: std::collections::HashMap<String, String> = [
            ("device-uri:1", "dnssd://Office%20Printer._ipp._tcp.local/"),
            ("device-info:1", "Office Printer"),
            ("device-make-and-model:1", "Example LaserJet"),
            ("device-uri:2", "usb://Example/Old?serial=1"),
            ("device-info:2", "Old USB"),
            ("device-uri:3", "ipp://10.0.0.5/ipp/print"),
            ("device-make-and-model:3", "Unknown"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
        let d = parse_devices(&found);
        assert_eq!(
            d.iter().map(|d| d.uri.as_str()).collect::<Vec<_>>(),
            [
                "dnssd://Office%20Printer._ipp._tcp.local/",
                "ipp://10.0.0.5/ipp/print"
            ]
        );
        assert_eq!(d[0].title(), "Office Printer");
        assert_eq!(
            d[1].title(),
            "ipp://10.0.0.5/ipp/print",
            "no info or model: its address"
        );
    }

    /// Against a real (private, scratch) CUPS with at least one printer:
    /// `HELIX_TEST_CUPS=127.0.0.1:PORT cargo test … against_a_real_cups
    /// -- --ignored`. Never run it against the system's own CUPS: it
    /// prints a test page.
    #[test]
    #[ignore]
    fn against_a_real_cups() {
        let server = Server::Tcp(
            std::env::var("HELIX_TEST_CUPS")
                .expect("HELIX_TEST_CUPS: host:port of a scratch cupsd"),
        );
        let s = read(&server).unwrap();
        eprintln!("{s:#?}");
        let p = s.printers.first().expect("a printer");
        assert!(!p.model.is_empty() && !p.device_uri.is_empty(), "{p:?}");
        let o = options(&server, &p.name).unwrap();
        eprintln!("{o:#?}");
        assert!(
            o.iter().any(|c| c.kind == OptionKind::PaperSize),
            "CUPS reports media-supported"
        );
        let page = std::fs::read(TEST_PAGE).unwrap();
        let id = print_test_page(&server, &p.name, &page).unwrap();
        assert!(id > 0);
        let j = jobs(&server, &p.name).unwrap();
        eprintln!("{j:#?}");
    }

    /// A polkit that allows everything, for [`through_the_real_helper`].
    struct AllowAll;

    /// polkit's answer: authorized, a challenge, details.
    #[derive(serde::Serialize, zbus::zvariant::Type)]
    struct Authorization(bool, bool, std::collections::HashMap<String, String>);

    #[zbus::interface(name = "org.freedesktop.PolicyKit1.Authority")]
    impl AllowAll {
        fn check_authorization(
            &self,
            _subject: (
                String,
                std::collections::HashMap<String, zbus::zvariant::OwnedValue>,
            ),
            _action: String,
            _details: std::collections::HashMap<String, String>,
            _flags: u32,
            _cancel: String,
        ) -> (Authorization,) {
            (Authorization(true, false, Default::default()),)
        }
    }

    /// The real cups-pk-helper on a private bus (`HELIX_TEST_BUS`; start
    /// `/usr/libexec/cups-pk-helper-mechanism` on it with
    /// `DBUS_SYSTEM_BUS_ADDRESS` set to it and `CUPS_SERVER` to a scratch
    /// cupsd, `HELIX_TEST_CUPS`, with a printer), polkit faked to allow
    /// everything: the changes reach CUPS. Never against the system's
    /// own bus or CUPS.
    #[test]
    #[ignore]
    fn through_the_real_helper() {
        let bus = std::env::var("HELIX_TEST_BUS").expect("HELIX_TEST_BUS: a private bus");
        let server = Server::Tcp(std::env::var("HELIX_TEST_CUPS").expect("HELIX_TEST_CUPS"));
        let polkit = testbus::service_at(&bus);
        polkit
            .object_server()
            .at("/org/freedesktop/PolicyKit1/Authority", AllowAll)
            .unwrap();
        polkit.request_name("org.freedesktop.PolicyKit1").unwrap();
        let c = testbus::connect_at(&bus);
        let p = read(&server).unwrap().printers[0].name.clone();
        set_default(&c, &p).unwrap();
        assert_eq!(read(&server).unwrap().default.as_deref(), Some(p.as_str()));
        set_enabled(&c, &p, false).unwrap();
        assert_eq!(
            read(&server).unwrap().printers[0].state,
            PrinterState::Stopped
        );
        set_enabled(&c, &p, true).unwrap();
        assert_ne!(
            read(&server).unwrap().printers[0].state,
            PrinterState::Stopped
        );
        set_option_default(&c, &p, "media", "iso_a4_210x297mm").unwrap();
        let media = options(&server, &p)
            .unwrap()
            .into_iter()
            .find(|c| c.kind == OptionKind::PaperSize)
            .unwrap();
        assert_eq!(media.default.as_deref(), Some("iso_a4_210x297mm"));
        let uri = read(&server).unwrap().printers[0].device_uri.clone();
        add(&c, "Second", &uri, "Second Printer", "Here").unwrap();
        let s = read(&server).unwrap();
        let second = s
            .printers
            .iter()
            .find(|p| p.name == "Second")
            .expect("added");
        assert_eq!(
            (second.info.as_str(), second.location.as_str()),
            ("Second Printer", "Here")
        );
        remove(&c, "Second").unwrap();
        assert!(read(&server)
            .unwrap()
            .printers
            .iter()
            .all(|p| p.name != "Second"));
        eprintln!("devices: {:#?}", devices(&c, 3));
    }

    /// What the fake helper was asked to do.
    pub type Log = Arc<Mutex<Vec<String>>>;

    pub struct FakeHelper {
        pub log: Log,
        /// Every call fails with this, when set.
        pub error: String,
    }

    #[zbus::interface(name = "org.opensuse.CupsPkHelper.Mechanism")]
    impl FakeHelper {
        fn printer_set_default(&self, name: String) -> String {
            self.log.lock().unwrap().push(format!("SetDefault {name}"));
            self.error.clone()
        }
        fn printer_set_enabled(&self, name: String, enabled: bool) -> String {
            self.log
                .lock()
                .unwrap()
                .push(format!("SetEnabled {name} {enabled}"));
            self.error.clone()
        }
        fn printer_delete(&self, name: String) -> String {
            self.log.lock().unwrap().push(format!("Delete {name}"));
            self.error.clone()
        }
        fn printer_add(
            &self,
            name: String,
            uri: String,
            ppd: String,
            info: String,
            location: String,
        ) -> String {
            self.log
                .lock()
                .unwrap()
                .push(format!("Add {name} {uri} {ppd} {info} {location}"));
            self.error.clone()
        }
        fn printer_add_option_default(
            &self,
            name: String,
            option: String,
            values: Vec<String>,
        ) -> String {
            self.log
                .lock()
                .unwrap()
                .push(format!("OptionDefault {name} {option} {values:?}"));
            self.error.clone()
        }
        fn job_cancel_purge(&self, id: i32, purge: bool) -> String {
            self.log
                .lock()
                .unwrap()
                .push(format!("Cancel {id} {purge}"));
            self.error.clone()
        }
        fn job_set_hold_until(&self, id: i32, until: String) -> String {
            self.log.lock().unwrap().push(format!("Hold {id} {until}"));
            self.error.clone()
        }
        fn devices_get(
            &self,
            timeout: i32,
            limit: i32,
            include: Vec<String>,
            exclude: Vec<String>,
        ) -> (String, std::collections::HashMap<String, String>) {
            self.log.lock().unwrap().push(format!(
                "DevicesGet {timeout} {limit} {include:?} {exclude:?}"
            ));
            let found = [
                ("device-uri:1", "ipp://10.0.0.5/ipp/print"),
                ("device-info:1", "Hall Printer"),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .collect();
            (self.error.clone(), found)
        }
    }

    /// Serves a fake cups-pk-helper on `conn`.
    pub fn serve_helper(conn: &Connection, log: &Log, error: &str) {
        conn.object_server()
            .at(
                "/",
                FakeHelper {
                    log: log.clone(),
                    error: error.into(),
                },
            )
            .unwrap();
        conn.request_name("org.opensuse.CupsPkHelper.Mechanism")
            .unwrap();
    }

    #[test]
    fn changes_go_through_cups_pk_helper() {
        let Some(bus) = testbus::private_bus() else {
            return;
        };
        let service = testbus::service(&bus);
        let log = Log::default();
        serve_helper(&service, &log, "");
        let c = testbus::connect(&bus);
        set_default(&c, "office").unwrap();
        set_enabled(&c, "office", false).unwrap();
        add(
            &c,
            "Hall",
            "ipp://10.0.0.5/ipp/print",
            "Hall Printer",
            "Hall",
        )
        .unwrap();
        set_option_default(&c, "office", "sides", "two-sided-long-edge").unwrap();
        cancel_job(&c, 3).unwrap();
        hold_job(&c, 4, true).unwrap();
        hold_job(&c, 4, false).unwrap();
        remove(&c, "office").unwrap();
        let d = devices(&c, 5).unwrap();
        assert_eq!(d[0].title(), "Hall Printer");
        assert_eq!(
            *log.lock().unwrap(),
            [
                "SetDefault office",
                "SetEnabled office false",
                "Add Hall ipp://10.0.0.5/ipp/print everywhere Hall Printer Hall",
                "OptionDefault office sides [\"two-sided-long-edge\"]",
                "Cancel 3 false",
                "Hold 4 indefinite",
                "Hold 4 no-hold",
                "Delete office",
                "DevicesGet 5 0 [] []",
            ]
        );
    }

    #[test]
    fn helper_errors_and_its_absence() {
        let Some(bus) = testbus::private_bus() else {
            return;
        };
        let c = testbus::connect(&bus);
        assert_eq!(set_default(&c, "office"), Err(bus::Error::Missing));
        let service = testbus::service(&bus);
        serve_helper(
            &service,
            &Log::default(),
            "The printer or class does not exist.",
        );
        assert_eq!(
            set_default(&c, "office"),
            Err(bus::Error::Other(
                "The printer or class does not exist.".into()
            ))
        );
    }
}
