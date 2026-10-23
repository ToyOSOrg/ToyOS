use crate::layout::Desk;
use crate::rect::Rect;
use crate::window::Level;
use toyos_abi::syscall::PAGE_SIZE;

/// Share of physical memory the compositor will hold in window buffers.
///
/// Policy, not derivation. Nothing in the kernel says what a process may use —
/// no per-process limit, no pressure signal, no OOM killer — so the quantity
/// that would make this derivable does not exist yet. An eighth leaves seven
/// eighths for the kernel, the other daemons, and the clients' own heaps.
pub const WINDOW_BUDGET_SHARE: u64 = 8;

/// Most physical memory one window can cost at this screen size: two
/// screen-sized buffers.
///
/// A screen-sized content buffer is the largest the compositor ever hands out:
/// `MSG_CREATE_WINDOW` refuses a physical request bigger than the screen and
/// fits a logical one to the work area, and every path that grows a window
/// afterwards — maximize, snap, drag-resize, a relay at a larger scale — is
/// bounded by the screen too. **A window holds two from a resize until its
/// client presents into the new one, and never more** ([`crate::Buffers`]) —
/// every window at once on a change of scale, and for as long as a client
/// that never presents again lives. A buffer costs whole [`PAGE_SIZE`]s, so a
/// one-pixel window still costs two of them.
///
/// Deliberately a function of the screen — a bigger screen means bigger
/// windows means fewer of them — which no bare constant expresses.
pub fn window_bytes(screen: Rect) -> u64 {
    let page = PAGE_SIZE as u64;
    2 * (screen.area() * 4).div_ceil(page).max(1) * page
}

/// How many windows the compositor will hold at this screen size.
///
/// One eighth of physical memory divided by what a window costs there, floored
/// at one — a compositor that can never open a window is worse than one over
/// its budget by a single window — and capped at `slots`, what one poller can
/// watch.
///
/// **This is a mitigation, and the thing it mitigates is the real defect.** A
/// window buffer is charged to nobody: there is no per-process memory limit, no
/// pressure signal and no OOM killer (`issues/no-physical-memory-fairness.md`), so without a
/// cap any client can walk the machine into exhaustion by asking for windows,
/// and the compositor cannot tell a desktop from an attack. The 2 MiB rounding
/// amplifies it: at 2048x2048 a window costs exactly 16 MiB, so 64 windows is
/// a gigabyte. Read this as "how much we can afford to hand out while nothing
/// can make us take it back", not as a considered UX limit — the number to
/// delete this in favour of is a kernel memory limit.
pub fn max_windows(total_mem: u64, screen: Rect, slots: usize) -> usize {
    let budget = total_mem / WINDOW_BUDGET_SHARE;
    (budget / window_bytes(screen)).clamp(1, slots as u64) as usize
}

/// Whether a `MSG_CREATE_WINDOW` gets a window.
///
/// Every arm but [`Allow`](Self::Allow) is an answer to untrusted input, so
/// none of them is a panic and none is a silent shrink of what was asked for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    Allow,
    AtCapacity,
    TooLarge,
}

/// A size a client asked for, against the screen and the budget.
///
/// `requested` is `(0, 0)` for a client that did not name a size, which is a
/// request for whatever the desktop gives it and can never be too large.
///
/// **The prompt's window is outside the budget**: `clients`, the windows
/// below the prompt layer, are held to `max`, and the prompt to the one its
/// layer admits ([`crate::Stack::admits`]), so no client filling the desktop
/// keeps the person at the screen from being asked. One window past the
/// memory budget, as [`max_windows`]'s floor is.
pub fn create_verdict(requested: (u32, u32), screen: Rect, level: Level, clients: usize, max: usize) -> Verdict {
    if level != Level::Prompt && clients >= max {
        return Verdict::AtCapacity;
    }
    let (w, h) = requested;
    if w > screen.w() as u32 || h > screen.h() as u32 {
        return Verdict::TooLarge;
    }
    Verdict::Allow
}

/// The physical size a window's request comes to on `desk`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Requested {
    /// No size: the desktop chooses.
    Any,
    /// This size, which [`create_verdict`] still judges.
    Exact(u32, u32),
    /// A logical size whose frame would pass the work area at the scale,
    /// fitted to it: a toolkit's preference, shrunk and said, never refused.
    Fitted { asked: (u32, u32), given: (u32, u32) },
}

impl Requested {
    /// What [`create_verdict`] is asked about: `(0, 0)` for no size.
    pub fn size(self) -> (u32, u32) {
        match self {
            Self::Any => (0, 0),
            Self::Exact(w, h) | Self::Fitted { given: (w, h), .. } => (w, h),
        }
    }
}

/// `size` as `desk` gives it: a physical size as it is, a logical one at the
/// desk's scale and fitted to the work area.
pub fn requested(size: window::Size, desk: &Desk) -> Requested {
    let Some(asked) = size.physical(desk.scale.wire()) else { return Requested::Any };
    if !matches!(size, window::Size::Logical { .. }) {
        return Requested::Exact(asked.0, asked.1);
    }
    let (w, h) = desk.chrome.largest_content(desk.screen);
    let given = (asked.0.min(w as u32), asked.1.min(h as u32));
    if given == asked { Requested::Exact(asked.0, asked.1) } else { Requested::Fitted { asked, given } }
}

/// Which of the compositor's ports a connection came in on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Port {
    /// `compositor`, the clients'.
    Compositor,
    /// `prompt`, the layer above every window.
    Prompt,
    /// `clipboard`, its watcher's.
    Clipboard,
    /// `display`, its settings' holder's.
    Display,
}

/// Whether a connection just accepted off `port` may wait for its first frame,
/// beside the ones already `waiting`: **one off each port but `compositor`
/// has a slot of its own**, and the clients' share `max`, so no client filling
/// the table keeps the prompt from being made, the clipboard from being
/// watched or the display from being set.
pub fn pending_admits(port: Port, waiting: impl IntoIterator<Item = Port>, max: usize) -> bool {
    let mut waiting = waiting.into_iter();
    match port {
        Port::Compositor => waiting.filter(|p| *p == Port::Compositor).count() < max,
        own => !waiting.any(|p| p == own),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: Rect = Rect::new(0, 0, 1920, 1080);

    /// Two buffers, each of whole pages.
    #[test]
    fn a_window_costs_two_buffers_of_whole_pages() {
        let page = PAGE_SIZE as u64;
        assert_eq!(window_bytes(Rect::new(0, 0, 1, 1)), 2 * page);
        // 1920*1080*4 = 8_294_400, which is 2,025 pages exactly.
        assert_eq!(window_bytes(HD), 2 * 2025 * page);
        assert_eq!(window_bytes(Rect::new(0, 0, 2048, 2048)), 2 * 4096 * page);
    }

    #[test]
    fn a_bigger_screen_affords_fewer_windows() {
        let mem = 4 * 1024 * 1024 * 1024;
        let small = max_windows(mem, Rect::new(0, 0, 640, 480), 1000);
        let big = max_windows(mem, Rect::new(0, 0, 3840, 2160), 1000);
        assert!(small > big, "{small} vs {big}");
    }

    #[test]
    fn a_machine_with_almost_no_memory_still_gets_one_window() {
        assert_eq!(max_windows(1, HD, 1000), 1);
    }

    #[test]
    fn the_poller_ceiling_binds_before_memory_does() {
        assert_eq!(max_windows(u64::MAX, HD, 61), 61);
    }

    #[test]
    fn a_window_larger_than_the_screen_is_refused_by_name() {
        assert_eq!(create_verdict((1921, 1080), HD, Level::Ordinary, 0, 10), Verdict::TooLarge);
        assert_eq!(create_verdict((1920, 1081), HD, Level::Ordinary, 0, 10), Verdict::TooLarge);
        assert_eq!(create_verdict((1920, 1080), HD, Level::Ordinary, 0, 10), Verdict::Allow);
    }

    #[test]
    fn capacity_is_checked_before_size() {
        // Both would refuse; the one that names the compositor's own budget
        // wins, because it is the one a client can wait out.
        assert_eq!(create_verdict((9999, 9999), HD, Level::Ordinary, 10, 10), Verdict::AtCapacity);
    }

    #[test]
    fn a_client_that_names_no_size_is_never_too_large() {
        assert_eq!(create_verdict((0, 0), Rect::new(0, 0, 4, 4), Level::Ordinary, 0, 10), Verdict::Allow);
    }

    /// **A desktop full of clients still lets the prompt in**: every client
    /// window and every waiting client connection is spent, and the prompt's
    /// window and its connection are each admitted; a second of either is not.
    #[test]
    fn the_prompt_is_admitted_past_every_client_ceiling() {
        for level in [Level::Ordinary, Level::Topmost] {
            assert_eq!(create_verdict((600, 400), HD, level, 10, 10), Verdict::AtCapacity, "{level:?}");
        }
        assert_eq!(create_verdict((600, 400), HD, Level::Prompt, 10, 10), Verdict::Allow);
        assert_eq!(create_verdict((1921, 400), HD, Level::Prompt, 0, 10), Verdict::TooLarge);
        let full = [Port::Compositor; 32];
        assert!(!pending_admits(Port::Compositor, full, 32));
        assert!(pending_admits(Port::Prompt, full, 32));
        assert!(!pending_admits(Port::Prompt, full.iter().copied().chain([Port::Prompt]), 32));
        assert!(pending_admits(Port::Compositor, [Port::Prompt].into_iter().chain([Port::Compositor; 31]), 32));
    }

    /// doom asks for 960x600 logical. At 2 on a 1920x1080 panel that is
    /// 1920x1200, past the work area: it is fitted and given a window, where
    /// a physical request that large is refused.
    #[test]
    fn a_logical_request_past_the_work_area_is_fitted_and_a_physical_one_refused() {
        let at = |s: &str| crate::Scale::step(s.parse().unwrap()).unwrap();
        let desk = Desk::new(HD, at("2"), 0);
        let doom = window::Size::Logical { width: 960, height: 600 };
        let (w, h) = desk.chrome.largest_content(HD);
        let Requested::Fitted { asked, given } = requested(doom, &desk) else { panic!("{:?}", requested(doom, &desk)) };
        assert_eq!((asked, given), ((1920, 1200), (w as u32, h as u32)));
        let content = desk.chrome.initial_content(Some((w, h)), 0, HD);
        assert!(desk.work_area().contains(desk.chrome.frame(content)), "{content:?}");
        assert_eq!(create_verdict(given, HD, Level::Ordinary, 0, 10), Verdict::Allow);
        assert_eq!(requested(doom, &Desk::new(HD, crate::Scale::ONE, 0)), Requested::Exact(960, 600));
        let physical = window::Size::Physical { width: 1920, height: 1200 };
        assert_eq!(requested(physical, &desk), Requested::Exact(1920, 1200));
        assert_eq!(create_verdict((1920, 1200), HD, Level::Ordinary, 0, 10), Verdict::TooLarge);
        assert_eq!(requested(window::Size::Any, &desk), Requested::Any);
    }

    /// The display's holder waits in a slot of its own, as the prompt does.
    #[test]
    fn a_display_request_is_admitted_past_every_client_ceiling() {
        let full = [Port::Compositor; 32];
        assert!(pending_admits(Port::Display, full.iter().copied().chain([Port::Clipboard]), 32));
        assert!(!pending_admits(Port::Display, full.iter().copied().chain([Port::Display]), 32));
    }

    /// The clipboard's watcher waits in a slot of its own, as the prompt does.
    #[test]
    fn the_watcher_is_admitted_past_every_client_ceiling() {
        let full = [Port::Compositor; 32];
        assert!(pending_admits(Port::Clipboard, full.iter().copied().chain([Port::Prompt]), 32));
        assert!(!pending_admits(Port::Clipboard, full.iter().copied().chain([Port::Clipboard]), 32));
        assert!(pending_admits(Port::Compositor, [Port::Clipboard].into_iter().chain([Port::Compositor; 31]), 32));
    }
}
