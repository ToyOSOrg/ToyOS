//! The desktop.
//!
//! Four layers, and the split is the point. `toyos_desktop` decides — where a
//! window goes, what changed since the last frame, what the pointer hit, who
//! has the keyboard, what a key means, what is visible in a damaged region —
//! and every one of those is a pure function with host tests that run in
//! milliseconds. [`session`] does the rest: it claims the devices, drains the
//! client connections without ever blocking on one, allocates and grants the
//! shared
//! buffers, and hands finished rectangles to the panel. [`render`] turns a
//! compose plan into pixels, and [`client`] is the framing and the protocol's
//! failure vocabulary.
//!
//! What is left in this file is the policy the other three read: the numbers
//! that are decisions rather than derivations.

#![forbid(unsafe_code)]

mod client;
mod display;
mod output;
mod render;
mod session;
mod signout;
mod stats;

use std::time::Duration;

use client::MAX_PENDING_CONNS;
use toyos::poller::Poller;
use toyos_manifest::package;

pub const DOUBLE_CLICK_TIME: Duration = Duration::from_millis(400);
pub const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667); // ~60fps

/// How long one pass may spend draining clients before it must composite.
///
/// Without it a client that never stops sending keeps its handle ready forever
/// and the loop never reaches the composite — a freeze with a different shape and
/// the same result. The drain's promise is "everything pending", and this is
/// the clause that makes it "or one frame's worth, whichever is sooner".
pub const DRAIN_BUDGET: Duration = FRAME_INTERVAL;

/// Handles the compositor watches that are not windows: keyboard, mouse, the
/// two listeners, `compositor` and `prompt`, the display's claim, the desktop
/// session's control connection, and the pipe its opens come back on.
pub const FIXED_POLL_HANDLES: u32 = 7;

/// The `input` port's listener and each of its sources.
pub const INPUT_POLL_HANDLES: u32 = 1 + toyos_input::wire::CONNECTIONS as u32;

/// Hard ceiling on live client windows, from the poller rather than from memory.
///
/// Every window's handle is registered in the same batch as every other
/// handle the compositor watches, counted below, and [`Poller::MAX_HANDLES`]
/// is the widest set one poller can carry. Unlike the memory budget this does not move when the
/// mode or the scale does, which is why the poller is sized from it: a mode
/// set on the display port can make windows cheaper mid-run, and a poller sized
/// for the old screen would then be too small. At any resolution this machine
/// can actually scan out, the memory budget is far below this and is what
/// binds.
pub const MAX_WINDOW_SLOTS: u32 = Poller::MAX_HANDLES
    - FIXED_POLL_HANDLES
    - MAX_PENDING_CONNS
    - PROMPT_POLL_HANDLES
    - CLIPBOARD_POLL_HANDLES
    - DISPLAY_PORT_POLL_HANDLES
    - DISPLAY_OUTPUT_POLL_HANDLES
    - INPUT_POLL_HANDLES
    - VOLUME_POLL_HANDLES;

/// The prompt's own: its connection waiting for its first frame, and its
/// window, which no client's ceiling counts (`toyos_desktop::pending_admits`,
/// `toyos_desktop::create_verdict`).
pub const PROMPT_POLL_HANDLES: u32 = 2;

/// The clipboard's own: its port's acceptor, and its one connection, waiting
/// for its first frame or watching (`toyos_desktop::clipboard`).
pub const CLIPBOARD_POLL_HANDLES: u32 = 2;

/// The display port's own: its acceptor, and the one request waiting for its
/// first frame (`toyos_desktop::pending_admits`).
pub const DISPLAY_PORT_POLL_HANDLES: u32 = 2;

/// The `display-output` port's own: its acceptor, and its one driver.
pub const DISPLAY_OUTPUT_POLL_HANDLES: u32 = 2;

/// soundserver's `volume` port: the one connection the volume keys' requests go on.
pub const VOLUME_POLL_HANDLES: u32 = 1;

/// What the launcher offers before `/apps` is read: the label it shows and the
/// program it starts.
pub const BUILT_IN_APPS: &[(&str, &str)] =
    &[("Terminal", "/system/bin/terminal"), ("Files", "/system/bin/files")];

/// The launcher's list: this image's own programs, then every installed
/// package under `/apps`, by what its `manifest.toml` says.
///
/// **Read on every open rather than once**: a package is a directory and
/// registers with nothing, so the moment it is drawn is the only moment the
/// list can be current. `package::listed` bounds it.
pub fn launcher_apps() -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> =
        BUILT_IN_APPS.iter().map(|(l, p)| ((*l).to_string(), (*p).to_string())).collect();
    let listing: Vec<(String, String)> = match std::fs::read_dir(package::DIR) {
        Ok(entries) => entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_str()?.to_string();
                let text = std::fs::read_to_string(package::Package::path(&name)).ok()?;
                Some((name, text))
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    out.extend(package::listed(&listing).into_iter().map(|p| (p.name, p.program)));
    out
}

fn main() {
    let mut session = session::Session::start();
    loop {
        session.pass();
    }
}
