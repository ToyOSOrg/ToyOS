use alloc::string::String;

use crate::input::CursorStyle;
use crate::layout::Chrome;
use crate::rect::Rect;

/// Which layer of the stack a window sits in: every window of a higher level
/// is above every window of a lower one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Ordinary,
    /// Its client asked to stay on top: a dialog over the application that
    /// opened it.
    Topmost,
    /// Made through the prompt port, which one program alone holds: above the
    /// taskbar and every other window, at most one at a time, and the one
    /// window with any input while it is up.
    Prompt,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowMode {
    Normal,
    Maximized,
    SnappedLeft,
    SnappedRight,
}

/// A window's identity, stable for as long as it is in a [`Stack`](crate::stack::Stack).
///
/// Its position is not: every close, reorder or dead-client sweep can move a
/// window or remove it, so state that outlives one event-loop pass — a drag,
/// a resize — must not name a window by where it sat when the state began.
/// [`Stack::insert`](crate::stack::Stack::insert) is the only place one is
/// minted, and [`Stack::position`](crate::stack::Stack::position) is how it
/// is turned back into a position, fresh, every time it is needed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WindowId(pub(crate) u64);

impl WindowId {
    /// [`Window::new`]'s placeholder before the window has ever been in a
    /// stack. `Stack::insert` never hands this out — its counter starts at 1
    /// — so a caller can tell an unminted window from a real one.
    pub(crate) const UNASSIGNED: Self = Self(0);
}

/// A window as the desktop reasons about it: geometry, order and state.
///
/// `C` is whatever the compositor needs to *reach* the client — a connection,
/// a pid, a shared mapping, a receive buffer — and nothing here ever looks at
/// it. That is the split: this crate decides where a window is and what it
/// gets, the shell does it. The client rides inside the same value so that
/// reordering the stack cannot desynchronise geometry from the connection it
/// belongs to, which a second parallel list would allow.
pub struct Window<C> {
    /// Assigned by `Stack::insert`; see [`WindowId`].
    pub id: WindowId,
    pub client: C,
    /// The client's own pixels, in screen coordinates.
    pub content: Rect,
    /// The buffer the client actually holds. A drag-resize runs ahead of it:
    /// the window is already bigger than the memory behind it until the
    /// pointer is released.
    pub buf_w: i32,
    pub buf_h: i32,
    pub title: String,
    pub minimized: bool,
    pub level: Level,
    pub mode: WindowMode,
    /// Where a maximized or snapped window goes back to.
    pub saved: Rect,
    /// The client has drawn something not yet composited.
    pub presented: bool,
    pub cursor_style: CursorStyle,
}

impl<C> Window<C> {
    pub fn new(
        client: C,
        content: Rect,
        title: String,
        level: Level,
        cursor_style: CursorStyle,
    ) -> Self {
        Self {
            id: WindowId::UNASSIGNED,
            client,
            content,
            buf_w: content.w(),
            buf_h: content.h(),
            title,
            minimized: false,
            level,
            mode: WindowMode::Normal,
            saved: Rect::EMPTY,
            presented: false,
            cursor_style,
        }
    }

    pub fn frame(&self, chrome: &Chrome) -> Rect {
        chrome.frame(self.content)
    }

    /// Whether everything inside the window's frame comes from this window.
    ///
    /// False while a drag-resize is ahead of the buffer the client was given:
    /// the content blit is clipped to the buffer, so the rest of the frame is
    /// whatever was under it and the wallpaper below still has to be composed.
    pub fn is_opaque(&self) -> bool {
        !self.minimized && self.content.w() <= self.buf_w && self.content.h() <= self.buf_h
    }

    /// The part of the content the client's buffer actually backs.
    pub fn backed(&self) -> Rect {
        Rect::new(
            self.content.x0,
            self.content.y0,
            self.content.w().min(self.buf_w),
            self.content.h().min(self.buf_h),
        )
    }

    /// Remember where to come back to, if this is the first departure from
    /// [`WindowMode::Normal`].
    pub fn save_if_normal(&mut self) {
        if self.mode == WindowMode::Normal {
            self.saved = self.content;
        }
    }

    /// Where a client's own damage claim lands on the screen.
    ///
    /// `claim` is in the window's own pixels and is the payload of
    /// `MSG_PRESENT`. It is clamped to the window rather than believed: a bad
    /// one is a client scribbling on the desktop, and there is no reading of
    /// it that is worth a repaint of the screen.
    pub fn present_damage(&self, claim: Rect) -> Rect {
        claim
            .intersect(Rect::new(0, 0, self.content.w(), self.content.h()))
            .translate(self.content.x0, self.content.y0)
    }
}

/// A window's buffers: the one it is shown from, and at most one given its
/// client since, each named by the generation the client presents it with.
///
/// **Nothing flashes**: until the client presents with the given buffer's
/// generation the window is drawn from the buffer it last presented, clipped
/// to its new rectangle. **A window holds at most two**: while one is given
/// and not presented no other is given; a window wanting another meanwhile
/// is owed it, and given it once it presents the outstanding one.
pub struct Buffers<B> {
    shown: (u32, B),
    given: Option<(u32, B)>,
    next: u32,
    owed: bool,
}

/// What a client's present with a generation did.
#[derive(Debug, PartialEq, Eq)]
pub enum Presented<B> {
    /// It drew into the buffer already shown.
    Shown,
    /// It drew into the given buffer, shown from now on; `old` is the one it
    /// replaces, to drop, and `owed` whether the window wants another.
    Swapped { old: B, owed: bool },
    /// No buffer the window holds has this generation: not this protocol.
    Unknown,
}

impl<B> Buffers<B> {
    /// `shown` at generation 0.
    pub fn new(shown: B) -> Self {
        Self { shown: (0, shown), given: None, next: 1, owed: false }
    }

    pub fn shown(&self) -> &B {
        &self.shown.1
    }

    /// The buffer the client draws into: the given one, if there is one.
    pub fn drawn(&self) -> &B {
        self.given.as_ref().map_or(&self.shown.1, |(_, b)| b)
    }

    /// Whether a buffer is given and not yet presented, in which case the
    /// window is owed the one it wanted and nothing is given now.
    pub fn owe(&mut self) -> bool {
        self.owed |= self.given.is_some();
        self.given.is_some()
    }

    /// Give the client `buffer`, and answer its generation. One given and
    /// not presented is the caller's to have asked [`owe`](Self::owe) about.
    pub fn give(&mut self, buffer: B) -> u32 {
        assert!(self.given.is_none(), "a window holds at most two buffers");
        let generation = self.next;
        self.next = self.next.wrapping_add(1);
        self.given = Some((generation, buffer));
        generation
    }

    /// The client presented, drawn into the buffer of `generation`.
    pub fn presented(&mut self, generation: u32) -> Presented<B> {
        if generation == self.shown.0 {
            return Presented::Shown;
        }
        match self.given.take() {
            Some(given) if given.0 == generation => {
                let old = core::mem::replace(&mut self.shown, given).1;
                Presented::Swapped { old, owed: core::mem::take(&mut self.owed) }
            }
            given => {
                self.given = given;
                Presented::Unknown
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    #[test]
    fn a_window_is_shown_from_its_old_buffer_until_it_presents_into_the_new() {
        let mut b = Buffers::new("old");
        let new = b.give("new");
        assert_eq!((*b.shown(), *b.drawn()), ("old", "new"));
        assert_eq!(b.presented(new), Presented::Swapped { old: "old", owed: false });
        assert_eq!((*b.shown(), *b.drawn()), ("new", "new"));
        assert_eq!(b.presented(new), Presented::Shown);
    }

    /// A present drawn into the old buffer after the new one was given, as a
    /// client mid-frame sends, shows the old buffer still: the new one was
    /// never drawn into.
    #[test]
    fn a_present_into_the_old_buffer_does_not_swap() {
        let mut b = Buffers::new("old");
        let new = b.give("new");
        assert_eq!(b.presented(0), Presented::Shown);
        assert_eq!(*b.shown(), "old");
        assert_eq!(b.presented(new), Presented::Swapped { old: "old", owed: false });
    }

    #[test]
    fn a_generation_the_window_never_held_is_unknown() {
        let mut b = Buffers::new("old");
        let new = b.give("new");
        assert_eq!(b.presented(new + 1), Presented::Unknown);
        assert_eq!(*b.drawn(), "new");
    }

    /// Two resizes before a present: the second is owed and not given, so a
    /// window holds two buffers and not three, and is given the one it wants
    /// once it presents the first.
    #[test]
    fn a_second_buffer_is_owed_while_one_is_outstanding() {
        let mut b = Buffers::new("old");
        assert!(!b.owe());
        let first = b.give("first");
        assert!(b.owe());
        assert_eq!(*b.drawn(), "first");
        assert_eq!(b.presented(first), Presented::Swapped { old: "old", owed: true });
        assert!(!b.owe());
        let second = b.give("second");
        assert_eq!(b.presented(second), Presented::Swapped { old: "first", owed: false });
    }

    fn win(content: Rect) -> Window<()> {
        Window::new((), content, "t".to_string(), Level::Ordinary, CursorStyle::Default)
    }

    #[test]
    fn a_client_cannot_damage_more_than_its_own_window() {
        let w = win(Rect::new(100, 200, 400, 300));
        assert_eq!(w.present_damage(Rect::new(0, 0, 10, 10)), Rect::new(100, 200, 10, 10));
        assert_eq!(
            w.present_damage(Rect::from_wire(0, 0, u32::MAX, u32::MAX)),
            Rect::new(100, 200, 400, 300)
        );
        assert!(w.present_damage(Rect::new(500, 500, 10, 10)).is_empty());
        assert!(w.present_damage(Rect::from_wire(u32::MAX, u32::MAX, 1, 1)).is_empty());
    }

    #[test]
    fn a_window_mid_resize_is_not_opaque_and_its_backing_is_the_buffer() {
        let mut w = win(Rect::new(10, 10, 400, 300));
        assert!(w.is_opaque());
        w.content = Rect::new(10, 10, 600, 300);
        assert!(!w.is_opaque());
        assert_eq!(w.backed(), Rect::new(10, 10, 400, 300));
    }

    #[test]
    fn a_window_shrunk_below_its_buffer_is_still_opaque() {
        let mut w = win(Rect::new(10, 10, 400, 300));
        w.content = Rect::new(10, 10, 200, 100);
        assert!(w.is_opaque());
        assert_eq!(w.backed(), w.content);
    }

    #[test]
    fn a_minimized_window_is_never_opaque() {
        let mut w = win(Rect::new(10, 10, 400, 300));
        w.minimized = true;
        assert!(!w.is_opaque());
    }

    #[test]
    fn only_the_first_departure_from_normal_is_saved() {
        let mut w = win(Rect::new(10, 10, 400, 300));
        w.save_if_normal();
        assert_eq!(w.saved, Rect::new(10, 10, 400, 300));
        w.mode = WindowMode::Maximized;
        w.content = Rect::new(0, 0, 1920, 1000);
        w.save_if_normal();
        assert_eq!(w.saved, Rect::new(10, 10, 400, 300));
    }
}
