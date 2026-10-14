//! The pointer's sprite at a scale: its edge, the pixel of it that is the
//! pointer, and whether a display's cursor plane carries it.

use crate::input::CursorStyle;
use crate::rect::{Point, Rect};
use crate::scale::Scale;

/// Every cursor's edge, in logical pixels.
pub const EDGE: i32 = 20;

/// The edge of the image a hardware cursor takes: virtio-gpu's cursor
/// resource is 64x64 (virtio 1.2 §5.7.6.10).
pub const HARDWARE_EDGE: i32 = 64;

/// The edge of every cursor icon's `viewBox`.
const VIEW: u32 = 256;

/// The point of `style`'s icon that is the pointer, in its `viewBox`: the
/// arrow's tip, where the arc that rounds it is furthest up and left, and the
/// centre of the crosshair and of the resize arrow.
const fn hot_spot_in_view(style: CursorStyle) -> (i32, i32) {
    match style {
        CursorStyle::Default => (34, 34),
        CursorStyle::Crosshair | CursorStyle::Resize => (128, 128),
    }
}

/// `style`'s sprite edge at `scale`, in physical pixels.
pub const fn edge(scale: Scale) -> i32 {
    scale.px(EDGE)
}

/// The hot spot of `style`'s sprite drawn at `scale`, from its top left.
pub const fn hot_spot(style: CursorStyle, scale: Scale) -> Point {
    let (x, y) = hot_spot_in_view(style);
    let edge = edge(scale) as u32;
    Point { x: window::scale::round_ratio(x, edge, VIEW), y: window::scale::round_ratio(y, edge, VIEW) }
}

/// Where `style`'s sprite is drawn with the pointer at `at`.
pub const fn drawn(style: CursorStyle, at: Point, scale: Scale) -> Rect {
    let hot = hot_spot(style, scale);
    let edge = edge(scale);
    Rect::new(at.x - hot.x, at.y - hot.y, edge, edge)
}

/// Whether the hardware cursor carries a sprite drawn at `scale`: up to 3.2.
pub const fn in_hardware(scale: Scale) -> bool {
    edge(scale) <= HARDWARE_EDGE
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str) -> Scale {
        Scale::step(text.parse().unwrap()).unwrap()
    }

    /// The arrow's tip is 2.66 pixels in at 20, 3.98 at 30 and 5.31 at 40,
    /// and the crosshair's centre is half the edge.
    #[test]
    fn the_hot_spot_scales_with_the_sprite() {
        for (scale, edge, tip, centre) in [("1", 20, 3, 10), ("1.5", 30, 4, 15), ("2", 40, 5, 20)] {
            let s = at(scale);
            assert_eq!(super::edge(s), edge, "{scale}");
            assert_eq!(hot_spot(CursorStyle::Default, s), Point { x: tip, y: tip }, "{scale}");
            assert_eq!(hot_spot(CursorStyle::Crosshair, s), Point { x: centre, y: centre }, "{scale}");
        }
    }

    #[test]
    fn the_sprite_is_drawn_with_its_hot_spot_on_the_pointer() {
        let s = at("2");
        let r = drawn(CursorStyle::Crosshair, Point { x: 100, y: 50 }, s);
        assert_eq!(r, Rect::new(80, 30, 40, 40));
        let hot = hot_spot(CursorStyle::Crosshair, s);
        assert_eq!(Point { x: r.x0 + hot.x, y: r.y0 + hot.y }, Point { x: 100, y: 50 });
    }

    /// 64 / 20 is 3.2: the hardware plane carries every step to 3, and 3.25
    /// is drawn in software.
    #[test]
    fn the_hardware_cursor_carries_a_sprite_up_to_sixty_four_pixels() {
        assert!(in_hardware(at("3")));
        assert_eq!(edge(at("3")), 60);
        assert!(!in_hardware(at("3.25")));
        assert_eq!(edge(at("3.25")), 65);
    }
}
