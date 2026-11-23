//! Helix Settings.

mod accounts;
mod agents;
mod bluez;
mod bus;
mod cups;
mod dbus;
mod defaults;
mod displays;
mod extensions;
#[cfg(test)]
mod fakes;
mod greeter;
mod icon_themes;
mod login_screen;
mod netconf;
mod nm;
mod panels;
mod permissions;
mod rfkill;
mod sound;
mod sysinfo;
mod ui;
mod values;
mod wallpaper;
mod xkb;

use gtk::glib;

const USAGE: &str = "Usage: helix-settings-app [PANEL]\n       helix-settings-app --list\n\nOpens Settings, optionally on PANEL or a page such as agent-access (see --list).";

fn main() -> glib::ExitCode {
    helix_app_common::i18n::init();
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        None => {}
        Some("-h" | "--help") => {
            println!("{USAGE}");
            return glib::ExitCode::SUCCESS;
        }
        Some("-l" | "--list") => {
            for p in panels::PANELS {
                let state = if p.available
                    && (p.id != panels::PanelId::LoginScreen || greeter::panel_shown_here())
                {
                    "available"
                } else {
                    "unavailable"
                };
                println!("{}\t{}\t{}", p.name, p.title, state);
            }
            for p in panels::PAGES {
                let there = p
                    .schema
                    .is_none_or(|s| helix_app_common::lookup_schema(s).is_some());
                let state = if there { "available" } else { "unavailable" };
                println!("{}\t{}\t{}", p.name, p.title, state);
            }
            return glib::ExitCode::SUCCESS;
        }
        Some(name) if args.len() == 2 && panels::lookup_target(name).is_some() => {}
        Some(name) => {
            eprintln!("helix-settings-app: unknown panel '{name}'");
            let names: Vec<&str> = panels::PANELS
                .iter()
                .map(|p| p.name)
                .chain(panels::PAGES.iter().map(|p| p.name))
                .collect();
            eprintln!("Panels: {}", names.join(", "));
            return glib::ExitCode::from(2);
        }
    }
    ui::run(args)
}
