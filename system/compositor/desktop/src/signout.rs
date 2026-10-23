//! The desktop session's sign-out view: one line above the taskbar while a
//! sign-out waits, naming what it waits for and the two keys that decide. A
//! sign-out that is the machine's stop says so: its keys decide the stop.

use alloc::format;
use alloc::string::String;

use crate::layout::Desk;
use crate::rect::Rect;

/// The widest line the view shows: its words, then as many names as fit.
pub const SIGN_OUT_CHARS: usize = 72;

/// What the view says for a sign-out, the machine's stop where `stopping`,
/// waiting on `waiting`, and `more` it was not told the names of, clipped to
/// [`SIGN_OUT_CHARS`].
pub fn sign_out_text<'a>(stopping: bool, waiting: impl IntoIterator<Item = &'a str>, more: u32) -> String {
    const KEYS: &str = " - Esc cancels, Enter ends them";
    let lead = if stopping { "Stopping, waiting for: " } else { "Waiting for: " };
    let mut names = String::new();
    let mut left = more as usize;
    let mut waiting = waiting.into_iter().peekable();
    if waiting.peek().is_none() && more == 0 {
        return String::from(if stopping { "The machine stops" } else { "Signing out" });
    }
    for name in waiting.by_ref() {
        let next = if names.is_empty() { String::from(name) } else { format!(", {name}") };
        // Room kept for the count of names left out, ` +` and five digits.
        if lead.len() + names.len() + next.len() + " +99999".len() + KEYS.len() > SIGN_OUT_CHARS {
            left += 1;
            continue;
        }
        names.push_str(&next);
    }
    let rest = if left > 0 { format!(" +{left}") } else { String::new() };
    let text = format!("{lead}{names}{rest}{KEYS}");
    text.chars().take(SIGN_OUT_CHARS).collect()
}

/// Where the view is: centred above the taskbar, one taskbar high.
pub fn sign_out_rect(desk: &Desk) -> Rect {
    let strip = desk.taskbar(0).strip();
    let w = (SIGN_OUT_CHARS as i32 * desk.font_w + desk.chrome.taskbar).min(strip.w());
    let h = desk.chrome.taskbar;
    let x0 = strip.x0 + (strip.w() - w) / 2;
    let y1 = strip.y0 - desk.chrome.taskbar_padding * 2;
    Rect::corners(x0, y1 - h, x0 + w, y1)
}

/// Whether the compositor has asked for a desktop session and had no answer
/// yet: while it has, a launch asks for no second one. **Every answer ends
/// the wait**, a refusal as much as a session, so a later launch asks again.
#[derive(Debug, Default)]
pub struct Opening {
    asked: bool,
}

impl Opening {
    /// Whether to ask for a session now, `open` saying whether one is: asked
    /// once, until it is answered.
    pub fn ask(&mut self, open: bool) -> bool {
        if open || self.asked {
            return false;
        }
        self.asked = true;
        true
    }

    /// The open was answered, with a session or a refusal.
    pub fn answered(&mut self) {
        self.asked = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    /// One open is asked at a time, and a refused one is asked again by the
    /// next launch, as a granted one ends the asking.
    #[test]
    fn a_refused_open_is_asked_again() {
        let mut opening = Opening::default();
        assert!(opening.ask(false));
        assert!(!opening.ask(false), "a second open was asked before the first was answered");
        opening.answered();
        assert!(opening.ask(false), "a refused open was never asked again");
        opening.answered();
        assert!(!opening.ask(true), "an open was asked with a session open");
    }

    #[test]
    fn the_view_names_what_waits_and_the_keys() {
        assert_eq!(sign_out_text(false, [], 0), "Signing out");
        assert_eq!(sign_out_text(false, ["holder"], 0), "Waiting for: holder - Esc cancels, Enter ends them");
        assert_eq!(sign_out_text(false, ["a", "b"], 2), "Waiting for: a, b +2 - Esc cancels, Enter ends them");
        assert_eq!(sign_out_text(true, [], 0), "The machine stops");
        assert_eq!(sign_out_text(true, ["holder"], 0), "Stopping, waiting for: holder - Esc cancels, Enter ends them");
    }

    /// However many names, the line fits its box and still says the keys.
    #[test]
    fn a_long_list_is_cut_to_the_box_and_keeps_its_keys() {
        let names: Vec<String> = (0..40).map(|i| format!("application{i}")).collect();
        for stopping in [false, true] {
            let text = sign_out_text(stopping, names.iter().map(String::as_str), 0);
            assert!(text.chars().count() <= SIGN_OUT_CHARS, "{text}");
            assert!(text.ends_with("Enter ends them"), "{text}");
            assert!(text.contains('+'), "{text}");
            let long = "x".repeat(200);
            assert!(sign_out_text(stopping, [long.as_str()], 0).chars().count() <= SIGN_OUT_CHARS);
        }
    }

    #[test]
    fn the_view_sits_on_the_screen_above_the_taskbar() {
        let desk = Desk::new(Rect::new(0, 0, 1920, 1080), crate::Scale::ONE, 2);
        let rect = sign_out_rect(&desk);
        assert!(!rect.is_empty());
        assert!(rect.y1 <= desk.taskbar(0).strip().y0);
        assert!(rect.x0 >= 0 && rect.x1 <= 1920);
        let narrow = Desk::new(Rect::new(0, 0, 320, 240), crate::Scale::ONE, 2);
        let rect = sign_out_rect(&narrow);
        assert!(rect.x0 >= 0 && rect.x1 <= 320, "{rect:?}");
    }
}
