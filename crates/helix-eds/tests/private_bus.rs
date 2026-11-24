//! End-to-end test against real EDS daemons on a PRIVATE session bus.
//!
//! Skipped unless `HELIX_EDS_PRIVATE_BUS=1`. Run it through a wrapper that
//! starts `dbus-run-session` with scratch XDG dirs and launches
//! `evolution-source-registry`, `evolution-addressbook-factory` and
//! `evolution-calendar-factory` inside it. It refuses to run on what looks
//! like the user's real session bus.

use chrono::{Duration, NaiveDate, Utc};
use helix_eds::ical::{expand, Event, Time, Zones};
use helix_eds::vcard::{Contact, Photo, StructuredName, Typed};
use helix_eds::{sexp, source, Eds, ModType, SourceKind, ViewEvent};
use std::sync::mpsc;

fn private_eds() -> Option<Eds> {
    if std::env::var("HELIX_EDS_PRIVATE_BUS").ok().as_deref() != Some("1") {
        eprintln!("skipping: HELIX_EDS_PRIVATE_BUS not set");
        return None;
    }
    let addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").expect("bus address");
    assert!(
        !addr.contains("/run/user/"),
        "refusing to run on the real session bus: {addr}"
    );
    Some(Eds::with_address(&addr).expect("connect"))
}

fn wait_for<T>(rx: &mpsc::Receiver<ViewEvent>, mut pred: impl FnMut(&ViewEvent) -> Option<T>) -> T {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        let ev = rx
            .recv_timeout(left)
            .expect("timed out waiting for view event");
        if let Some(v) = pred(&ev) {
            return v;
        }
    }
}

fn wait_source(eds: &Eds, uid: &str, present: bool) -> Option<helix_eds::Source> {
    for _ in 0..100 {
        let found = eds.sources().unwrap().into_iter().find(|s| s.uid == uid);
        if found.is_some() == present {
            return found;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    panic!("source {uid} presence != {present}");
}

#[test]
fn contacts_round_trip() {
    let Some(eds) = private_eds() else { return };
    let sources = eds.sources().unwrap();
    assert!(sources
        .iter()
        .any(|s| s.uid == "system-address-book" && s.is(SourceKind::AddressBook)));

    // A new local address book.
    let uid = source::new_uid();
    eds.create_source(&uid, &source::new_local_address_book("Helix Test"))
        .unwrap();
    let src = wait_source(&eds, &uid, true).unwrap();
    assert_eq!(src.display_name(), "Helix Test");
    assert!(src.removable && src.writable);

    let book = eds.open_book(&uid).unwrap();
    assert!(book.writable);
    let (tx, rx) = mpsc::channel();
    let _view = book
        .view(&sexp::all_contacts(), move |ev| {
            let _ = tx.send(ev);
        })
        .unwrap();
    wait_for(&rx, |e| {
        matches!(e, ViewEvent::Complete(None)).then_some(())
    });

    let mut c = Contact {
        name: StructuredName::guess_from("Grace Hopper"),
        ..Default::default()
    };
    c.emails.push(Typed::new(&["WORK"], "grace@example.org"));
    c.note = "Line one\nline; two".into();
    let uids = book.create(&[c]).unwrap();
    assert_eq!(uids.len(), 1);
    let added = wait_for(&rx, |e| match e {
        ViewEvent::Added(v) => Some(v.clone()),
        _ => None,
    });
    let got = Contact::parse(&added[0]).unwrap();
    assert_eq!(got.uid, uids[0]);
    assert_eq!(got.display_name(), "Grace Hopper");
    assert_eq!(got.note, "Line one\nline; two");
    assert_eq!(got.file_as, "Hopper, Grace");

    let mut edited = book.contact(&uids[0]).unwrap();
    edited.phones.push(Typed::new(&["CELL"], "+1 555 0100"));
    book.modify(&[edited]).unwrap();
    let modified = wait_for(&rx, |e| match e {
        ViewEvent::Modified(v) => Some(v.clone()),
        _ => None,
    });
    assert_eq!(
        Contact::parse(&modified[0]).unwrap().phones[0].value,
        "+1 555 0100"
    );
    assert_eq!(
        book.contacts(&sexp::contacts_containing("hopper"))
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        book.contacts(&sexp::contacts_with_email("grace@example.org"))
            .unwrap()
            .len(),
        1
    );

    book.remove(&uids).unwrap();
    let removed = wait_for(&rx, |e| match e {
        ViewEvent::Removed(v) => Some(v.clone()),
        _ => None,
    });
    assert_eq!(removed, uids);
    drop(_view);
    drop(book);

    eds.remove_source(&src).unwrap();
    wait_source(&eds, &uid, false);
}

/// What the local (file) backend stores for PHOTO: inline photos become
/// `file://` URIs in the book's own `photos` directory, and an external
/// `file://` URI is kept as it is (so readers must not trust it).
#[test]
fn photo_uris_round_trip() {
    let Some(eds) = private_eds() else { return };
    // The built-in book: creating and removing sources is what sometimes
    // crashes evolution-source-registry 3.56 here.
    let book = eds.open_book("system-address-book").unwrap();

    let external = "file:///nonexistent/helix-eds-test/secret.png";
    let linked = Contact {
        name: StructuredName::guess_from("Linked Photo"),
        photo: Some(Photo::Uri(external.into())),
        ..Default::default()
    };
    let inline = Contact {
        name: StructuredName::guess_from("Inline Photo"),
        photo: Some(Photo::Inline {
            mime: Some("image/png".into()),
            data: b"\x89PNG\r\n\x1a\nnot really".to_vec(),
        }),
        ..Default::default()
    };
    let uids = book.create(&[linked, inline]).unwrap();

    let got = book.contact(&uids[0]).unwrap();
    assert_eq!(got.photo, Some(Photo::Uri(external.into())));

    let got = book.contact(&uids[1]).unwrap();
    let Some(Photo::Uri(stored)) = &got.photo else {
        panic!("inline photo not stored as a file: {:?}", got.photo);
    };
    let data = std::env::var("XDG_DATA_HOME").unwrap();
    let dir = format!("file://{data}/evolution/addressbook/system/photos/");
    assert!(stored.starts_with(&dir), "{stored} not under {dir}");
    assert!(got.photo_bytes().is_some(), "own photo not readable");

    book.remove(&uids).unwrap();
}

#[test]
fn calendar_round_trip() {
    let Some(eds) = private_eds() else { return };
    let uid = source::new_uid();
    eds.create_source(&uid, &source::new_local_calendar("Helix Cal", "#62a0ea"))
        .unwrap();
    let mut src = wait_source(&eds, &uid, true).unwrap();
    assert_eq!(src.color(), Some("#62a0ea"));
    assert!(src.selected());

    // Toggle visibility through Source.Writable.
    src.set_selected(false);
    eds.write_source(&src).unwrap();
    let mut ok = false;
    for _ in 0..50 {
        if !eds
            .sources()
            .unwrap()
            .into_iter()
            .find(|s| s.uid == uid)
            .unwrap()
            .selected()
        {
            ok = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(ok, "Selected=false not persisted");

    let cal = eds.open_calendar(&uid).unwrap();
    assert!(cal.writable);
    let zones = Zones::new(chrono_tz::Europe::Berlin);
    let start = NaiveDate::from_ymd_opt(2026, 11, 2)
        .unwrap()
        .and_hms_opt(9, 0, 0)
        .unwrap();
    let mut ev = Event::new(
        Time::Zoned(start, "Europe/Berlin".into()),
        Time::Zoned(start + Duration::hours(1), "Europe/Berlin".into()),
    );
    ev.summary = "Daily sync".into();
    ev.rrule = helix_eds::rrule::RRule::parse("FREQ=DAILY;COUNT=5");
    ev.alarms
        .push(helix_eds::ical::Alarm::before(Duration::minutes(10)));

    let range_from = Utc::now() - Duration::days(3650);
    let range_to = Utc::now() + Duration::days(3650);
    let (tx, rx) = mpsc::channel();
    let _view = cal
        .view(&sexp::occur_in_range(range_from, range_to), move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
    wait_for(&rx, |e| {
        matches!(e, ViewEvent::Complete(None)).then_some(())
    });

    let created = cal.create(&[ev.to_ics(Utc::now())]).unwrap();
    assert_eq!(created, vec![ev.uid.clone()]);
    wait_for(&rx, |e| matches!(e, ViewEvent::Added(_)).then_some(()));

    let events = cal
        .events(&sexp::occur_in_range(range_from, range_to))
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].summary, "Daily sync");
    assert_eq!(
        events[0].alarms[0].before_start,
        Some(Duration::minutes(10))
    );
    let from = NaiveDate::from_ymd_opt(2026, 11, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();
    let to = from + Duration::days(30);
    let inst = expand(&events, from, to, &zones);
    assert_eq!(inst.len(), 5);

    // Move the third occurrence ("this" only).
    let third = &inst[2];
    let mut detached = events[0].clone();
    detached.rrule = None;
    detached.recurrence_id = Some(Time::Zoned(third.start, "Europe/Berlin".into()));
    detached.start = Time::Zoned(third.start + Duration::hours(3), "Europe/Berlin".into());
    detached.end = Time::Zoned(third.start + Duration::hours(4), "Europe/Berlin".into());
    detached.summary = "Moved sync".into();
    cal.modify(&[detached.to_ics(Utc::now())], ModType::This)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let events = cal.events(&sexp::all_events()).unwrap();
    let inst = expand(&events, from, to, &zones);
    assert_eq!(inst.len(), 5, "{events:#?}");
    assert_eq!(inst[2].start, third.start + Duration::hours(3));
    assert_eq!(events[inst[2].event].summary, "Moved sync");

    // Delete the fourth occurrence only.
    let fourth = inst[3].clone();
    cal.remove(&[(fourth.uid.clone(), fourth.rid.clone())], ModType::This)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let events = cal.events(&sexp::all_events()).unwrap();
    let inst = expand(&events, from, to, &zones);
    assert_eq!(inst.len(), 4, "{events:#?}");
    assert!(inst.iter().all(|i| i.start != fourth.start));

    // Search.
    assert!(!cal
        .objects(&sexp::events_containing("sync"))
        .unwrap()
        .is_empty());

    // Delete the series.
    cal.remove(&[(ev.uid.clone(), String::new())], ModType::All)
        .unwrap();
    wait_for(&rx, |e| matches!(e, ViewEvent::Removed(_)).then_some(()));
    assert!(cal.events(&sexp::all_events()).unwrap().is_empty());
    drop(_view);
    drop(cal);
    eds.remove_source(&src).unwrap();
    wait_source(&eds, &uid, false);
}

/// Serves `body` as `text/calendar` on 127.0.0.1 (any path) until the
/// process exits; returns the port and a request counter.
fn serve_ics(body: &'static str) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let h = hits.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut buf = [0u8; 4096];
            let _ = s.read(&mut buf);
            h.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/calendar; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes());
        }
    });
    (port, hits)
}

#[test]
fn webcal_subscription() {
    let Some(eds) = private_eds() else { return };
    const ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Helix//Test//EN\r\n\
BEGIN:VEVENT\r\nUID:webcal-test-1@helix\r\nDTSTAMP:20261101T000000Z\r\n\
DTSTART;VALUE=DATE:20261225\r\nDTEND;VALUE=DATE:20261226\r\nSUMMARY:Christmas Day\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";
    let (port, hits) = serve_ics(ICS);
    let url = source::parse_webcal_url(&format!("http://127.0.0.1:{port}/holidays.ics")).unwrap();

    let uid = source::new_uid();
    eds.create_source(
        &uid,
        &source::new_webcal_calendar("Holidays", "#e66100", &url),
    )
    .unwrap();
    let src = wait_source(&eds, &uid, true).unwrap();
    assert_eq!(src.backend_name(SourceKind::Calendar), Some("webcal"));
    assert_eq!(src.parent(), Some("webcal-stub"));
    assert_eq!(
        source::webcal_url(&src).unwrap(),
        format!("http://127.0.0.1:{port}/holidays.ics")
    );
    assert!(src.removable);

    let cal = eds.open_calendar(&uid).unwrap();
    assert!(!cal.writable, "webcal calendars are read-only");
    let (tx, rx) = mpsc::channel();
    let _view = cal
        .view(&sexp::all_events(), move |e| {
            let _ = tx.send(e);
        })
        .unwrap();
    // EDS downloads the feed on open; the event arrives through the view.
    let added = wait_for(&rx, |e| match e {
        ViewEvent::Added(v) => Some(v.clone()),
        _ => None,
    });
    assert!(
        added.iter().any(|o| o.contains("Christmas Day")),
        "{added:?}"
    );
    assert!(hits.load(std::sync::atomic::Ordering::SeqCst) >= 1);
    let events = cal.events(&sexp::all_events()).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].summary, "Christmas Day");

    drop(_view);
    drop(cal);
    eds.remove_source(&src).unwrap();
    wait_source(&eds, &uid, false);
}

/// Removing a source right after closing its book or calendar must not
/// crash `evolution-source-registry` (helixdesktop/apps#205): the Calendar
/// app closes its client and then removes the calendar. `remove_source`
/// waits for the closed backend to finish with the source first.
#[test]
fn remove_right_after_close() {
    let Some(eds) = private_eds() else { return };
    for round in 0..6 {
        let uid = source::new_uid();
        let book = round % 2 == 0;
        let data = if book {
            source::new_local_address_book("Helix Gone")
        } else {
            source::new_local_calendar("Helix Gone", "#62a0ea")
        };
        eds.create_source(&uid, &data).unwrap();
        let src = wait_source(&eds, &uid, true).unwrap();
        // The factory can learn of a new source a little after the
        // registry lists it ("No such source for UID").
        let mut tries = 0;
        loop {
            let opened = if book {
                eds.open_book(&uid).map(drop)
            } else {
                eds.open_calendar(&uid).map(drop)
            };
            match opened {
                Ok(()) => break,
                Err(e) if tries < 50 && e.to_string().contains("No such source") => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
                Err(e) => panic!("open {uid}: {e}"),
            }
        }
        // Closed (dropped): remove it at once, as the Calendar app does.
        eds.remove_source(&src).unwrap();
        wait_source(&eds, &uid, false);
        // Still serving: a crashed registry gives NoReply or "not provided".
        eds.sources()
            .unwrap_or_else(|e| panic!("registry gone after round {round}: {e}"));
    }
}
