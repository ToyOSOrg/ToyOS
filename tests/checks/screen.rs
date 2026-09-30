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

/// [`Ppm::text_row_bands`] against a panel drawn the way edk2's graphics
/// console draws one: 8x19 cells from the top-left corner, every cell a line
/// reaches blitted whole, over a logo centred behind the text. A short row
/// over the logo's top is cut free of it by the long row below, and two short
/// rows over the rest join it into one band, so a count across the whole width
/// is two rows short where one at the edge is exact.
pub fn edge_self_test() -> Result<(), String> {
    const COLUMNS: usize = 80;
    const ROWS: usize = 20;
    const EFI_GLYPH_HEIGHT: usize = 19;
    let (width, height) = (COLUMNS * EFI_GLYPH_WIDTH, ROWS * EFI_GLYPH_HEIGHT);
    let (logo_w, logo_h) = (160, 58);
    let long = "Black box: 0x8000000 armed, and the kernel is told so on its line";
    let wide = "Slot A: signed header 168fd26e79d8b062c82902c22b585649e13f6ff487b5a4079d8d21fbaef273ca verifies";
    let spaced = format!("{}{}", &wide[..COLUMNS], " ".repeat(20));
    let lines = [
        "BdsDxe: loading Boot0002",
        "ToyOS Bootloader 1.0",
        "Kernel: 3476976 bytes",
        "Loading kernel elf...",
        "Kernel stack size: 8388608",
        "Kernel memory size: 12783616",
        "Applied 5053 relocations",
        "",
        "GOP: mode 640x380",
        long,
        "Starting kernel...",
        "Boot map: root",
        spaced.as_str(),
        wide,
        "Loader log: the kernel handoff begins",
    ];

    let (text, logo) = ([0x98u8; 3], [0xFFu8; 3]);
    let mut pixels = vec![[0u8; 3]; width * height];
    let (lx, ly) = ((width - logo_w) / 2, (height - logo_h) / 2);
    for y in ly..ly + logo_h {
        pixels[y * width + lx..y * width + lx + logo_w].fill(logo);
    }
    let mut row = 0;
    for line in lines {
        let chars: Vec<char> = line.chars().collect();
        let wrapped: Vec<&[char]> = if chars.is_empty() { vec![&[]] } else { chars.chunks(COLUMNS).collect() };
        for cells in wrapped {
            for (col, c) in cells.iter().enumerate() {
                for gy in 0..EFI_GLYPH_HEIGHT {
                    for gx in 0..EFI_GLYPH_WIDTH {
                        let lit = !c.is_whitespace() && (3..=17).contains(&gy) && (1..=6).contains(&gx);
                        let (x, y) = (col * EFI_GLYPH_WIDTH + gx, row * EFI_GLYPH_HEIGHT + gy);
                        pixels[y * width + x] = if lit { text } else { [0; 3] };
                    }
                }
            }
            row += 1;
        }
    }

    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    for p in &pixels {
        ppm.extend_from_slice(p);
    }
    let counted = Ppm::parse(&ppm).text_row_bands()?;
    let expected = edge_rows(lines, COLUMNS);
    if (counted, expected) != (15, 15) {
        return Err(format!(
            "the staged panel carries 15 rows at its edge; text_row_bands counted {counted} and \
             edge_rows expected {expected}"
        ));
    }
    Ok(())
}
