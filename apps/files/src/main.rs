//! Helix Files.

mod archive;
mod bookmarks;
mod cli;
mod clipformat;
mod fmt;
mod history;
mod naming;
mod ops;
mod pathbar;
mod search;
mod sorting;
mod stars;
mod ui;

fn main() -> gtk::glib::ExitCode {
    helix_app_common::i18n::init();
    ui::run()
}
