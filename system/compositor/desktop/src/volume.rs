//! The volume keys' half of the desktop: the requests the compositor keeps in
//! flight to soundserver's `volume` port, and the overlay that shows each
//! answer.
//!
//! **The compositor never waits on soundserver.** One request is in flight at
//! a time; presses that arrive meanwhile are added up here and sent as one when
//! the answer comes.

use alloc::format;
use alloc::string::String;

use crate::layout::Desk;
use crate::rect::Rect;

/// Which volume key was pressed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VolumeKey {
    Mute,
    Up,
    Down,
}

/// One request on the `volume` port.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Request {
    /// Move the level by this many positions down; a step unmutes.
    Step(i32),
    /// Mute, or unmute at the level it had.
    Mute,
}

/// The presses not yet sent, in the one order that keeps their meaning: a
/// step unmutes, so only the mutes pressed after the last step decide what
/// follows it.
#[derive(Default)]
pub struct Requests {
    in_flight: bool,
    /// Positions down, summed over every step pressed since the last send.
    steps: i32,
    stepped: bool,
    /// Mute presses since the last step, whose parity is all that counts.
    mutes: u32,
}

impl Requests {
    /// A press; answers the request to send now, if none is in flight.
    pub fn press(&mut self, key: VolumeKey) -> Option<Request> {
        match key {
            VolumeKey::Mute => self.mutes += 1,
            // Up raises the level, which is a position nearer the top.
            VolumeKey::Up | VolumeKey::Down => {
                self.steps = self.steps.saturating_add(if key == VolumeKey::Up { -1 } else { 1 });
                self.stepped = true;
                self.mutes = 0;
            }
        }
        if self.in_flight { None } else { self.take() }
    }

    /// The request in flight was answered; answers the next to send.
    pub fn answered(&mut self) -> Option<Request> {
        self.in_flight = false;
        self.take()
    }

    /// The connection ended: nothing is in flight on it, and what was pressed
    /// and not sent goes on the next one.
    pub fn disconnected(&mut self) {
        self.in_flight = false;
    }

    fn take(&mut self) -> Option<Request> {
        let request = if self.stepped {
            self.stepped = false;
            Some(Request::Step(core::mem::take(&mut self.steps)))
        } else if core::mem::take(&mut self.mutes) % 2 == 1 {
            Some(Request::Mute)
        } else {
            None
        };
        self.in_flight = request.is_some();
        request
    }
}

/// What an answer from soundserver says.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shown {
    /// `position` positions below the top, of `bottom`.
    Level { position: u8, bottom: u8 },
    Muted,
    Unavailable,
}

impl Shown {
    /// The overlay's text: the level counted up from the bottom.
    pub fn text(self) -> String {
        match self {
            Self::Level { position, bottom } => {
                format!("Volume {}/{bottom}", bottom.saturating_sub(position))
            }
            Self::Muted => String::from("Volume muted"),
            Self::Unavailable => String::from("Volume unavailable"),
        }
    }
}

/// How long the overlay stays after the last answer, in nanoseconds.
pub const OVERLAY_HOLD_NANOS: u64 = 1_500_000_000;

/// The overlay: shown by an answer, hidden [`OVERLAY_HOLD_NANOS`] after the last.
#[derive(Default)]
pub struct Overlay {
    shown: Option<(Shown, u64)>,
}

impl Overlay {
    /// An answer arrived at `now`.
    pub fn answered(&mut self, shown: Shown, now: u64) {
        self.shown = Some((shown, now.saturating_add(OVERLAY_HOLD_NANOS)));
    }

    /// What it shows at `now`, if it is up.
    pub fn visible(&self, now: u64) -> Option<Shown> {
        self.shown.filter(|&(_, until)| now < until).map(|(shown, _)| shown)
    }

    /// Forget an overlay that has hidden at `now`; answers whether it did, so
    /// the compositor damages where it was.
    pub fn expire(&mut self, now: u64) -> bool {
        if self.shown.is_some_and(|(_, until)| now >= until) {
            self.shown = None;
            return true;
        }
        false
    }
}

/// The widest overlay text, in characters.
const OVERLAY_CHARS: i32 = 18;

/// Where the overlay is drawn: above the taskbar, centred, one taskbar high.
pub fn overlay_rect(desk: &Desk) -> Rect {
    let strip = desk.taskbar(0).strip();
    let w = OVERLAY_CHARS * desk.font_w + desk.chrome.taskbar;
    let h = desk.chrome.taskbar;
    let x0 = strip.x0 + (strip.w() - w) / 2;
    Rect::corners(x0, strip.y0 - h - desk.chrome.taskbar_padding * 2, x0 + w, strip.y0 - desk.chrome.taskbar_padding * 2)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_request_is_in_flight_and_the_presses_meanwhile_go_as_one() {
        let mut requests = Requests::default();
        assert_eq!(requests.press(VolumeKey::Up), Some(Request::Step(-1)));
        assert_eq!(requests.press(VolumeKey::Down), None);
        assert_eq!(requests.press(VolumeKey::Down), None);
        assert_eq!(requests.press(VolumeKey::Down), None);
        assert_eq!(requests.answered(), Some(Request::Step(3)));
        assert_eq!(requests.answered(), None);
        // Nothing in flight, so the next press goes at once.
        assert_eq!(requests.press(VolumeKey::Mute), Some(Request::Mute));
    }

    #[test]
    fn only_the_mutes_after_the_last_step_follow_it() {
        let mut requests = Requests::default();
        assert_eq!(requests.press(VolumeKey::Mute), Some(Request::Mute));
        // A mute, a step that unmutes, then a mute: the step, then the mute.
        requests.press(VolumeKey::Mute);
        requests.press(VolumeKey::Up);
        requests.press(VolumeKey::Mute);
        assert_eq!(requests.answered(), Some(Request::Step(-1)));
        assert_eq!(requests.answered(), Some(Request::Mute));
        assert_eq!(requests.answered(), None);
        // Two mutes cancel.
        assert_eq!(requests.press(VolumeKey::Mute), Some(Request::Mute));
        requests.press(VolumeKey::Mute);
        requests.press(VolumeKey::Mute);
        assert_eq!(requests.answered(), None);
    }

    #[test]
    fn a_connection_that_ends_sends_what_was_pressed_on_the_next() {
        let mut requests = Requests::default();
        assert_eq!(requests.press(VolumeKey::Up), Some(Request::Step(-1)));
        requests.disconnected();
        assert_eq!(requests.press(VolumeKey::Up), Some(Request::Step(-1)));
    }

    #[test]
    fn an_answer_shows_the_overlay_and_it_hides_a_hold_after_the_last() {
        let mut overlay = Overlay::default();
        assert_eq!(overlay.visible(0), None);
        let level = Shown::Level { position: 2, bottom: 22 };
        overlay.answered(level, 100);
        assert_eq!(overlay.visible(100), Some(level));
        // A second answer before the first's hold restarts it.
        overlay.answered(Shown::Muted, 1_000_000_000);
        assert_eq!(overlay.visible(100 + OVERLAY_HOLD_NANOS), Some(Shown::Muted));
        assert!(!overlay.expire(1_000_000_000 + OVERLAY_HOLD_NANOS - 1));
        assert_eq!(overlay.visible(1_000_000_000 + OVERLAY_HOLD_NANOS), None);
        assert!(overlay.expire(1_000_000_000 + OVERLAY_HOLD_NANOS));
        assert!(!overlay.expire(u64::MAX));
    }

    #[test]
    fn the_overlay_says_the_level_from_the_bottom_and_fits_its_box() {
        assert_eq!(Shown::Level { position: 0, bottom: 22 }.text(), "Volume 22/22");
        assert_eq!(Shown::Level { position: 22, bottom: 22 }.text(), "Volume 0/22");
        for shown in [Shown::Level { position: 255, bottom: 255 }, Shown::Muted, Shown::Unavailable] {
            assert!(shown.text().chars().count() <= OVERLAY_CHARS as usize);
        }
    }

    #[test]
    fn the_overlay_sits_above_the_taskbar_centred() {
        let desk = crate::layout::Desk {
            screen: Rect::new(0, 0, 1920, 1080),
            chrome: crate::layout::Chrome::DEFAULT,
            font_w: 8,
            apps: 0,
        };
        let r = overlay_rect(&desk);
        let strip = desk.taskbar(0).strip();
        assert!(r.y1 <= strip.y0);
        assert_eq!(r.x0 - strip.x0, strip.x1 - r.x1);
        assert!(r.w() as usize >= OVERLAY_CHARS as usize * 8);
    }
}
