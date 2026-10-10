use crate::rect::Rect;
use crate::window::Level;

/// Share of physical memory the compositor will hold in window buffers.
///
/// Policy, not derivation. Nothing in the kernel says what a process may use —
/// no per-process limit, no pressure signal, no OOM killer — so the quantity
/// that would make this derivable does not exist yet. An eighth leaves seven
/// eighths for the kernel, the other daemons, and the clients' own heaps.
pub const WINDOW_BUDGET_SHARE: u64 = 8;

/// The kernel rounds every shared region up to one of these
/// (`shared_memory::alloc` calls `align_2m`), so this and not the pixel count
/// is what a window costs.
const PAGE_2M: u64 = 2 * 1024 * 1024;

/// Most physical memory one window can cost at this screen size.
///
/// A screen-sized content buffer is the largest the compositor ever hands out:
/// `MSG_CREATE_WINDOW` refuses a request bigger than the screen, and every
/// path that grows a window afterwards — maximize, snap, drag-resize — is
/// bounded by the screen too. A one-pixel window still costs a whole page.
///
/// Deliberately a function of the screen — a bigger screen means bigger
/// windows means fewer of them — which no bare constant expresses.
pub fn window_bytes(screen: Rect) -> u64 {
    (screen.area() * 4).div_ceil(PAGE_2M).max(1) * PAGE_2M
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

/// Whether a connection just accepted may wait for its first frame, off
/// `prompt` or off the clients' port, beside the ones already `waiting` (each
/// `true` for one off `prompt`): **one off `prompt` has a slot of its own**,
/// and the clients' share `max`, so no client filling the table keeps the
/// prompt from being made.
pub fn pending_admits(prompt: bool, waiting: impl IntoIterator<Item = bool>, max: usize) -> bool {
    let mut waiting = waiting.into_iter();
    match prompt {
        true => !waiting.any(|p| p),
        false => waiting.filter(|p| !p).count() < max,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HD: Rect = Rect::new(0, 0, 1920, 1080);

    #[test]
    fn a_window_costs_whole_pages() {
        assert_eq!(window_bytes(Rect::new(0, 0, 1, 1)), PAGE_2M);
        // 1920*1080*4 = 8_294_400, which is three pages and most of a fourth.
        assert_eq!(window_bytes(HD), 4 * PAGE_2M);
        assert_eq!(window_bytes(Rect::new(0, 0, 2048, 2048)), 8 * PAGE_2M);
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
        let full = [false; 32];
        assert!(!pending_admits(false, full, 32));
        assert!(pending_admits(true, full, 32));
        assert!(!pending_admits(true, full.iter().copied().chain([true]), 32));
        assert!(pending_admits(false, [true; 1].into_iter().chain([false; 31]), 32));
    }
}
