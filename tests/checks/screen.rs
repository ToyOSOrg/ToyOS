use super::*;
use screen::*;

/// Render `lines` the way the kernel would and decode them back, proving the
/// decoder against a bitmap it fully controls before it is pointed at a real
/// screendump. Panics on mismatch.
pub fn self_test() {
    Font::load();
    let raw = std::fs::read(Font::path()).expect("font8x16.bin not found");
    let lines = [
        "PANIC: panicked at src/loader.rs:952:40",
        "  0xffff80007d102adc kernel::loader::spawn_kernel+0x28e",
        "the quick brown fox JUMPS over 13 lazy dogs {}[]<>|~",
    ];
    let cols = lines.iter().map(|l| l.len()).max().unwrap();
    let width = cols * GLYPH_W;
    let height = lines.len() * GLYPH_H;
    // Dark red fill and white text: the same colours render() uses, so the
    // threshold is exercised, not bypassed.
    let mut pixels = vec![[0x60u8, 0x00, 0x00]; width * height];
    for (row, line) in lines.iter().enumerate() {
        for (col, ch) in line.bytes().enumerate() {
            let at = (ch - FIRST_CH) as usize * GLYPH_H;
            let g = &raw[at..at + GLYPH_H];
            for (r, bits) in g.iter().enumerate() {
                for c in 0..GLYPH_W {
                    if bits & (0x80 >> c) != 0 {
                        let x = col * GLYPH_W + c;
                        let y = row * GLYPH_H + r;
                        pixels[y * width + x] = [0xFF, 0xFF, 0xFF];
                    }
                }
            }
        }
    }

    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    for p in &pixels {
        ppm.extend_from_slice(p);
    }

    let decoded = Ppm::parse(&ppm).text();
    let expected = lines.map(|l| l.trim_end()).join("\n");
    assert_eq!(decoded, expected, "screen decoder round-trip failed");

    console_self_test();
}

/// The same round trip for the console's font, and one thing the kernel's
/// cannot have: the two tables must not decode each other. `ConsoleFont::load`
/// has already refused an ambiguous printable-ASCII table by the time this
/// runs.
fn console_self_test() {
    let font = ConsoleFont::load();
    let lines = [
        "[kernel 0.099] i8042: ok selftest=0x55 cfg=0x77->0x64 port1=ok port2=ok",
        "/> echo hello",
        "the quick brown fox JUMPS over 13 lazy dogs {}[]<>|~",
    ];
    let cols = lines.iter().map(|l| l.len()).max().unwrap();
    let width = cols * GLYPH_W;
    let height = lines.len() * GLYPH_H;
    // White on black: `draw_char`'s blend then reduces to the alpha itself,
    // which is what makes the decode exact rather than a nearest match.
    let mut pixels = vec![[0u8, 0, 0]; width * height];
    for (row, line) in lines.iter().enumerate() {
        for (col, ch) in line.chars().enumerate() {
            let cell = font.by_cell.iter().find(|(_, c)| **c == ch).expect("a glyph for every char staged").0;
            for r in 0..GLYPH_H {
                for c in 0..GLYPH_W {
                    let a = cell[r * GLYPH_W + c];
                    pixels[(row * GLYPH_H + r) * width + col * GLYPH_W + c] = [a, a, a];
                }
            }
        }
    }

    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    for p in &pixels {
        ppm.extend_from_slice(p);
    }
    let dump = Ppm::parse(&ppm);
    let expected = lines.map(|l| l.trim_end()).join("\n");
    assert_eq!(
        dump.console_text(&font),
        expected,
        "console screen decoder round-trip failed"
    );

    // The non-vacuity property the console tests lean on, measured rather than
    // argued: a screen the *kernel* painted carries the thresholded form of
    // these glyphs, and the two tables are not interchangeable in either
    // direction.
    assert!(
        !dump.text().contains("i8042: ok selftest"),
        "the kernel's 1-bit table decodes anti-aliased console glyphs, so a \
         console test could pass on a screen the console never touched"
    );
}
