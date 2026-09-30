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

/// [`Ppm::firmware_text_mode`], [`Ppm::edge_rows`] and [`TextMode::panel`]
/// against the T14's 1920x1080 panel drawn the way edk2's graphics console
/// draws it, in the two text modes firmware has chosen there: 80x50, whose
/// text starts 640 pixels in and which these lines scroll, and 240x56, whose
/// text starts at the edge and which they do not.
pub fn edge_self_test() -> Result<(), String> {
    let short: Vec<String> = (0..45).map(|n| format!("Kernel memory size: {n}")).collect();
    let wide = format!("Slot A: signed header {} verifies", "0123456789abcdef".repeat(9));
    let lines: Vec<&str> = short
        .iter()
        .map(String::as_str)
        .chain([
            wide.as_str(),
            "Boot attempts: this image has had the machine 0 time(s) without reporting; now 1",
            "",
            "        Kernel memory located past the edge",
            "Loader log: the kernel handoff begins",
        ])
        .collect();
    // Counted by hand: at 80 columns the lines take 45 + 3 + 2 + 1 + 1 + 1 = 53
    // rows and the first four scroll off; at 240 they take 50.
    for (staged, lit) in [
        (TextMode { columns: 80, rows: 50, left: 640, top: 65 }, 41 + 3 + 1 + 1),
        (TextMode { columns: 240, rows: 56, left: 0, top: 8 }, 45 + 1 + 1 + 1),
    ] {
        let dump = edk2_panel(1920, 1080, staged, &lines);
        let mode = dump.firmware_text_mode()?;
        let (carried, printed) = (dump.edge_rows(mode), mode.panel(lines.iter().copied()));
        let count = |rows: &[bool]| rows.iter().filter(|&&row| row).count();
        if (mode, count(&carried), count(&printed)) != (staged, lit, lit) || carried != printed {
            return Err(format!(
                "a panel staged in {staged:?} with {lit} rows lit at its edge was read as {mode:?}, \
                 carrying {} where the console put {}",
                count(&carried),
                count(&printed)
            ));
        }
    }
    Ok(())
}

/// `lines` printed in `mode` on a cleared `width` x `height` panel, over a
/// block the size of edk2's `Logo.bmp` centred on it. Every cell a line
/// reaches is blitted whole, a glyph lighting the pixels an 8x19 capital does;
/// a line wraps once its row is full; and a line feed on the last row moves
/// the text area's pixels, the logo's with them, up a row.
fn edk2_panel(width: usize, height: usize, mode: TextMode, lines: &[&str]) -> Ppm {
    let (text, logo) = ([0x98u8; 3], [0xFFu8; 3]);
    let (logo_w, logo_h) = (193, 58);
    let mut pixels = vec![[0u8; 3]; width * height];
    let (lx, ly) = ((width - logo_w) / 2, (height - logo_h) / 2);
    for y in ly..ly + logo_h {
        pixels[y * width + lx..][..logo_w].fill(logo);
    }
    let area = mode.columns * EFI_GLYPH_WIDTH;
    let line_feed = |pixels: &mut Vec<[u8; 3]>, row: &mut usize| {
        if *row + 1 < mode.rows {
            *row += 1;
            return;
        }
        for y in mode.top..mode.top + (mode.rows - 1) * EFI_GLYPH_HEIGHT {
            let from = (y + EFI_GLYPH_HEIGHT) * width + mode.left;
            pixels.copy_within(from..from + area, y * width + mode.left);
        }
        for y in mode.top + (mode.rows - 1) * EFI_GLYPH_HEIGHT..mode.top + mode.rows * EFI_GLYPH_HEIGHT {
            pixels[y * width + mode.left..][..area].fill([0; 3]);
        }
    };
    let (mut row, mut column) = (0, 0);
    for line in lines {
        for c in line.chars() {
            for gy in 0..EFI_GLYPH_HEIGHT {
                for gx in 0..EFI_GLYPH_WIDTH {
                    let lit = !c.is_whitespace() && (3..=14).contains(&gy) && gx <= 6;
                    let (x, y) = (mode.left + column * EFI_GLYPH_WIDTH + gx, mode.top + row * EFI_GLYPH_HEIGHT + gy);
                    pixels[y * width + x] = if lit { text } else { [0; 3] };
                }
            }
            column += 1;
            if column == mode.columns {
                column = 0;
                line_feed(&mut pixels, &mut row);
            }
        }
        column = 0;
        line_feed(&mut pixels, &mut row);
    }

    let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
    for p in &pixels {
        ppm.extend_from_slice(p);
    }
    Ppm::parse(&ppm)
}
