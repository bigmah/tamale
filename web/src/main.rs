//! tamale's web UI, in Dioxus. Built for WebAssembly it's the page tamale's `/web` serves
//! (see `build.sh`); built with the `desktop` feature it's a window of its own that runs
//! that same page (see `shell`).

#![allow(unused_mut)]

#[cfg(not(feature = "desktop"))]
mod app;
#[cfg(not(feature = "desktop"))]
mod conn;
#[cfg(not(feature = "desktop"))]
mod e2e;
#[cfg(not(feature = "desktop"))]
mod host;
#[cfg(not(feature = "desktop"))]
mod html;
#[cfg(not(feature = "desktop"))]
mod icons;
#[cfg(not(feature = "desktop"))]
mod review;
#[cfg(feature = "desktop")]
mod shell;
#[cfg(not(feature = "desktop"))]
mod ui;
#[cfg(not(feature = "desktop"))]
mod voice;
#[cfg(not(feature = "desktop"))]
mod wire;

fn main() {
    #[cfg(feature = "desktop")]
    shell::run();
    #[cfg(not(feature = "desktop"))]
    dioxus::launch(ui::Root);
}
