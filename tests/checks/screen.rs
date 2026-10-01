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
}
