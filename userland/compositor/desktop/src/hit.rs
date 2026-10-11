use crate::layout::Desk;
use crate::rect::Point;
use crate::stack::Stack;
use crate::window::WindowMode;

/// What is under the pointer.
///
/// The window indices are into the stack as it stands when the test ran; every
/// caller acts on the answer before it reorders anything.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Hit {
    Desktop,
    /// A prompt is up and the pointer is not over its content: nothing is hit.
    Blocked,
    TitleBar(usize),
    MinimizeButton(usize),
    MaximizeButton(usize),
    CloseButton(usize),
    Content(usize),
    ResizeCorner(usize),
    TaskbarItem(usize),
    TaskbarNew,
    LauncherItem(usize),
}

/// What the pointer is over, at `p`.
///
/// Front to back, and against exactly the rectangles the renderer paints:
/// `Chrome::buttons` gives both this and the painter their rects, so a click
/// that closes a window is a click on the close button as drawn. It was not —
/// the hit test used an open-coded half-open half-unbounded expression, which
/// made the frame's border pixel beside the button close the window too.
pub fn hit_test<C>(desk: &Desk, stack: &Stack<C>, p: Point, launcher_open: bool) -> Hit {
    // While a prompt is up its content is the one thing the pointer reaches:
    // not the taskbar, the launcher, another window, nor its own chrome, so it
    // is not moved, minimized or closed but by its own answer.
    if let Some(prompt) = stack.prompt() {
        return match stack[prompt].content.contains_point(p) {
            true => Hit::Content(prompt),
            false => Hit::Blocked,
        };
    }
    let bar = desk.taskbar(stack.len());

    if launcher_open && bar.launcher().contains_point(p) {
        for i in 0..desk.apps {
            if bar.launcher_item(i).contains_point(p) {
                return Hit::LauncherItem(i);
            }
        }
    }

    if bar.strip().contains_point(p) {
        for i in 0..stack.len() {
            if bar.tab(i).contains_point(p) {
                return Hit::TaskbarItem(i);
            }
        }
        if bar.new_button().contains_point(p) {
            return Hit::TaskbarNew;
        }
        return Hit::Desktop;
    }

    for (idx, win) in stack.iter().enumerate().rev() {
        if win.minimized {
            continue;
        }
        let frame = win.frame(&desk.chrome);
        if !frame.contains_point(p) {
            continue;
        }
        let [close, maximize, minimize] = desk.chrome.buttons(frame);
        if close.contains_point(p) {
            return Hit::CloseButton(idx);
        }
        if maximize.contains_point(p) {
            return Hit::MaximizeButton(idx);
        }
        if minimize.contains_point(p) {
            return Hit::MinimizeButton(idx);
        }
        // A maximized or snapped window has no resize corner: its size is the
        // mode's to decide, and dragging one would leave a window whose mode
        // and geometry disagree.
        if win.mode == WindowMode::Normal && desk.chrome.resize_corner(frame).contains_point(p) {
            return Hit::ResizeCorner(idx);
        }
        if desk.chrome.title_strip(frame).contains_point(p) {
            return Hit::TitleBar(idx);
        }
        return Hit::Content(idx);
    }

    Hit::Desktop
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::CursorStyle;
    use crate::layout::Chrome;
    use crate::rect::Rect;
    use crate::window::{Level, Window};
    use alloc::string::ToString;

    const DESK: Desk = Desk {
        chrome: Chrome::DEFAULT,
        screen: Rect::new(0, 0, 1920, 1080),
        font_w: 8,
        apps: 2,
    };

    fn at(x: i32, y: i32) -> Point {
        Point { x, y }
    }

    fn one_window(content: Rect) -> Stack<u32> {
        let mut s = Stack::default();
        s.insert(Window::new(0, content, "w".to_string(), Level::Ordinary, CursorStyle::Default));
        s
    }

    #[test]
    fn nothing_under_the_pointer_is_the_desktop() {
        let s: Stack<u32> = Stack::default();
        assert_eq!(hit_test(&DESK, &s, at(500, 500), false), Hit::Desktop);
    }

    #[test]
    fn each_button_answers_for_exactly_the_rect_it_is_drawn_in() {
        let content = Rect::new(101, 130, 400, 300);
        let s = one_window(content);
        let frame = DESK.chrome.frame(content);
        let names = [Hit::CloseButton(0), Hit::MaximizeButton(0), Hit::MinimizeButton(0)];
        for (rect, want) in DESK.chrome.buttons(frame).into_iter().zip(names) {
            for p in [
                at(rect.x0, rect.y0),
                at(rect.x1 - 1, rect.y1 - 1),
                at(i32::midpoint(rect.x0, rect.x1), i32::midpoint(rect.y0, rect.y1)),
            ] {
                assert_eq!(hit_test(&DESK, &s, p, false), want, "{p:?} in {rect:?}");
            }
            // One pixel above the button is the frame's border, which is
            // title bar and not a button.
            assert_eq!(hit_test(&DESK, &s, at(rect.x0, rect.y0 - 1), false), Hit::TitleBar(0));
        }
    }

    #[test]
    fn the_resize_corner_belongs_to_normal_windows_only() {
        let content = Rect::new(101, 130, 400, 300);
        let mut s = one_window(content);
        let frame = DESK.chrome.frame(content);
        let corner = at(frame.x1 - 2, frame.y1 - 2);
        assert_eq!(hit_test(&DESK, &s, corner, false), Hit::ResizeCorner(0));
        s[0].mode = WindowMode::Maximized;
        assert_eq!(hit_test(&DESK, &s, corner, false), Hit::Content(0));
    }

    #[test]
    fn the_front_window_wins_an_overlap_and_a_minimized_one_never_does() {
        let mut s = one_window(Rect::new(100, 100, 400, 300));
        s.insert(Window::new(1, Rect::new(150, 150, 400, 300), "b".to_string(), Level::Ordinary, CursorStyle::Default));
        let p = at(200, 200);
        assert_eq!(hit_test(&DESK, &s, p, false), Hit::Content(1));
        s[1].minimized = true;
        assert_eq!(hit_test(&DESK, &s, p, false), Hit::Content(0));
    }

    #[test]
    fn the_taskbar_takes_precedence_over_a_window_reaching_into_it() {
        // A window dragged down over the bar: the bar is still clickable, or
        // the last window opened could hide the launcher for good.
        let s = one_window(Rect::new(10, 900, 400, 300));
        let bar = DESK.taskbar(s.len());
        assert_eq!(hit_test(&DESK, &s, at(20, bar.strip().y0 + 4), false), Hit::TaskbarItem(0));
    }

    #[test]
    fn the_bar_past_its_own_buttons_is_desktop() {
        let s = one_window(Rect::new(10, 10, 400, 300));
        let bar = DESK.taskbar(s.len());
        assert_eq!(hit_test(&DESK, &s, at(bar.new_button().x1 + 5, bar.strip().y0 + 4), false), Hit::Desktop);
    }

    #[test]
    fn the_launcher_is_only_hit_while_it_is_open() {
        let s = one_window(Rect::new(10, 10, 400, 300));
        let bar = DESK.taskbar(s.len());
        let item = bar.launcher_item(1);
        let p = at(item.x0 + 4, item.y0 + 4);
        assert_eq!(hit_test(&DESK, &s, p, true), Hit::LauncherItem(1));
        assert_ne!(hit_test(&DESK, &s, p, false), Hit::LauncherItem(1));
    }

    #[test]
    fn an_open_launcher_covers_the_window_beneath_it() {
        let s = one_window(Rect::new(10, 500, 1000, 500));
        let bar = DESK.taskbar(s.len());
        let item = bar.launcher_item(0);
        let p = at(item.x0 + 4, item.y0 + 4);
        assert_eq!(hit_test(&DESK, &s, p, false), Hit::Content(0));
        assert_eq!(hit_test(&DESK, &s, p, true), Hit::LauncherItem(0));
    }

    /// **While a prompt is up a press reaches its content and nothing else**:
    /// not a fullscreen topmost window around it, the taskbar, the open
    /// launcher, nor the prompt's own close button or title bar.
    #[test]
    fn a_hit_outside_the_prompt_s_content_is_swallowed() {
        let mut s: Stack<u32> = Stack::default();
        s.insert(Window::new(0, DESK.work_area(), "hostile".to_string(), Level::Topmost, CursorStyle::Default));
        let ask = Rect::new(700, 400, 400, 200);
        s.insert(Window::new(1, ask, "ask".to_string(), Level::Prompt, CursorStyle::Default));
        let bar = DESK.taskbar(s.len());
        let frame = DESK.chrome.frame(ask);
        let [close, maximize, _] = DESK.chrome.buttons(frame);
        for p in [
            at(10, 10),
            at(bar.new_button().x0 + 2, bar.new_button().y0 + 2),
            at(bar.tab(0).x0 + 2, bar.tab(0).y0 + 2),
            at(bar.launcher_item(0).x0 + 4, bar.launcher_item(0).y0 + 4),
            at(close.x0 + 2, close.y0 + 2),
            at(maximize.x0 + 2, maximize.y0 + 2),
            at(frame.x0 + 40, frame.y0 + 4),
            at(ask.x1, ask.y1),
        ] {
            for launcher in [false, true] {
                assert_eq!(hit_test(&DESK, &s, p, launcher), Hit::Blocked, "{p:?}, launcher open {launcher}");
            }
        }
        for p in [at(ask.x0, ask.y0), at(ask.x1 - 1, ask.y1 - 1), at(900, 500)] {
            assert_eq!(hit_test(&DESK, &s, p, true), Hit::Content(1), "{p:?}");
        }
    }
}
