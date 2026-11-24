//! Every fake service of the tests at once, for looking at the panels
//! in a scratch session (never on the user's buses):
//!
//! ```sh
//! dbus-run-session -- sh -c 'HELIX_FAKES_BUS=$DBUS_SESSION_BUS_ADDRESS \
//!     target/debug/deps/helix_settings_app-… --ignored --exact fakes::serve_fakes & …'
//! ```
//!
//! and run Settings on the same bus, with `DBUS_SYSTEM_BUS_ADDRESS` set to
//! it too.

#[test]
#[ignore]
fn serve_fakes() {
    let address = std::env::var("HELIX_FAKES_BUS").expect("HELIX_FAKES_BUS: a private bus");
    let connect = || crate::bus::testbus::service_at(&address);
    let log = crate::bluez::tests::Log::default();
    let bluez = connect();
    crate::bluez::tests::serve(&bluez, &log);
    let shell = connect();
    crate::bluez::tests::serve_send_to(&shell, &log);
    // The shell's Agent Access interface, on the same name.
    crate::agents::tests::serve_object(&shell, crate::agents::tests::sample_agents());
    let nm = connect();
    crate::nm::tests::serve(&nm, &log);
    let accounts = connect();
    crate::accounts::tests::serve(&accounts, crate::accounts::my_uid(), true, &log);
    let store = connect();
    crate::permissions::tests::serve(&store);
    // Only when asked: the compositor normally serves it.
    let displays = connect();
    if std::env::var_os("HELIX_FAKE_DISPLAYS").is_some_and(|v| !v.is_empty()) {
        crate::displays::tests::serve(&displays, &log);
    }
    // systemd's display manager, for the Login Screen panel (with the
    // greeter's files under HELIX_SETTINGS_TEST_ROOT).
    let systemd = connect();
    if let Ok(dm) = std::env::var("HELIX_FAKE_DISPLAY_MANAGER") {
        crate::greeter::tests::serve_systemd(&systemd, Some(dm.as_str()).filter(|d| !d.is_empty()));
    }
    eprintln!("fakes ready");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(5));
        let mut l = log.lock().unwrap();
        for line in l.drain(..) {
            eprintln!("fake: {line}");
        }
    }
}
