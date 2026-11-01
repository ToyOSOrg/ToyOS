//! The desktop's icons: an SVG in the subset they are written in, its
//! coverage at any edge, and that coverage blended onto a canvas in one
//! colour.
//!
//! The SVG is read as the subset the icons are written in, and anything past it
//! is refused by name: a root `svg` with a square `viewBox`, then `path`,
//! `line`, `circle` and `rect`, each painted `currentColor` or `none`. A literal
//! colour is refused because the mask cannot carry it; an attribute this does
//! not read is refused because it would ship drawn without it. A stroke must be
//! round-capped and round-joined, which makes it exactly the points within half
//! its width of its path, the region drawn here. **[`Icon::parse`] panics on a
//! refusal**: the build parses every icon an image ships, so a program reading
//! one off its image never meets one.
//!
//! The pixels follow resvg's, which drew these icons before ToyOS did,
//! because the desktop's look is not this file's to change: a pixel
//! samples four rows of four points, a sample worth 16 of 255; a fill is
//! nonzero, its arcs become cubics as `kurbo` splits them and each cubic, cut
//! where it turns in y, becomes `2^shift` chords by tiny-skia's count; and each
//! shape is laid over the ones before it. The transcendental functions are
//! `libm`'s, so a host and a guest draw the same pixels.

/// Sample points per pixel along each axis.
const GRID: usize = 4;

type Point = (f64, f64);

/// An icon's coverage at one edge: one byte per pixel, row by row, 255 where
/// the icon covers the pixel whole.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mask {
    edge: usize,
    alpha: Vec<u8>,
}

/// What a [`Mask`] is drawn onto: pixels read and written as red, green and
/// blue, each place inside the canvas.
pub trait Canvas {
    fn size(&self) -> (usize, usize);
    fn get(&self, x: usize, y: usize) -> [u8; 3];
    fn put(&self, x: usize, y: usize, rgb: [u8; 3]);
}

impl Mask {
    pub fn edge(&self) -> usize {
        self.edge
    }

    pub fn alpha(&self) -> &[u8] {
        &self.alpha
    }

    /// Blended onto `canvas` in `color`, its top-left corner at (`x`, `y`),
    /// clipped to the canvas on every side.
    pub fn draw(&self, canvas: &impl Canvas, x: i64, y: i64, color: [u8; 3]) {
        let (w, h) = canvas.size();
        for (i, &alpha) in self.alpha.iter().enumerate() {
            let (px, py) = (x + (i % self.edge) as i64, y + (i / self.edge) as i64);
            if alpha == 0 || px < 0 || py < 0 || px >= w as i64 || py >= h as i64 {
                continue;
            }
            let (px, py) = (px as usize, py as usize);
            canvas.put(px, py, over(color, canvas.get(px, py), alpha));
        }
    }
}

/// `color` laid over `under` at `alpha` of 255.
fn over(color: [u8; 3], under: [u8; 3], alpha: u8) -> [u8; 3] {
    let (a, inv) = (u16::from(alpha), u16::from(255 - alpha));
    core::array::from_fn(|i| ((u16::from(color[i]) * a + u16::from(under[i]) * inv) / 255) as u8)
}

/// A subpath: its start, then one cubic per segment, a line as the cubic
/// that runs along it.
#[derive(Clone)]
struct Subpath {
    start: Point,
    cubics: Vec<[Point; 3]>,
    closed: bool,
}

struct Shape {
    subpaths: Vec<Subpath>,
    fill: bool,
    /// Half the stroke's width; `None` where it is not stroked.
    stroke: Option<f64>,
}

pub struct Icon {
    /// The `viewBox`'s origin and edge, in user units.
    origin: Point,
    edge: f64,
    shapes: Vec<Shape>,
}

impl Icon {
    /// `svg`'s shapes; panics naming what of it is past the subset.
    pub fn parse(svg: &str) -> Icon {
        let mut tags = Tags { rest: svg.trim() };
        let (name, attrs, open) = tags.next().expect("an empty SVG");
        assert!(name == "svg" && open, "the root element is <{name}>, not an open <svg>");
        let mut root_fill = None;
        let mut view_box = None;
        for (key, value) in attrs {
            match key {
                "xmlns" => assert_eq!(value, "http://www.w3.org/2000/svg", "xmlns"),
                "viewBox" => view_box = Some(numbers(value)),
                "fill" => root_fill = Some(paint(value)),
                _ => panic!("<svg {key}> is not read"),
            }
        }
        let view_box = view_box.expect("an <svg> with no viewBox");
        let [x, y, w, h] = view_box[..] else { panic!("viewBox {view_box:?} is not four numbers") };
        assert!(w == h && w > 0.0, "viewBox {view_box:?} is not square");

        let mut shapes = vec![];
        loop {
            let (name, attrs, open) = tags.next().expect("an <svg> that never closes");
            if name == "/svg" {
                break;
            }
            assert!(!open, "<{name}> has children");
            shapes.push(Shape::parse(name, &attrs, root_fill));
        }
        assert!(tags.rest.trim().is_empty(), "text after </svg>: {:?}", tags.rest);
        Icon { origin: (x, y), edge: w, shapes }
    }
}

impl Shape {
    fn parse(name: &str, attrs: &[(&str, &str)], root_fill: Option<bool>) -> Shape {
        let mut fill = root_fill;
        let mut stroke = false;
        let mut width = 1.0;
        let mut round = (false, false);
        let mut path = Path::default();
        let num = |value: &str| -> f64 { value.parse().unwrap_or_else(|_| panic!("<{name}> {value:?} is not a number")) };
        let get = |key: &str| attrs.iter().find(|(k, _)| *k == key).map(|(_, v)| num(v));
        let geometry: &[&str] = match name {
            "path" => {
                let d = attrs.iter().find(|(k, _)| *k == "d").expect("a <path> with no d").1;
                path.data(d);
                &["d"]
            }
            "line" => {
                let at = |k| get(k).unwrap_or(0.0);
                path.move_to((at("x1"), at("y1")));
                path.line_to((at("x2"), at("y2")));
                &["x1", "y1", "x2", "y2"]
            }
            "circle" => {
                let (x, y, r) = (get("cx").unwrap_or(0.0), get("cy").unwrap_or(0.0), get("r").expect("r"));
                path.move_to((x + r, y));
                for to in [(x, y + r), (x - r, y), (x, y - r), (x + r, y)] {
                    path.arc(r, r, 0.0, false, true, to);
                }
                path.close();
                &["cx", "cy", "r"]
            }
            "rect" => {
                let (x, y) = (get("x").unwrap_or(0.0), get("y").unwrap_or(0.0));
                let (w, h) = (get("width").expect("width"), get("height").expect("height"));
                let (rx, ry) = match (get("rx"), get("ry")) {
                    (None, None) => (0.0, 0.0),
                    (Some(r), None) | (None, Some(r)) => (r, r),
                    (Some(rx), Some(ry)) => (rx, ry),
                };
                let (rx, ry) = (rx.min(w / 2.0), ry.min(h / 2.0));
                path.move_to((x + rx, y));
                path.line_to((x + w - rx, y));
                path.arc(rx, ry, 0.0, false, true, (x + w, y + ry));
                path.line_to((x + w, y + h - ry));
                path.arc(rx, ry, 0.0, false, true, (x + w - rx, y + h));
                path.line_to((x + rx, y + h));
                path.arc(rx, ry, 0.0, false, true, (x, y + h - ry));
                path.line_to((x, y + ry));
                path.arc(rx, ry, 0.0, false, true, (x + rx, y));
                path.close();
                &["x", "y", "width", "height", "rx", "ry"]
            }
            _ => panic!("<{name}> is not drawn"),
        };
        for &(key, value) in attrs {
            match key {
                "fill" => fill = Some(paint(value)),
                "stroke" => stroke = paint(value),
                "stroke-width" => width = num(value),
                "stroke-linecap" => round.0 = value == "round",
                "stroke-linejoin" => round.1 = value == "round",
                _ if geometry.contains(&key) => {}
                _ => panic!("<{name} {key}> is not read"),
            }
        }
        let fill = fill.unwrap_or_else(|| panic!("<{name}> fills black, a colour no mask carries"));
        assert!(!stroke || round == (true, true), "<{name}> is stroked without round caps and joins");
        Shape { subpaths: path.finish(), fill, stroke: stroke.then_some(width / 2.0) }
    }
}

/// Whether a paint draws: `currentColor` does, `none` does not.
fn paint(value: &str) -> bool {
    match value {
        "currentColor" => true,
        "none" => false,
        _ => panic!("paint {value:?} is neither currentColor nor none"),
    }
}

fn numbers(value: &str) -> Vec<f64> {
    let mut data = Data { rest: value.as_bytes() };
    std::iter::from_fn(|| data.number()).collect()
}

/// An SVG element: its name, its attributes and whether it opens.
type Tag<'a> = (&'a str, Vec<(&'a str, &'a str)>, bool);

/// An SVG's elements, one [`Tag`] at a time.
struct Tags<'a> {
    rest: &'a str,
}

impl<'a> Tags<'a> {
    fn next(&mut self) -> Option<Tag<'a>> {
        self.rest = self.rest.trim_start();
        if self.rest.is_empty() {
            return None;
        }
        let end = self.rest.find('>').unwrap_or_else(|| panic!("an unclosed tag: {:?}", self.rest));
        let tag = self.rest[..end].strip_prefix('<').unwrap_or_else(|| panic!("text where a tag goes: {:?}", self.rest));
        self.rest = &self.rest[end + 1..];
        let (tag, open) = match tag.strip_suffix('/') {
            Some(tag) => (tag, false),
            None => (tag, !tag.starts_with('/')),
        };
        let tag = tag.trim();
        let (name, mut attrs) = tag.split_once(char::is_whitespace).unwrap_or((tag, ""));
        let mut out = vec![];
        loop {
            attrs = attrs.trim_start();
            if attrs.is_empty() {
                break;
            }
            let (key, rest) = attrs.split_once("=\"").unwrap_or_else(|| panic!("<{name}>: {attrs:?}"));
            let (value, rest) = rest.split_once('"').unwrap_or_else(|| panic!("<{name}>: {attrs:?}"));
            out.push((key.trim(), value));
            attrs = rest;
        }
        Some((name, out, open))
    }
}

/// Path data, read one number or command at a time.
struct Data<'a> {
    rest: &'a [u8],
}

impl Data<'_> {
    fn skip(&mut self) {
        while let [b' ' | b'\t' | b'\n' | b'\r' | b',', rest @ ..] = self.rest {
            self.rest = rest;
        }
    }

    fn command(&mut self) -> Option<u8> {
        self.skip();
        match self.rest {
            [c, rest @ ..] if c.is_ascii_alphabetic() => {
                self.rest = rest;
                Some(*c)
            }
            _ => None,
        }
    }

    fn number(&mut self) -> Option<f64> {
        self.skip();
        let mut end = 0;
        let digits = |at: &mut usize, rest: &[u8]| {
            while rest.get(*at).is_some_and(u8::is_ascii_digit) {
                *at += 1;
            }
        };
        if matches!(self.rest.first(), Some(b'+' | b'-')) {
            end += 1;
        }
        digits(&mut end, self.rest);
        if self.rest.get(end) == Some(&b'.') {
            end += 1;
            digits(&mut end, self.rest);
        }
        if matches!(self.rest.get(end), Some(b'e' | b'E')) {
            end += 1;
            if matches!(self.rest.get(end), Some(b'+' | b'-')) {
                end += 1;
            }
            digits(&mut end, self.rest);
        }
        if end == 0 {
            return None;
        }
        let text = std::str::from_utf8(&self.rest[..end]).unwrap();
        self.rest = &self.rest[end..];
        Some(text.parse().unwrap_or_else(|_| panic!("{text:?} is not a number")))
    }

    fn flag(&mut self) -> bool {
        self.skip();
        match self.rest {
            [c @ (b'0' | b'1'), rest @ ..] => {
                self.rest = rest;
                *c == b'1'
            }
            _ => panic!("an arc flag that is not 0 or 1"),
        }
    }

    fn point(&mut self) -> Point {
        let x = self.number().expect("a coordinate");
        (x, self.number().expect("a coordinate"))
    }
}

/// A shape's subpaths as they are read.
#[derive(Default)]
struct Path {
    done: Vec<Subpath>,
    open: Option<Subpath>,
}

impl Path {
    fn at(&self) -> Point {
        let open = self.open.as_ref().expect("a drawing command before any moveto");
        open.cubics.last().map_or(open.start, |c| c[2])
    }

    /// Ends the current subpath; a lone moveto draws nothing.
    fn end(&mut self, closed: bool) {
        if let Some(open) = self.open.take().filter(|open| !open.cubics.is_empty()) {
            self.done.push(Subpath { closed, ..open });
        }
    }

    fn finish(mut self) -> Vec<Subpath> {
        self.end(false);
        self.done
    }

    fn move_to(&mut self, p: Point) {
        self.end(false);
        self.open = Some(Subpath { start: p, cubics: vec![], closed: false });
    }

    fn cubic_to(&mut self, c1: Point, c2: Point, p: Point) {
        self.at();
        self.open.as_mut().unwrap().cubics.push([c1, c2, p]);
    }

    fn line_to(&mut self, p: Point) {
        let a = self.at();
        let third = |t: f64| (a.0 + (p.0 - a.0) * t, a.1 + (p.1 - a.1) * t);
        self.cubic_to(third(1.0 / 3.0), third(2.0 / 3.0), p);
    }

    fn close(&mut self) {
        let start = self.open.as_ref().expect("a closepath before any moveto").start;
        self.end(true);
        self.open = Some(Subpath { start, cubics: vec![], closed: false });
    }

    /// An elliptical arc: its centre by the SVG specification's conversion
    /// (SVG 1.1, appendix F.6.5 and F.6.6), then the cubics `kurbo` splits it
    /// into at the 0.1 user units usvg asks of it.
    fn arc(&mut self, rx: f64, ry: f64, degrees: f64, large: bool, sweep: bool, p: Point) {
        let p0 = self.at();
        let (mut rx, mut ry) = (rx.abs(), ry.abs());
        if p0 == p {
            return;
        }
        if rx <= 1e-5 || ry <= 1e-5 {
            return self.line_to(p);
        }
        let (sin, cos) = libm::sincos(degrees.to_radians());
        let (hx, hy) = ((p0.0 - p.0) / 2.0, (p0.1 - p.1) / 2.0);
        let (x1, y1) = (cos * hx + sin * hy, -sin * hx + cos * hy);
        let lambda = (x1 * x1) / (rx * rx) + (y1 * y1) / (ry * ry);
        if lambda > 1.0 {
            rx *= lambda.sqrt();
            ry *= lambda.sqrt();
        }
        let num = rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1;
        let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
        let k = (num / den).abs().sqrt() * if large == sweep { -1.0 } else { 1.0 };
        let (cx1, cy1) = (k * rx * y1 / ry, -k * ry * x1 / rx);
        let centre = (cos * cx1 - sin * cy1 + f64::midpoint(p0.0, p.0), sin * cx1 + cos * cy1 + f64::midpoint(p0.1, p.1));
        let start = libm::atan2((y1 - cy1) / ry, (x1 - cx1) / rx);
        let tau = std::f64::consts::TAU;
        let mut delta = (libm::atan2((-y1 - cy1) / ry, (-x1 - cx1) / rx) - start) % tau;
        if sweep && delta < 0.0 {
            delta += tau;
        } else if !sweep && delta > 0.0 {
            delta -= tau;
        }

        let n = (libm::pow(1.1163 * rx.max(ry) / 0.1, 1.0 / 6.0).max(3.999_999) * delta.abs() / tau).ceil();
        let step = delta / n;
        let arm = 4.0 / 3.0 * libm::tan(step / 4.0);
        let at = |angle: f64| {
            let (s, c) = libm::sincos(angle);
            (rx * cos * c - ry * sin * s, rx * sin * c + ry * cos * s)
        };
        for i in 0..n as usize {
            let (a0, a1) = (start + step * i as f64, start + step * (i + 1) as f64);
            let (q0, d0) = (at(a0), at(a0 + tau / 4.0));
            let (q1, d1) = (at(a1), at(a1 + tau / 4.0));
            let c1 = (centre.0 + q0.0 + arm * d0.0, centre.1 + q0.1 + arm * d0.1);
            let c2 = (centre.0 + q1.0 - arm * d1.0, centre.1 + q1.1 - arm * d1.1);
            let end = if i + 1 == n as usize { p } else { (centre.0 + q1.0, centre.1 + q1.1) };
            self.cubic_to(c1, c2, end);
        }
    }

    fn data(&mut self, d: &str) {
        let mut data = Data { rest: d.as_bytes() };
        let mut command = data.command().expect("path data that does not start with a command");
        loop {
            let at = self.open.as_ref().map_or((0.0, 0.0), |_| self.at());
            let relative = command.is_ascii_lowercase();
            let abs = |p: Point| if relative { (at.0 + p.0, at.1 + p.1) } else { p };
            match command.to_ascii_uppercase() {
                b'M' => {
                    // A relative one after a closepath is from the closed
                    // subpath's start, which `close` leaves as the current point.
                    self.move_to(abs(data.point()));
                    // Coordinates after a moveto are linetos.
                    command = if relative { b'l' } else { b'L' };
                }
                b'L' => self.line_to(abs(data.point())),
                b'H' => {
                    let x = data.number().expect("an x");
                    self.line_to((if relative { at.0 + x } else { x }, at.1));
                }
                b'V' => {
                    let y = data.number().expect("a y");
                    self.line_to((at.0, if relative { at.1 + y } else { y }));
                }
                b'C' => {
                    let (c1, c2) = (abs(data.point()), abs(data.point()));
                    let p = abs(data.point());
                    self.cubic_to(c1, c2, p);
                }
                b'A' => {
                    let (rx, ry) = (data.number().expect("rx"), data.number().expect("ry"));
                    let degrees = data.number().expect("an x-axis rotation");
                    let (large, sweep) = (data.flag(), data.flag());
                    let p = abs(data.point());
                    self.arc(rx, ry, degrees, large, sweep, p);
                }
                b'Z' => self.close(),
                _ => panic!("path command {:?} is not read", command as char),
            }
            if let Some(next) = data.command() {
                command = next;
            } else if data.rest.is_empty() {
                break;
            } else if command.eq_ignore_ascii_case(&b'Z') {
                panic!("a number after a closepath");
            }
        }
    }
}

impl Subpath {
    fn map(&self, f: impl Fn(Point) -> Point) -> Subpath {
        Subpath {
            start: f(self.start),
            cubics: self.cubics.iter().map(|c| c.map(&f)).collect(),
            closed: self.closed,
        }
    }

    /// The subpath as chords, each cubic cut into as many as `chords` says.
    fn flatten(&self, chords: impl Fn(&[Point; 4]) -> Vec<Point>) -> Vec<Point> {
        let mut points = vec![self.start];
        for c in &self.cubics {
            let p0 = *points.last().unwrap();
            points.extend(chords(&[p0, c[0], c[1], c[2]]));
        }
        points
    }
}

fn eval(c: &[Point; 4], t: f64) -> Point {
    let u = 1.0 - t;
    let (a, b, d, e) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
    (a * c[0].0 + b * c[1].0 + d * c[2].0 + e * c[3].0, a * c[0].1 + b * c[1].1 + d * c[2].1 + e * c[3].1)
}

fn derivative(c: &[Point; 4], t: f64) -> Point {
    let u = 1.0 - t;
    let (a, b, d) = (3.0 * u * u, 6.0 * u * t, 3.0 * t * t);
    (
        a * (c[1].0 - c[0].0) + b * (c[2].0 - c[1].0) + d * (c[3].0 - c[2].0),
        a * (c[1].1 - c[0].1) + b * (c[2].1 - c[1].1) + d * (c[3].1 - c[2].1),
    )
}

/// A device coordinate in tiny-skia's fixed point: 1/64ths of a sample.
fn dot6(v: f64) -> i64 {
    (v * (GRID * 64) as f64) as i64
}

/// A stroke's path, as many chords as tiny-skia's outline of it has: the
/// offset `r` out from the cubic in quads, each halved until its middle is
/// within a quarter of a pixel of the offset's, then each quad in as many
/// chords as tiny-skia steps it in.
fn stroked(c: &[Point; 4], r: f64) -> Vec<Point> {
    let tangent = |t: f64| {
        let (x, y) = derivative(c, t);
        let length = libm::hypot(x, y);
        assert!(length > 0.0, "a stroked curve with no direction at {t}");
        (x / length, y / length)
    };
    let offset = |t: f64| {
        let (p, d) = (eval(c, t), tangent(t));
        (p.0 - d.1 * r, p.1 + d.0 * r)
    };
    let mut out = vec![];
    let mut pieces = vec![(0.0, 1.0)];
    while let Some((t0, t1)) = pieces.pop() {
        let (o0, o1, d0, d1) = (offset(t0), offset(t1), tangent(t0), tangent(t1));
        let cross = d0.0 * d1.1 - d0.1 * d1.0;
        if cross.abs() < 1e-9 {
            // Straight: one chord.
            out.push((t1, 1));
            continue;
        }
        // Where the offset's end tangents meet: the quad's control point.
        let s = ((o1.0 - o0.0) * d1.1 - (o1.1 - o0.1) * d1.0) / cross;
        let q = (o0.0 + d0.0 * s, o0.1 + d0.1 * s);
        let mid = ((o0.0 + 2.0 * q.0 + o1.0) / 4.0, (o0.1 + 2.0 * q.1 + o1.1) / 4.0);
        let half = f64::midpoint(t0, t1);
        let want = offset(half);
        if libm::hypot(mid.0 - want.0, mid.1 - want.1) > 0.25 {
            pieces.push((half, t1));
            pieces.push((t0, half));
            continue;
        }
        let bend = |a: f64, b: f64, c: f64| (2 * dot6(b) - dot6(a) - dot6(c)) >> 2;
        out.push((t1, chords(bend(o0.0, q.0, o1.0), bend(o0.1, q.1, o1.1), 0)));
    }
    let mut points = vec![];
    let mut t0 = 0.0;
    for (t1, n) in out {
        points.extend((1..=n).map(|i| eval(c, t0 + (t1 - t0) * i as f64 / n as f64)));
        t0 = t1;
    }
    points
}

/// How many chords tiny-skia steps a curve in, from how far it strays from
/// its chord in each axis, in 1/64ths of a sample: `2^(shift + extra)`, at
/// least two and at most 64.
fn chords(dx: i64, dy: i64, extra: i64) -> usize {
    let (dx, dy) = (dx.abs(), dy.abs());
    let distance = (dx.max(dy) + (dx.min(dy) >> 1) + 16) >> 5;
    let shift = ((64 - distance.leading_zeros() as i64) >> 1) + extra;
    1 << shift.clamp(1, 6)
}

/// A fill's edges as tiny-skia steps them: the cubic cut where it turns in y,
/// and each piece in `2^shift` chords, `shift` from how far its control points
/// stray from its chord in 1/64ths of a sample.
fn stepped(c: &[Point; 4]) -> Vec<Point> {
    let (a, b, d) = (c[1].1 - c[0].1, c[2].1 - c[1].1, c[3].1 - c[2].1);
    // Where dy/dt, `(a - 2b + d)t^2 + 2(b - a)t + a`, is zero.
    let (qa, qb) = (a - 2.0 * b + d, 2.0 * (b - a));
    let mut turns: Vec<f64> = if qa.abs() < 1e-12 {
        if qb != 0.0 { vec![-a / qb] } else { vec![] }
    } else {
        let disc = qb * qb - 4.0 * qa * a;
        if disc < 0.0 { vec![] } else { [-1.0, 1.0].map(|s| (-qb + s * disc.sqrt()) / (2.0 * qa)).to_vec() }
    };
    turns.retain(|t| *t > 0.0 && *t < 1.0);
    turns.sort_by(f64::total_cmp);
    turns.dedup();
    let mut bounds = vec![0.0];
    bounds.extend(turns);
    bounds.push(1.0);

    let mut out = vec![];
    for piece in bounds.windows(2) {
        let (t0, t1) = (piece[0], piece[1]);
        let at = |s: f64| eval(c, t0 + (t1 - t0) * s);
        // The piece's own control points, by its end tangents.
        let span = t1 - t0;
        let (p0, p3) = (at(0.0), at(1.0));
        let (d0, d3) = (derivative(c, t0), derivative(c, t1));
        let p1 = (p0.0 + d0.0 * span / 3.0, p0.1 + d0.1 * span / 3.0);
        let p2 = (p3.0 - d3.0 * span / 3.0, p3.1 - d3.1 * span / 3.0);
        // How far the curve at 1/3 and 2/3 strays from the control points
        // there, with 19/512 for 1/27.
        let delta = |a: f64, b: f64, c: f64, d: f64| {
            let (a, b, c, d) = (dot6(a), dot6(b), dot6(c), dot6(d));
            let third = ((a * 8 - b * 15 + 6 * c + d) * 19) >> 9;
            let two_thirds = ((a + 6 * b - c * 15 + d * 8) * 19) >> 9;
            third.abs().max(two_thirds.abs())
        };
        let n = chords(delta(p0.0, p1.0, p2.0, p3.0), delta(p0.1, p1.1, p2.1, p3.1), 1);
        out.extend((1..=n).map(|i| at(i as f64 / n as f64)));
    }
    out
}

impl Icon {
    /// The icon drawn `edge` pixels square.
    pub fn mask(&self, edge: usize) -> Mask {
        Mask { edge, alpha: coverage(self, edge) }
    }
}

/// Each pixel's coverage, `size` by `size`, row by row.
fn coverage(icon: &Icon, size: usize) -> Vec<u8> {
    let scale = size as f64 / icon.edge;
    let device = |(x, y): Point| ((x - icon.origin.0) * scale, (y - icon.origin.1) * scale);

    let mut out = vec![0u8; size * size];
    let mut spans: Vec<(f64, f64)> = vec![];
    let mut crossings: Vec<(f64, i32)> = vec![];
    for shape in &icon.shapes {
        let subpaths: Vec<Subpath> = shape.subpaths.iter().map(|s| s.map(device)).collect();
        let fills: Vec<Vec<Point>> = if shape.fill { subpaths.iter().map(|s| s.flatten(stepped)).collect() } else { vec![] };
        let r = shape.stroke.unwrap_or(0.0) * scale;
        let strokes: Vec<(Vec<Point>, bool)> = match shape.stroke {
            Some(_) => subpaths.iter().map(|s| (s.flatten(|c| stroked(c, r)), s.closed)).collect(),
            None => vec![],
        };

        let mut alpha = vec![0u32; size * size];
        for row in 0..size * GRID {
            let y = (row as f64 + 0.5) / GRID as f64;
            spans.clear();
            crossings.clear();
            for points in &fills {
                for (a, b) in edges(points, true) {
                    if (a.1 < y && y <= b.1) || (b.1 < y && y <= a.1) {
                        let x = a.0 + (y - a.1) * (b.0 - a.0) / (b.1 - a.1);
                        crossings.push((x, if b.1 > a.1 { 1 } else { -1 }));
                    }
                }
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut winding = 0;
            for &(x, dir) in &crossings {
                if winding == 0 {
                    spans.push((x, f64::INFINITY));
                }
                winding += dir;
                if winding == 0 {
                    spans.last_mut().unwrap().1 = x;
                }
            }
            for (points, closed) in &strokes {
                for (a, b) in edges(points, *closed) {
                    spans.extend(capsule(a, b, r, y));
                }
            }
            spans.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut counts = vec![0u32; size];
            let mut i = 0;
            while i < spans.len() {
                let (a, mut b) = spans[i];
                i += 1;
                while i < spans.len() && spans[i].0 <= b {
                    b = b.max(spans[i].1);
                    i += 1;
                }
                // The sample points `(column + 0.5) / GRID` in `a < x <= b`.
                let first = ((a * GRID as f64 - 0.5).floor() + 1.0).max(0.0);
                let last = (b * GRID as f64 - 0.5).floor().min((size * GRID) as f64 - 1.0);
                if first <= last {
                    for column in first as usize..=last as usize {
                        counts[column / GRID] += 1;
                    }
                }
            }
            // A row of four is worth 64, but 63 in a pixel's last row, so that
            // a covered pixel sums to 255 and not 256.
            let full = if row % GRID == GRID - 1 { 63 } else { 64 };
            let pixels = &mut alpha[(row / GRID) * size..][..size];
            for (pixel, &n) in pixels.iter_mut().zip(&counts) {
                *pixel += if n == GRID as u32 { full } else { n * 16 };
            }
        }
        for (o, &a) in out.iter_mut().zip(&alpha) {
            *o = ((255 * a + *o as u32 * (255 - a) + 255) >> 8) as u8;
        }
    }
    out
}

/// A polyline's segments, with the closing one where `closed`.
fn edges(points: &[Point], closed: bool) -> impl Iterator<Item = (Point, Point)> + '_ {
    let closing = (closed && points.len() > 1).then(|| (points[points.len() - 1], points[0]));
    points.windows(2).map(|w| (w[0], w[1])).chain(closing)
}

/// Where the row `y` crosses the points within `r` of the segment `a`-`b`.
fn capsule(a: Point, b: Point, r: f64, y: f64) -> Option<(f64, f64)> {
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for c in [a, b] {
        let dy = y - c.1;
        if dy.abs() <= r {
            let h = (r * r - dy * dy).sqrt();
            lo = lo.min(c.0 - h);
            hi = hi.max(c.0 + h);
        }
    }
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let length = libm::hypot(dx, dy);
    if length > 0.0 {
        // Along the segment, 0..=1, and across it, -r..=r, are each linear in x.
        let along = solve(dx / (length * length), (y - a.1) * dy / (length * length), 0.0, 1.0);
        let across = solve(dy / length, -(y - a.1) * dx / length, -r, r);
        if let (Some(p), Some(q)) = (along, across) {
            let (from, to) = (p.0.max(q.0), p.1.min(q.1));
            if from <= to {
                lo = lo.min(from + a.0);
                hi = hi.max(to + a.0);
            }
        }
    }
    (lo <= hi).then_some((lo, hi))
}

/// The `x` at which `slope * x + offset` lies in `low..=high`, relative to
/// the segment's start.
fn solve(slope: f64, offset: f64, low: f64, high: f64) -> Option<(f64, f64)> {
    if slope == 0.0 {
        return (low..=high).contains(&offset).then_some((f64::NEG_INFINITY, f64::INFINITY));
    }
    let (p, q) = ((low - offset) / slope, (high - offset) / slope);
    Some((p.min(q), p.max(q)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(body: &str, edge: u32, size: usize) -> Vec<u8> {
        let svg = format!(
            "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {edge} {edge}\">{body}</svg>"
        );
        coverage(&Icon::parse(&svg), size)
    }

    /// A covered pixel is 255, and one a quarter covered is four samples of 16:
    /// a square on pixel boundaries, and one half a pixel off them.
    #[test]
    fn a_square_covers_its_samples() {
        let on = draw("<path fill=\"currentColor\" d=\"M1 1H3V3H1Z\"/>", 4, 4);
        assert_eq!(on, [0, 0, 0, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 0, 0, 0]);
        let off = draw("<path fill=\"currentColor\" d=\"M.5.5h1v1h-1z\"/>", 4, 4);
        assert_eq!(off, [64, 64, 0, 0, 64, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    /// Against the area of a disc and of a ring, the oracle for the arc
    /// conversion and the stroke that needs no resvg: a filled circle drawn as
    /// two arcs, and a circle's round stroke, each within 0.5% of its area.
    #[test]
    fn a_disc_and_a_ring_cover_their_areas() {
        let area = |cov: &[u8]| cov.iter().map(|&a| a as f64 / 255.0).sum::<f64>();
        let pi = std::f64::consts::PI;
        let disc = draw("<path fill=\"currentColor\" d=\"M28 128A100 100 0 0 1 228 128a100,100,0,1,1-200,0Z\"/>", 256, 256);
        let want = pi * 100.0 * 100.0;
        assert!((area(&disc) - want).abs() < want * 5e-3, "disc: {} of {want}", area(&disc));
        let ring = draw(
            "<circle cx=\"128\" cy=\"128\" r=\"96\" fill=\"none\" stroke=\"currentColor\" \
             stroke-linecap=\"round\" stroke-linejoin=\"round\" stroke-width=\"24\"/>",
            256,
            256,
        );
        let want = pi * (108.0f64.powi(2) - 84.0f64.powi(2));
        assert!((area(&ring) - want).abs() < want * 5e-3, "ring: {} of {want}", area(&ring));
    }

    /// Each committed icon and the edge, in pixels, it is drawn at at scale
    /// 1: the compositor's cursors at 20, its title-bar buttons at 14, and
    /// `files`' entries at 32.
    const SHIPPED: &[(&str, u32)] = &[
        ("cursor-bold", 20),
        ("arrow-down-right-bold", 20),
        ("crosshair-simple-bold", 20),
        ("minus-bold", 14),
        ("square-bold", 14),
        ("x-bold", 14),
        ("folder-bold", 32),
        ("file-bold", 32),
    ];

    /// The committed icons draw to the pixels the owner accepted against
    /// resvg's, which no reading of this file can see, at the edges they
    /// were compared at: each as the width, the height and the coverage, the
    /// form the build shipped them in.
    #[test]
    fn the_icons_are_the_pixels_compared_with_resvg() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut digest = toyos_crypto::hash::Sha256::new();
        for &(stem, edge) in SHIPPED {
            let svg = std::fs::read(root.join(format!("assets/icons/{stem}.svg"))).expect("a committed icon");
            let mask = Icon::parse(std::str::from_utf8(&svg).unwrap()).mask(edge as usize);
            digest.update(&edge.to_le_bytes()).unwrap();
            digest.update(&edge.to_le_bytes()).unwrap();
            digest.update(mask.alpha()).unwrap();
        }
        let hex: String = digest.finalize().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(
            hex,
            "bc246dcfb81736e4aef449383ea3c666e38122ca47d0b2a2dfead9302653929e",
            "the icons' pixels moved: draw them against resvg 0.47 out of tree with the differential \
             posted on pull request #814, and ask the owner again on any visible change before \
             pinning the new digest"
        );
    }

    /// An icon at twice the edge is drawn at that edge: not the smaller
    /// mask doubled, whose every 2x2 block is one value.
    #[test]
    fn a_larger_edge_is_drawn_at_its_size() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../assets/icons/x-bold.svg");
        let icon = Icon::parse(&std::fs::read_to_string(root).unwrap());
        let (one, two) = (icon.mask(14), icon.mask(28));
        let doubled: Vec<u8> = (0..28 * 28).map(|i| one.alpha()[(i / 28 / 2) * 14 + (i % 28) / 2]).collect();
        assert_eq!(two.edge(), 28);
        assert_ne!(two.alpha(), &doubled[..]);
    }

    struct Pixels(core::cell::RefCell<Vec<[u8; 3]>>, usize, usize);

    impl Canvas for Pixels {
        fn size(&self) -> (usize, usize) {
            (self.1, self.2)
        }
        fn get(&self, x: usize, y: usize) -> [u8; 3] {
            self.0.borrow()[y * self.1 + x]
        }
        fn put(&self, x: usize, y: usize, rgb: [u8; 3]) {
            self.0.borrow_mut()[y * self.1 + x] = rgb;
        }
    }

    /// A mask hanging off the canvas's corners is drawn as far as the canvas
    /// goes; a covered pixel takes the colour and a quarter-covered one is a
    /// quarter of the way to it.
    #[test]
    fn a_mask_is_blended_and_clipped_to_its_canvas() {
        let mask = Mask { edge: 2, alpha: vec![255, 64, 0, 255] };
        let canvas = Pixels(core::cell::RefCell::new(vec![[0, 0, 0]; 4]), 2, 2);
        mask.draw(&canvas, 1, 1, [200, 100, 40]);
        assert_eq!(*canvas.0.borrow(), [[0, 0, 0], [0, 0, 0], [0, 0, 0], [200, 100, 40]]);
        mask.draw(&canvas, 0, 0, [255, 255, 255]);
        assert_eq!(canvas.0.borrow()[1], [64, 64, 64]);
        mask.draw(&canvas, -1, -1, [9, 9, 9]);
        assert_eq!(canvas.0.borrow()[0], [9, 9, 9]);
        assert_eq!(canvas.0.borrow()[1], [64, 64, 64]);
    }

    /// A contour wound against its outer one is a hole; one wound with it is not.
    #[test]
    fn a_fill_is_nonzero() {
        let hole = draw("<path fill=\"currentColor\" d=\"M0 0H3V3H0ZM1 1V2H2V1Z\"/>", 3, 3);
        assert_eq!(hole, [255, 255, 255, 255, 0, 255, 255, 255, 255]);
        let solid = draw("<path fill=\"currentColor\" d=\"M0 0H3V3H0ZM1 1H2V2H1Z\"/>", 3, 3);
        assert_eq!(solid, [255; 9]);
    }

    #[test]
    #[should_panic(expected = "paint \"#ff0000\" is neither currentColor nor none")]
    fn a_literal_colour_is_refused() {
        draw("<path fill=\"#ff0000\" d=\"M0 0H1V1Z\"/>", 4, 4);
    }

    #[test]
    #[should_panic(expected = "<path> fills black")]
    fn an_unpainted_shape_is_refused() {
        draw("<path d=\"M0 0H1V1Z\"/>", 4, 4);
    }

    #[test]
    #[should_panic(expected = "<line> is stroked without round caps and joins")]
    fn a_square_cap_is_refused() {
        draw("<line x2=\"4\" fill=\"none\" stroke=\"currentColor\" stroke-linecap=\"square\" stroke-linejoin=\"round\"/>", 4, 4);
    }

    #[test]
    #[should_panic(expected = "<path transform> is not read")]
    fn an_unread_attribute_is_refused() {
        draw("<path fill=\"currentColor\" transform=\"scale(2)\" d=\"M0 0H1V1Z\"/>", 4, 4);
    }

    #[test]
    #[should_panic(expected = "<g> has children")]
    fn a_group_is_refused() {
        draw("<g fill=\"currentColor\"><path d=\"M0 0H1V1Z\"/></g>", 4, 4);
    }

    #[test]
    #[should_panic(expected = "path command 'Q' is not read")]
    fn an_unread_command_is_refused() {
        draw("<path fill=\"currentColor\" d=\"M0 0Q1 1 2 0Z\"/>", 4, 4);
    }
}
