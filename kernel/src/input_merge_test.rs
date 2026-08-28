//! Exercises the keyboard/pointer merge that QEMU cannot: only one input
//! handler per device class is ever live in a guest. A mismatch panics —
//! nothing here crossed a trust boundary.

use crate::keyboard::{self, RawKeyEvent, MOD_RELEASED, MOD_SHIFT};
use crate::mouse::{self, Motion, PointerSource};

fn next_key(what: &str) -> RawKeyEvent {
    keyboard::try_read_event().unwrap_or_else(|| panic!("input-merge: no event for {what}"))
}

fn drain() {
    while keyboard::try_read_event().is_some() {}
    while mouse::try_read_event().is_some() {}
}

pub fn run() {
    drain();

    // The kernel reports MOD_SHIFT only; toyos-keymap turns it into a capital.
    assert!(keyboard::handle_key(0xE1, true), "input-merge: Shift down queued nothing");
    assert!(keyboard::handle_key(0x04, true), "input-merge: 'a' down queued nothing");
    assert!(keyboard::handle_key(0xE1, false), "input-merge: Shift up queued nothing");
    // A repeated make (no intervening break) must queue nothing.
    assert!(!keyboard::handle_key(0x04, true), "input-merge: repeat make queued an event");

    let shift_down = next_key("Shift down");
    assert!(
        shift_down.keycode == 0xE1 && shift_down.modifiers & MOD_RELEASED == 0,
        "input-merge: {:#x} mods {:#x} is not Shift down",
        shift_down.keycode,
        shift_down.modifiers
    );
    let letter = next_key("'a' down");
    assert_eq!(
        (letter.keycode, letter.modifiers),
        (0x04, MOD_SHIFT),
        "input-merge: 'a' down should be usage 0x04 with exactly the other keyboard's Shift set"
    );
    let shift_up = next_key("Shift up");
    assert!(
        shift_up.keycode == 0xE1 && shift_up.modifiers & MOD_RELEASED != 0,
        "input-merge: {:#x} mods {:#x} is not Shift up",
        shift_up.keycode,
        shift_up.modifiers
    );

    // release_all must clear a key a reset left down, or a modifier sticks for the boot.
    assert_eq!(keyboard::release_all(), 1, "input-merge: release_all missed the held key");
    assert!(
        next_key("release_all").modifiers & MOD_RELEASED != 0,
        "input-merge: release_all queued a press"
    );
    assert_eq!(keyboard::modifiers(), 0, "input-merge: a modifier survived release_all");

    // An unchanged report must queue nothing — the wake guard depends on it.
    let mut one = [0u8; 8];
    let report = [0u8, 0, 0x05, 0, 0, 0, 0, 0];
    assert_eq!(
        keyboard::handle_report(&mut one, &report),
        1,
        "input-merge: new report queued nothing"
    );
    assert_eq!(
        keyboard::handle_report(&mut one, &report),
        0,
        "input-merge: an unchanged report queued an event"
    );

    // Each keyboard diffs against its own report array; sharing one flapped the held key.
    let mut two = [0u8; 8];
    assert_eq!(
        keyboard::handle_report(&mut two, &[0u8; 8]),
        0,
        "input-merge: an idle second keyboard released the first one's key"
    );
    assert_eq!(keyboard::handle_report(&mut one, &[0u8; 8]), 1, "input-merge: the release went missing");

    drain();

    // Publishing a second pointer's buttons verbatim released the first pointer's held button.
    let tablet = PointerSource::claim().expect("input-merge: no button-table entry for the tablet");
    let usb_mouse = PointerSource::claim().expect("input-merge: no button-table entry for the mouse");
    assert!(tablet != usb_mouse, "input-merge: two pointers claimed one source");
    assert!(
        mouse::handle_motion(PointerSource::PS2, 1, Motion::Relative { dx: 1, dy: 0 }, 0),
        "input-merge: PS/2 motion queued nothing"
    );
    assert!(
        mouse::handle_motion(tablet, 0, Motion::Absolute { x: 100, y: 100 }, 0),
        "input-merge: tablet motion queued nothing"
    );
    assert_eq!(
        last_button_state(),
        1,
        "input-merge: a tablet report with no buttons released the button the other pointer holds"
    );

    // Two pointers on the same bus must not alias to one slot.
    assert!(
        mouse::handle_motion(tablet, 2, Motion::Absolute { x: 101, y: 100 }, 0),
        "input-merge: tablet button queued nothing"
    );
    assert!(
        mouse::handle_motion(usb_mouse, 0, Motion::Relative { dx: 1, dy: 0 }, 0),
        "input-merge: second USB pointer queued nothing"
    );
    assert_eq!(
        last_button_state(),
        3,
        "input-merge: one USB pointer's empty report cleared another USB pointer's button"
    );

    // release_buttons is the only way to clear a source quarantined with a button held.
    assert!(mouse::release_buttons(tablet), "input-merge: releasing a held source queued nothing");
    assert_eq!(last_button_state(), 1, "input-merge: the tablet's button survived its release");
    assert!(
        !mouse::release_buttons(usb_mouse),
        "input-merge: releasing a source that held nothing queued an event"
    );
    assert!(mouse::release_buttons(PointerSource::PS2), "input-merge: PS/2 release queued nothing");
    assert_eq!(last_button_state(), 0, "input-merge: a button survived every release");

    drain();

    // unbind must give the entry back for reuse, or every plug cycle leaks one of 255 sources.
    assert!(
        mouse::handle_motion(usb_mouse, 1, Motion::Relative { dx: 1, dy: 0 }, 0),
        "input-merge: the pointer about to be unplugged queued nothing"
    );
    assert_eq!(last_button_state(), 1, "input-merge: its button did not reach the merge");
    assert!(
        mouse::unbind(usb_mouse),
        "input-merge: unbinding a pointer holding a button published no release"
    );
    assert_eq!(
        last_button_state(),
        0,
        "input-merge: an unplugged pointer's button survived it — every other pointer's motion \
         republishes it, which is a compositor stuck in a drag"
    );
    let replug = PointerSource::claim().expect("input-merge: no entry for the pointer plugged back in");
    assert_eq!(
        replug, usb_mouse,
        "input-merge: a pointer plugged back in took a fresh entry rather than the one its \
         predecessor gave back"
    );
    // Reuse must take the released entry, never one still in use.
    assert!(tablet != replug, "input-merge: the entry a live pointer holds was handed out again");

    drain();

    // Relative motion accumulates in a square space the compositor maps by width and
    // height, so pixels-per-count must differ per axis or the pointer skews by the aspect ratio.
    for (w, h) in [(1920u32, 1200u32), (1024, 768), (3840, 2160), (1200, 1920), (800, 800)] {
        let (sx, sy) = mouse::rel_scale_for(w, h);
        assert!(sx >= 1 && sy >= 1, "input-merge: {w}x{h} scaled a count to nothing");
        let across = sx as u64 * w as u64;
        let down = sy as u64 * h as u64;
        // Each scale is a truncated integer, so each product carries under one
        // screen dimension of error.
        assert!(
            across.abs_diff(down) < (w + h) as u64,
            "input-merge: {w}x{h} moves {across} across and {down} down per count"
        );
    }

    log!("input-merge: ok");
}

/// The buttons the last queued pointer event published, draining the queue.
fn last_button_state() -> u8 {
    let mut last = None;
    while let Some(ev) = mouse::try_read_event() {
        last = Some(ev);
    }
    last.expect("input-merge: no pointer event").buttons
}
