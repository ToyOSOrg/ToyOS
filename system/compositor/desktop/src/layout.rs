use crate::rect::Rect;
use crate::scale::Scale;
use crate::stack::Stack;
use crate::taskbar::Taskbar;
use crate::window::WindowMode;

/// The desktop's text cell, in logical pixels ([`font::CELL`]).
pub const TEXT_CELL: (i32, i32) = (font::CELL.0 as i32, font::CELL.1 as i32);

/// Put the window at `idx` into `mode`, and say what content rect it now needs.
///
/// The buffer behind it stays the shell's problem: a content rect that grew has
/// pixels no memory backs yet, and handing a client a window it cannot draw
/// into is worse than not resizing it. Restoring is
/// [`WindowMode::Normal`] and goes back to whatever the window saved on its way
/// out, which is why [`Window::save_if_normal`] runs before the mode changes
/// and not after.
///
/// [`Window::save_if_normal`]: crate::window::Window::save_if_normal
pub fn set_mode<C>(desk: &Desk, stack: &mut Stack<C>, idx: usize, mode: WindowMode) -> Rect {
    stack[idx].save_if_normal();
    let content = match desk.chrome.mode_frame(mode, desk.screen) {
        Some(frame) => desk.chrome.content(frame),
        None => stack[idx].saved,
    };
    stack[idx].mode = mode;
    stack[idx].content = content;
    content
}

/// Every window of `stack`, laid out on `to` from `from`: a new mode, a new
/// scale or both.
///
/// A normal window keeps its place and its size in logical pixels — its
/// rectangle scales by its corners from one scale to the other — and is
/// brought back onto the work area; a maximized or snapped window takes its
/// mode's frame on the new one, and where it goes back to scales with it. A
/// window's buffer stays the shell's to give it, as [`set_mode`] says.
pub fn relayout<C>(from: &Desk, to: &Desk, stack: &mut Stack<C>) {
    let (a, b) = (from.scale.wire(), to.scale.wire());
    let scaled = |r: Rect| Rect::corners(a.rescale(r.x0, b), a.rescale(r.y0, b), a.rescale(r.x1, b), a.rescale(r.y1, b));
    for idx in 0..stack.len() {
        let win = &mut stack[idx];
        win.saved = scaled(win.saved);
        match win.mode {
            WindowMode::Normal => win.content = to.chrome.reflow(scaled(win.content), to.screen),
            mode => {
                set_mode(to, stack, idx, mode);
            }
        }
    }
}

/// The screen the desktop is drawn on, and the furniture standing on it.
///
/// One value carrying everything the geometry is a function of, so that a
/// screen size, a chrome metric and a font width cannot be passed to different
/// halves of one frame's decisions.
#[derive(Clone, Copy, Debug)]
pub struct Desk {
    pub scale: Scale,
    /// [`Chrome::DEFAULT`] at [`scale`](Self::scale).
    pub chrome: Chrome,
    pub screen: Rect,
    /// Character cell width of the font the taskbar is measured in.
    pub font_w: i32,
    /// How many entries the launcher popup lists.
    pub apps: usize,
}

impl Desk {
    /// The desktop on `screen`, in physical pixels, drawn at `scale`.
    pub const fn new(screen: Rect, scale: Scale, apps: usize) -> Self {
        Self { scale, chrome: Chrome::DEFAULT.scaled(scale), screen, font_w: scale.px(TEXT_CELL.0), apps }
    }

    /// The text cell at this desk's scale.
    pub const fn cell(&self) -> (usize, usize) {
        self.scale.wire().cell()
    }

    /// The bar as it stands with `windows` tabs on it.
    pub fn taskbar(&self, windows: usize) -> Taskbar {
        Taskbar { desk: *self, windows }
    }

    /// The screen minus the taskbar — everything a window may occupy.
    pub fn work_area(&self) -> Rect {
        self.chrome.work_area(self.screen)
    }
}

/// Every dimension of the desktop's furniture, in pixels.
///
/// A value rather than a wall of constants so the geometry below can be
/// exercised at metrics it will never ship with: a test that fixes the border
/// at one pixel proves only that one arithmetic expression matches another
/// with the same literals in it. [`Chrome::DEFAULT`] is what the compositor
/// runs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrome {
    pub border: i32,
    pub title_bar: i32,
    /// A title-bar button — minimize, maximize and close are all this wide.
    pub button: i32,
    /// The square at a window's bottom-right corner that starts a resize.
    pub resize_handle: i32,
    pub taskbar: i32,
    pub taskbar_item: i32,
    pub taskbar_padding: i32,
    pub min_content_w: i32,
    pub min_content_h: i32,
    /// Where a window with no requested size starts, and how far each
    /// successive one is offset from it.
    pub initial_margin: i32,
    pub cascade: i32,
    pub launcher_width: i32,
    pub launcher_item: i32,
    /// From a title bar's or a tab's edge to its text.
    pub text_inset: i32,
    /// From the launcher's edge to an entry's text.
    pub launcher_inset: i32,
    /// From the screen's right edge to the status readout.
    pub status_margin: i32,
}

impl Chrome {
    pub const DEFAULT: Self = Self {
        border: 1,
        title_bar: 28,
        button: 28,
        resize_handle: 16,
        taskbar: 32,
        taskbar_item: 160,
        taskbar_padding: 4,
        min_content_w: 200,
        min_content_h: 100,
        initial_margin: 40,
        cascade: 30,
        launcher_width: 160,
        launcher_item: 28,
        text_inset: 8,
        launcher_inset: 12,
        status_margin: 12,
    };

    /// Every metric at `scale`, each converted alone.
    pub const fn scaled(&self, scale: Scale) -> Self {
        Self {
            border: scale.px(self.border),
            title_bar: scale.px(self.title_bar),
            button: scale.px(self.button),
            resize_handle: scale.px(self.resize_handle),
            taskbar: scale.px(self.taskbar),
            taskbar_item: scale.px(self.taskbar_item),
            taskbar_padding: scale.px(self.taskbar_padding),
            min_content_w: scale.px(self.min_content_w),
            min_content_h: scale.px(self.min_content_h),
            initial_margin: scale.px(self.initial_margin),
            cascade: scale.px(self.cascade),
            launcher_width: scale.px(self.launcher_width),
            launcher_item: scale.px(self.launcher_item),
            text_inset: scale.px(self.text_inset),
            launcher_inset: scale.px(self.launcher_inset),
            status_margin: scale.px(self.status_margin),
        }
    }

    /// What a window's frame adds to its content, left and right.
    pub const fn chrome_w(&self) -> i32 {
        self.border * 2
    }

    /// What a window's frame adds to its content, top and bottom.
    pub const fn chrome_h(&self) -> i32 {
        self.border * 2 + self.title_bar
    }

    /// The window's whole screen rect, given where its content is.
    pub fn frame(&self, content: Rect) -> Rect {
        Rect::corners(
            content.x0 - self.border,
            content.y0 - self.border - self.title_bar,
            content.x1 + self.border,
            content.y1 + self.border,
        )
    }

    /// The inverse of [`frame`](Self::frame).
    pub fn content(&self, frame: Rect) -> Rect {
        Rect::corners(
            frame.x0 + self.border,
            frame.y0 + self.border + self.title_bar,
            frame.x1 - self.border,
            frame.y1 - self.border,
        )
    }

    /// The title bar, borders included — the strip a click can start a drag in.
    pub fn title_strip(&self, frame: Rect) -> Rect {
        Rect::corners(frame.x0, frame.y0, frame.x1, frame.y0 + self.border + self.title_bar)
    }

    /// The three title-bar buttons, right to left: close, maximize, minimize.
    pub fn buttons(&self, frame: Rect) -> [Rect; 3] {
        let top = frame.y0 + self.border;
        let bottom = top + self.title_bar;
        let close_x = frame.x1 - self.border - self.button;
        [
            Rect::corners(close_x, top, close_x + self.button, bottom),
            Rect::corners(close_x - self.button, top, close_x, bottom),
            Rect::corners(close_x - self.button * 2, top, close_x - self.button, bottom),
        ]
    }

    pub fn resize_corner(&self, frame: Rect) -> Rect {
        Rect::corners(frame.x1 - self.resize_handle, frame.y1 - self.resize_handle, frame.x1, frame.y1)
    }

    /// The screen minus the taskbar — everything a window may occupy.
    pub fn work_area(&self, screen: Rect) -> Rect {
        Rect::corners(screen.x0, screen.y0, screen.x1, screen.y1 - self.taskbar)
    }

    /// The frame a window takes when it is maximized or snapped.
    ///
    /// One function for all three because they are one decision: the mode says
    /// which part of the work area the frame fills. [`WindowMode::Normal`] has
    /// no such answer — a normal window's frame is wherever the user left it —
    /// so it returns `None` rather than a plausible rectangle.
    pub fn mode_frame(&self, mode: WindowMode, screen: Rect) -> Option<Rect> {
        let work = self.work_area(screen);
        // A half is `w / 2`, and an odd screen keeps the odd column at the
        // right edge rather than giving it to either window: two halves that
        // summed to the width would make the right one a pixel wider than the
        // left for no reason a user could see.
        let half = work.w() / 2;
        match mode {
            WindowMode::Normal => None,
            WindowMode::Maximized => Some(work),
            WindowMode::SnappedLeft => {
                Some(Rect::corners(work.x0, work.y0, work.x0 + half, work.y1))
            }
            WindowMode::SnappedRight => {
                Some(Rect::corners(work.x0 + half, work.y0, work.x0 + half * 2, work.y1))
            }
        }
    }

    /// Where a new window's content goes.
    ///
    /// `requested` is the client's own size, when it asked for one. Without it
    /// the window fills the work area inside a margin, offset by how many are
    /// already open so a run of them cascades instead of stacking exactly.
    pub fn initial_content(&self, requested: Option<(i32, i32)>, live: usize, screen: Rect) -> Rect {
        let work = self.work_area(screen);
        let frame = match requested {
            Some((w, h)) if w > 0 && h > 0 => {
                let fw = w + self.chrome_w();
                let fh = h + self.chrome_h();
                Rect::new((screen.w() - fw).max(0) / 2, (work.h() - fh).max(0) / 2, fw, fh)
            }
            _ => {
                let offset = self.cascade * (live % 10) as i32;
                Rect::new(
                    self.initial_margin + offset,
                    self.initial_margin + offset,
                    screen.w() - self.initial_margin * 2,
                    work.h() - self.initial_margin * 2,
                )
            }
        };
        self.content(frame)
    }

    /// Where a dragged window's content lands, given the pointer's movement.
    ///
    /// The frame may leave the screen to the right and the bottom — a window
    /// dragged half off is a window half off — but never up or left past its
    /// own chrome, because a title bar off the top edge cannot be grabbed
    /// again.
    pub fn drag_to(&self, content: Rect, dx: i32, dy: i32) -> Rect {
        let moved = content.translate(dx, dy);
        Rect::new(
            moved.x0.max(self.border),
            moved.y0.max(self.border + self.title_bar),
            content.w(),
            content.h(),
        )
    }

    /// Where a drag-resize leaves the content, floored at the minimum size.
    pub fn resize_to(&self, content: Rect, dx: i32, dy: i32) -> Rect {
        Rect::new(
            content.x0,
            content.y0,
            (content.w() + dx).max(self.min_content_w),
            (content.h() + dy).max(self.min_content_h),
        )
    }

    /// Where a window's content goes after the screen changed size.
    ///
    /// Only [`WindowMode::Normal`] reaches here — a maximized or snapped
    /// window is re-derived from [`mode_frame`](Self::mode_frame) instead —
    /// and its size is the client's buffer, so the frame moves and never
    /// shrinks.
    pub fn reflow(&self, content: Rect, screen: Rect) -> Rect {
        self.content(self.frame(content).confine_to(self.work_area(screen)))
    }

    /// Where to put the pointer's grab point when a maximized window is
    /// un-maximized by dragging it.
    ///
    /// The window shrinks under a pointer that is already holding its title
    /// bar, so the grab has to stay at the same *fraction* of the width — grab
    /// the middle of a maximized title bar and the restored window hangs from
    /// its middle. Anchoring at the left edge instead makes a window grabbed
    /// near its right edge jump out from under the pointer.
    pub fn restore_under_pointer(
        &self,
        restored: Rect,
        grab_x: i32,
        old_frame_w: i32,
        pointer: crate::rect::Point,
    ) -> Rect {
        let new_frame_w = restored.w() + self.chrome_w();
        let offset = if old_frame_w > 0 {
            (grab_x.clamp(0, old_frame_w) as i64 * new_frame_w as i64 / old_frame_w as i64) as i32
        } else {
            0
        };
        Rect::new(
            (pointer.x - offset).max(self.border),
            pointer.y.max(self.border + self.title_bar),
            restored.w(),
            restored.h(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rect::Point;

    const SCREEN: Rect = Rect::new(0, 0, 1920, 1080);
    const C: Chrome = Chrome::DEFAULT;

    #[test]
    fn frame_and_content_are_inverses() {
        let content = Rect::new(41, 70, 800, 600);
        assert_eq!(C.content(C.frame(content)), content);
        let frame = Rect::new(10, 10, 400, 300);
        assert_eq!(C.frame(C.content(frame)), frame);
    }

    #[test]
    fn a_maximized_frame_is_exactly_the_work_area() {
        let f = C.mode_frame(WindowMode::Maximized, SCREEN).unwrap();
        assert_eq!(f, Rect::new(0, 0, 1920, 1080 - C.taskbar));
        assert!(!f.overlaps(taskbar_strip()));
    }

    fn taskbar_strip() -> Rect {
        Rect::corners(SCREEN.x0, SCREEN.y1 - C.taskbar, SCREEN.x1, SCREEN.y1)
    }

    #[test]
    fn snapped_halves_never_overlap_and_never_reach_the_taskbar() {
        for screen_w in [1920, 1921, 800, 1366] {
            let screen = Rect::new(0, 0, screen_w, 1080);
            let l = C.mode_frame(WindowMode::SnappedLeft, screen).unwrap();
            let r = C.mode_frame(WindowMode::SnappedRight, screen).unwrap();
            assert!(!l.overlaps(r), "{screen_w}: {l:?} {r:?}");
            assert_eq!(l.w(), r.w(), "{screen_w}");
            let work = C.work_area(screen);
            assert!(work.contains(l) && work.contains(r), "{screen_w}");
        }
    }

    #[test]
    fn a_normal_window_has_no_mode_frame() {
        assert!(C.mode_frame(WindowMode::Normal, SCREEN).is_none());
    }

    #[test]
    fn a_requested_window_is_centred_in_the_work_area() {
        let content = C.initial_content(Some((800, 600)), 0, SCREEN);
        assert_eq!(content.w(), 800);
        assert_eq!(content.h(), 600);
        let frame = C.frame(content);
        assert_eq!(frame.x0, SCREEN.w() - frame.x1);
        assert_eq!(frame.y0, C.work_area(SCREEN).h() - frame.y1);
    }

    #[test]
    fn successive_unsized_windows_cascade_and_wrap() {
        let first = C.initial_content(None, 0, SCREEN);
        let second = C.initial_content(None, 1, SCREEN);
        assert_eq!(second.origin(), Point { x: first.x0 + C.cascade, y: first.y0 + C.cascade });
        assert_eq!(C.initial_content(None, 10, SCREEN).origin(), first.origin());
    }

    #[test]
    fn a_drag_cannot_put_a_title_bar_off_the_top() {
        let content = Rect::new(100, 100, 400, 300);
        let up = C.drag_to(content, 0, -10_000);
        assert!(C.frame(up).y0 >= 0);
        let left = C.drag_to(content, -10_000, 0);
        assert!(C.frame(left).x0 >= 0);
        // Off the right and the bottom is allowed: half a window off the edge
        // is a window a user put there.
        let away = C.drag_to(content, 10_000, 10_000);
        assert!(away.x0 > SCREEN.x1);
    }

    #[test]
    fn a_resize_floors_at_the_minimum_and_never_moves_the_origin() {
        let content = Rect::new(100, 100, 400, 300);
        let tiny = C.resize_to(content, -10_000, -10_000);
        assert_eq!(tiny.w(), C.min_content_w);
        assert_eq!(tiny.h(), C.min_content_h);
        assert_eq!(tiny.origin(), content.origin());
    }

    #[test]
    fn reflow_keeps_the_size_and_brings_the_frame_back_on_screen() {
        let content = Rect::new(1500, 900, 380, 140);
        let small = Rect::new(0, 0, 800, 600);
        let moved = C.reflow(content, small);
        assert_eq!((moved.w(), moved.h()), (content.w(), content.h()));
        assert!(C.work_area(small).contains(C.frame(moved)));
    }

    #[test]
    fn reflow_leaves_a_window_that_already_fits_where_it_is() {
        let content = Rect::new(41, 70, 300, 200);
        assert_eq!(C.reflow(content, SCREEN), content);
    }

    #[test]
    fn buttons_sit_inside_the_title_bar_and_do_not_overlap() {
        let frame = Rect::new(10, 10, 500, 400);
        let strip = C.title_strip(frame);
        let b = C.buttons(frame);
        for r in b {
            assert!(strip.contains(r), "{r:?} outside {strip:?}");
        }
        assert!(!b[0].overlaps(b[1]) && !b[1].overlaps(b[2]) && !b[0].overlaps(b[2]));
    }

    #[test]
    fn an_unmaximized_window_keeps_the_pointer_at_the_same_fraction_of_its_title() {
        let restored = Rect::new(0, 0, 400, 300);
        let old_w = 1920;
        // Grabbed three quarters along a maximized title bar: three quarters
        // along the restored one is where the pointer must still be.
        let content = C.restore_under_pointer(restored, old_w * 3 / 4, old_w, Point { x: 900, y: 500 });
        let frame = C.frame(content);
        let fraction = (900 - frame.x0) as f32 / frame.w() as f32;
        assert!((fraction - 0.75).abs() < 0.01, "{fraction}");
    }

    #[test]
    fn restoring_under_a_pointer_at_the_left_edge_keeps_the_title_bar_reachable() {
        let restored = Rect::new(0, 0, 400, 300);
        let content = C.restore_under_pointer(restored, 1900, 1920, Point { x: 3, y: 1 });
        assert!(C.frame(content).x0 >= 0);
        assert!(C.frame(content).y0 >= 0);
    }

    fn at(text: &str) -> Scale {
        Scale::step(text.parse().unwrap()).unwrap()
    }

    fn window(content: Rect) -> crate::Window<()> {
        crate::Window::new((), content, alloc::string::String::new(), crate::Level::Ordinary, crate::CursorStyle::Default)
    }

    /// The text cell and the frame at the scales the benches and QEMU run at.
    #[test]
    fn the_desk_at_a_scale_converts_each_metric() {
        assert_eq!(C.scaled(Scale::ONE), C);
        for (scale, cell, border, title_bar, taskbar) in
            [("1", (8, 16), 1, 28, 32), ("1.5", (12, 24), 2, 42, 48), ("2", (16, 32), 2, 56, 64)]
        {
            let desk = Desk::new(SCREEN, at(scale), 0);
            assert_eq!(desk.cell(), cell, "{scale}");
            assert_eq!(desk.font_w as usize, cell.0, "{scale}");
            let c = desk.chrome;
            assert_eq!((c.border, c.title_bar, c.taskbar), (border, title_bar, taskbar), "{scale}");
        }
        assert_eq!(Desk::new(SCREEN, at("1.25"), 0).chrome.border, 1);
    }

    /// From 1 to 2 a normal window keeps its logical place and size: its
    /// rectangle doubles by its corners. Back to 1 it is where it was.
    #[test]
    fn a_normal_window_keeps_its_logical_rectangle_across_a_scale() {
        let one = Desk::new(SCREEN, Scale::ONE, 0);
        let two = Desk::new(SCREEN, at("2"), 0);
        let mut stack = Stack::default();
        stack.insert(window(Rect::new(100, 120, 200, 100)));
        relayout(&one, &two, &mut stack);
        assert_eq!(stack[0].content, Rect::new(200, 240, 400, 200));
        relayout(&two, &one, &mut stack);
        assert_eq!(stack[0].content, Rect::new(100, 120, 200, 100));
    }

    /// 1.5 and back: a corner on a half pixel rounds away and back, and
    /// moves the window by one pixel at most.
    #[test]
    fn a_normal_window_drifts_a_pixel_at_most_through_a_fractional_scale() {
        let one = Desk::new(SCREEN, Scale::ONE, 0);
        let half = Desk::new(SCREEN, at("1.5"), 0);
        let mut stack = Stack::default();
        let start = Rect::new(101, 33, 301, 97);
        stack.insert(window(start));
        relayout(&one, &half, &mut stack);
        relayout(&half, &one, &mut stack);
        let back = stack[0].content;
        for (a, b) in [(back.x0, start.x0), (back.y0, start.y0), (back.x1, start.x1), (back.y1, start.y1)] {
            assert!((a - b).abs() <= 1, "{back:?} from {start:?}");
        }
    }

    /// A window that fits at 1 and is too far right at 2 comes back onto the
    /// work area whole; a maximized one fills the new work area, and goes back
    /// to its own rectangle at the new scale.
    #[test]
    fn a_relaid_window_stays_on_the_work_area() {
        let one = Desk::new(SCREEN, Scale::ONE, 0);
        let two = Desk::new(SCREEN, at("2"), 0);
        let mut stack = Stack::default();
        stack.insert(window(Rect::new(1500, 800, 300, 150)));
        stack.insert(window(Rect::new(10, 40, 300, 200)));
        set_mode(&one, &mut stack, 1, WindowMode::Maximized);
        relayout(&one, &two, &mut stack);
        let work = two.work_area();
        assert_eq!((stack[0].content.w(), stack[0].content.h()), (600, 300));
        assert!(work.contains(two.chrome.frame(stack[0].content)), "{:?}", stack[0].content);
        assert_eq!(two.chrome.frame(stack[1].content), work);
        assert_eq!(stack[1].saved, Rect::new(20, 80, 600, 400));
    }

    /// The geometry is derived, not four expressions that happen to share the
    /// shipping literals.
    #[test]
    fn the_layout_follows_the_metrics_it_is_given() {
        let fat = Chrome { border: 4, title_bar: 40, taskbar: 60, ..Chrome::DEFAULT };
        let work = fat.work_area(SCREEN);
        assert_eq!(work.h(), 1080 - 60);
        let content = fat.content(fat.mode_frame(WindowMode::Maximized, SCREEN).unwrap());
        assert_eq!(content.w(), 1920 - 8);
        assert_eq!(content.h(), 1080 - 60 - 8 - 40);
    }
}
