use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

/// Where on ROOT the desktop font's licence is, and its text: the OFL asks
/// that every copy of the Font Software carry it.
pub const DESKTOP_FONT_LICENCE: (&str, &[u8]) =
    ("share/licenses/OFL-1.1-JetBrainsMono.txt", include_bytes!("../licenses/OFL-1.1-JetBrainsMono.txt"));

/// Where on ROOT the icons' licence is, and its text: MIT asks that every
/// copy carry it.
pub const ICON_LICENCE: (&str, &[u8]) =
    ("share/licenses/MIT-PhosphorIcons.txt", include_bytes!("../licenses/MIT-PhosphorIcons.txt"));

/// An icon ships as its SVG, which the program drawing it reads at its own
/// scale: the build reads it as that program will, so one past the subset
/// `toyos-icon` draws stops the build, by name, rather than the program.
fn refuse_past_the_subset(path: &Path, svg: &[u8]) {
    let text = std::str::from_utf8(svg).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    if std::panic::catch_unwind(|| toyos_icon::Icon::parse(text)).is_err() {
        panic!("{} is past the SVG subset `toyos-icon` draws, as said above", path.display());
    }
}

/// A TTF's glyphs in 8x16 cells, in the prebuilt format.
fn raster_8x16(ttf: &[u8]) -> Vec<u8> {
    font::Font::from_ttf(ttf, 8, 16).unwrap_or_else(|e| panic!("a TTF the build rasterizes: {e}")).prebuilt()
}

/// The pre-rasterized 8x16 font ROOT carries as
/// `/system/share/fonts/JetBrainsMono-Regular-8x16.font`, which `/system/bin/console`
/// blits.
///
/// Produced by the same `toyos-font` producer [`collect`] calls, and that a
/// program drawing at a scale runs on the TTF ROOT carries beside it, so the
/// screendump decoder in `tests/common/screen.rs` reads the exact table the
/// guest drew with at scale 1 — the property the checked-in `font8x16.bin`
/// gives the panic console, obtained here from one producer instead of one
/// file.
pub fn console_font(root: &Path) -> Vec<u8> {
    let ttf = fs::read(root.join("assets/JetBrainsMono-Regular.ttf"))
        .expect("console_font: JetBrainsMono-Regular.ttf not found");
    raster_8x16(&ttf)
}

/// Where the kernel's panic-console font lives, relative to the repo root.
pub const PANIC_FONT_PATH: &str = "kernel/src/drivers/panic_console/font8x16.bin";

/// First codepoint of the panic-console font; the file holds
/// `PANIC_FONT_GLYPHS` consecutive glyphs starting here.
pub const PANIC_FONT_FIRST: u8 = 0x20;
pub const PANIC_FONT_GLYPHS: usize = 0x7F - 0x20;
pub const PANIC_FONT_BYTES: usize = PANIC_FONT_GLYPHS * 16;

/// Alpha at or above which a rasterized pixel becomes a set bit. Chosen by
/// rendering the whole range at 8x16 and reading the decoded screendump: at
/// 96 every glyph in `0x20..=0x7E` is distinct and stems survive; higher
/// thresholds start eating the thin diagonals of `x` and `y`.
const PANIC_FONT_THRESHOLD: u8 = 96;

/// Regenerate `kernel/src/drivers/panic_console/font8x16.bin`.
///
/// Provenance: `assets/JetBrainsMono-Regular.ttf`, rasterized by `toyos-font`
/// at the largest pixel size whose printable-ASCII glyphs all fit an 8x16
/// cell, then thresholded to 1 bit at alpha >= 96. Layout is 95 glyphs of 16 bytes,
/// codepoint `0x20 + index`, one byte per row, bit 7 leftmost.
///
/// The artifact is checked in so the kernel can `include_bytes!` it with no
/// build-script coupling, and so the test harness's screendump decoder reads
/// the exact table the renderer blits. Two consumers, one file: the decoder
/// cannot drift from the renderer.
pub fn regen_panic_font(root: &Path) {
    let ttf = fs::read(root.join("assets/JetBrainsMono-Regular.ttf"))
        .expect("regen-font: JetBrainsMono-Regular.ttf not found");
    let font = font::Font::from_ttf(&ttf, 8, 16).expect("regen-font: the TTF");

    let mut out = vec![0u8; PANIC_FONT_BYTES];
    for glyph in 0..PANIC_FONT_GLYPHS {
        let ch = char::from(PANIC_FONT_FIRST + glyph as u8);
        let alpha = font.coverage(ch).expect("every printable ASCII glyph is rasterized");
        for row in 0..16 {
            let mut bits = 0u8;
            for col in 0..8 {
                if alpha[row * 8 + col] >= PANIC_FONT_THRESHOLD {
                    bits |= 0x80 >> col;
                }
            }
            out[glyph * 16 + row] = bits;
        }
    }

    let path = root.join(PANIC_FONT_PATH);
    fs::create_dir_all(path.parent().unwrap()).expect("regen-font: create dir");
    fs::write(&path, &out).expect("regen-font: write");
    println!("wrote {} ({} bytes)", path.display(), out.len());
}

/// Which files under `dir` git tracks, as paths relative to it.
///
/// **The image is a function of what the repository declares, not of what the
/// directory happens to hold.** Sweeping the directory instead put
/// `assets/.DS_Store` and an `assets/target/` some cargo invocation left behind
/// into every shipped image — 16,368 bytes of it, measured off
/// `target/bootable.img` — so a fresh clone built a different image and opening
/// the directory in Finder moved the image hash with no code change.
///
/// Asked of git rather than filtered by name: an ignore list for dotfiles and
/// `target/` states nothing about the property, and the next stray file ships
/// again. A build that cannot find out what is committed refuses, because it
/// cannot honestly build an image either.
fn tracked(dir: &Path) -> BTreeSet<PathBuf> {
    crate::sysroot::tracked_files(dir, &[]).unwrap_or_else(|e| panic!("{e}")).into_iter().map(PathBuf::from).collect()
}

/// The paths `declared` names under `dir` that are not there, in the order they
/// will be named in.
///
/// **An asset the build cannot find is named and skipped, not fatal.** It used
/// to stop the build, on the argument that a fresh clone should be told rather
/// than handed a doom with no music; the telling is what mattered and the
/// stopping was the part that cost — a fresh clone, a runner and every new
/// worktree red on a file that was then deliberately absent from all three.
///
/// The SoundFont is committed again, so `declared` is now git's index and
/// nothing else. That is what keeps this: [`tracked`] lists a file whether or
/// not the working tree holds it, so a deleted asset would otherwise leave the
/// image quietly without doom's music — which is exactly how `b8b0749` took it
/// away for a cycle with nothing saying so. Being told happens twice: here by
/// name, and again in the guest's own log when whatever wanted the file opens
/// it.
///
/// Its own function so that the naming is what a test can hold: the absence is
/// not a panic to catch, and "it printed something" is not a claim this build
/// can check about itself.
fn absentees(dir: &Path, declared: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    declared.iter().map(|name| dir.join(name)).filter(|path| !path.exists()).collect()
}

/// A file of ROOT's `share/`, and the one program that opens it.
///
/// **`assets = [..]` names a directory and sweeps it whole**, so a config that
/// builds no reader for a file would still ship it. Named here rather than per
/// config, because which program opens a file is a property of the program and
/// not of any one image, and a list repeated in five configs is a list that goes
/// stale in four of them. The names are ROOT's, which [`collect`] lower-cases;
/// `wallpaper.rgb` is the build's own drawing ([`crate::wallpaper`]) rather than
/// a swept file. `only_its_reader_opens_an_owned_file` is what keeps the
/// right-hand column true.
const OPENED_BY: &[(&str, &str)] =
    &[("doom1.wad", "doom"), ("soundfont.sf2", "doom"), ("wallpaper.rgb", "compositor")];

/// Whether ROOT's `share/<name>` goes into an image building exactly
/// `programs`: unless [`OPENED_BY`] names a reader the image does not build.
pub fn wanted(name: &str, programs: &BTreeSet<&str>) -> bool {
    match OPENED_BY.iter().find(|(file, _)| *file == name) {
        Some((_, reader)) if !programs.contains(reader) => {
            eprintln!("assets: leaving out share/{name} — only /system/bin/{reader} opens it and this image builds no {reader}");
            false
        }
        _ => true,
    }
}

/// ROOT's asset files, for an image building exactly `programs`.
pub fn collect(dirs: &[String], programs: &BTreeSet<&str>) -> Vec<(String, Vec<u8>)> {
    let mut files = vec![];

    for dir in dirs {
        let dir = Path::new(dir);
        let mut tracked = tracked(dir);
        for absent in absentees(dir, &tracked) {
            eprintln!(
                "assets: NOT IN THIS IMAGE — {} is committed and is not in this working tree, \
                 so whatever wants it runs without it.",
                absent.display()
            );
            tracked.remove(absent.strip_prefix(dir).unwrap_or(&absent));
        }
        let ships = |path: &Path| {
            let relative = path.strip_prefix(dir).unwrap_or(path);
            if !tracked.contains(relative) {
                eprintln!("assets: skipping {} — git does not track it", path.display());
                return false;
            }
            wanted(&path.file_name().unwrap_or_default().to_string_lossy().to_lowercase(), programs)
        };

        // A TTF at the top is the desktop's monospace font: it ships as its
        // 8x16 raster, which the console and a scale-1 screen test read, and
        // as itself, which a program drawing at a scale rasterizes.
        for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("Failed to read {}: {e}", dir.display())) {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "ttf") && ships(&path) {
                let ttf = fs::read(&path).unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
                let stem = path.file_stem().unwrap().to_str().unwrap();
                files.push((format!("share/fonts/{stem}-8x16.font"), raster_8x16(&ttf)));
                files.push((format!("share/fonts/{stem}.ttf"), ttf));
                let (licence, text) = DESKTOP_FONT_LICENCE;
                files.push((licence.to_string(), text.to_vec()));
            }
        }

        // Include all other files recursively (skipping pre-processed types).
        // A TTF at the top is taken above; one under `fonts/` is a system font
        // and ships as it is, to `/system/share/fonts/`, where the toolkits
        // look.
        fn add_dir(
            dir: &Path,
            prefix: &str,
            ships: &dyn Fn(&Path) -> bool,
            files: &mut Vec<(String, Vec<u8>)>,
        ) {
            for entry in fs::read_dir(dir).unwrap_or_else(|e| panic!("Failed to read {}: {e}", dir.display())) {
                let path = entry.unwrap().path();
                let system_font = prefix == "share/fonts/" && path.extension().is_some_and(|e| e == "ttf");
                if path.is_dir() {
                    let subdir = path.file_name().unwrap().to_str().unwrap();
                    add_dir(&path, &format!("{prefix}{subdir}/"), ships, files);
                } else if !system_font && path.extension().is_some_and(|e| e == "ttf") {
                    continue;
                } else if ships(&path) {
                    let name = path.file_name().unwrap().to_str().unwrap().to_lowercase();
                    let data = fs::read(&path).unwrap_or_else(|e| panic!("Failed to read {}: {e}", path.display()));
                    if path.extension().is_some_and(|e| e == "svg") {
                        refuse_past_the_subset(&path, &data);
                        if !files.iter().any(|(name, _)| *name == ICON_LICENCE.0) {
                            files.push((ICON_LICENCE.0.to_string(), ICON_LICENCE.1.to_vec()));
                        }
                    }
                    files.push((format!("{prefix}{name}"), data));
                }
            }
        }
        add_dir(dir, "share/", &ships, &mut files);
    }

    files
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// A sample of the `ch(de)` AltGr characters the console font once could
    /// not render — past Latin-1, so absent unless a layout's own reach is
    /// rasterized.
    #[test]
    fn the_console_font_covers_the_swiss_german_altgr_layer() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let raw = console_font(&root);
        let count = u32::from_le_bytes(raw[4..8].try_into().unwrap()) as usize;
        let covered: BTreeSet<u32> = (0..count)
            .map(|i| {
                let at = 8 + i * 4;
                u32::from_le_bytes(raw[at..at + 4].try_into().unwrap())
            })
            .collect();
        for ch in ['€', 'œ', 'Œ', 'ŋ', 'ħ', 'ł', 'ŧ', 'đ', 'ĸ', 'ſ', 'ẞ', 'Ω', 'ĉ', 'ń', 'Ÿ'] {
            assert!(covered.contains(&(ch as u32)), "{ch:?} is not in the console font");
        }
    }

    /// ROOT carries what the repository declares: git's index, and
    /// nothing else.
    ///
    /// Against a repository this test builds, not against `assets/`: the two
    /// files that shipped for real — `.DS_Store` and a stray `target/` — are
    /// exactly what a working tree acquires by being worked in, so a gate that
    /// depended on them being present would pass on a clean checkout and prove
    /// nothing. Here they are put there on purpose.
    ///
    /// `music.sf2` is the other half, and it is the half that matters most: a
    /// committed asset that is not in the working tree is silently absent from
    /// the image, which is how doom lost its music for a cycle with the whole
    /// suite green.
    #[test]
    fn root_carries_what_the_repository_declares() {
        let dir = toyos_tmpdir::TempDir::new("assets");
        fs::create_dir_all(dir.join("icons")).expect("make the asset tree");
        fs::create_dir_all(dir.join("target")).expect("make a stray target/");

        fs::write(dir.join("kept.wad"), b"tracked").expect("write kept.wad");
        // An icon ships drawn, and only when tracked: both are committed icons.
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        fs::copy(root.join("assets/icons/x-bold.svg"), dir.join("icons/x-bold.svg")).expect("copy x-bold.svg");
        fs::copy(root.join("assets/icons/minus-bold.svg"), dir.join("icons/minus-bold.svg")).expect("copy minus-bold.svg");
        fs::write(dir.join("music.sf2"), b"tracked").expect("write music.sf2");
        fs::write(dir.join(".DS_Store"), b"finder").expect("write .DS_Store");
        fs::write(dir.join("target/.deps-stamp"), b"cargo").expect("write target/.deps-stamp");

        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-C", &dir.display().to_string()])
                .args(args)
                .output()
                .expect("run git");
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q"]);
        git(&["add", "kept.wad", "icons/x-bold.svg", "music.sf2"]);

        let shipped: BTreeSet<String> = collect(&[dir.display().to_string()], &BTreeSet::new())
            .into_iter()
            .map(|(name, _)| name)
            .collect();

        assert_eq!(
            shipped,
            BTreeSet::from([
                "share/kept.wad".to_string(),
                "share/icons/x-bold.svg".to_string(),
                ICON_LICENCE.0.to_string(),
                "share/music.sf2".to_string(),
            ]),
            "ROOT's asset list is not what the repository says it is"
        );

        // And the other half: an asset git carries that this tree does not.
        // The build goes on without it — what has to survive is that the rest
        // of the image is exactly what it was, and that the absent one is
        // named.
        fs::remove_file(dir.join("music.sf2")).expect("take music.sf2 away");
        let without: BTreeSet<String> = collect(&[dir.display().to_string()], &BTreeSet::new())
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        let named = absentees(&dir, &BTreeSet::from([PathBuf::from("music.sf2")]));

        assert_eq!(
            without,
            BTreeSet::from([
                "share/kept.wad".to_string(),
                "share/icons/x-bold.svg".to_string(),
                ICON_LICENCE.0.to_string(),
            ]),
            "a committed asset that is not there took something else with it"
        );
        assert_eq!(
            named,
            vec![dir.join("music.sf2")],
            "a committed asset that is not there has to be named"
        );
    }

    /// An asset [`OPENED_BY`] names ships to the image building its reader and
    /// to no other, and an asset it does not name ships to both.
    #[test]
    fn an_owned_asset_ships_only_where_its_reader_does() {
        let dir = toyos_tmpdir::TempDir::new("owned");
        for name in ["doom1.wad", "soundfont.sf2", "readme.txt"] {
            fs::write(dir.join(name), b"tracked").unwrap_or_else(|e| panic!("write {name}: {e}"));
        }
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-C", &dir.display().to_string()])
                .args(args)
                .output()
                .expect("run git");
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q"]);
        git(&["add", "doom1.wad", "soundfont.sf2", "readme.txt"]);

        let shipped = |programs: BTreeSet<&str>| -> BTreeSet<String> {
            collect(&[dir.display().to_string()], &programs)
                .into_iter()
                .map(|(name, _)| name)
                .collect()
        };
        let with = shipped(BTreeSet::from(["doom", "compositor"]));
        let without = shipped(BTreeSet::from(["compositor"]));

        assert_eq!(
            with,
            BTreeSet::from([
                "share/doom1.wad".to_string(),
                "share/soundfont.sf2".to_string(),
                "share/readme.txt".to_string(),
            ]),
            "an image that builds doom did not get doom's assets"
        );
        assert_eq!(
            without,
            BTreeSet::from(["share/readme.txt".to_string()]),
            "an image with no doom in it still carries what only doom opens"
        );
    }

    /// A TTF under `fonts/` ships byte for byte where the toolkits look for
    /// system fonts; the one at the top ships only as the console's raster.
    #[test]
    fn a_system_font_ships_as_it_is_and_the_desktop_font_as_its_raster_and_itself() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let dir = toyos_tmpdir::TempDir::new("fonts");
        fs::create_dir_all(dir.join("fonts")).expect("make the asset tree");
        let console = fs::read(root.join("assets/JetBrainsMono-Regular.ttf")).expect("the desktop's font");
        let system = fs::read(root.join("assets/fonts/OpenSans-Regular.ttf")).expect("a system font");
        fs::write(dir.join("Console.ttf"), &console).unwrap();
        fs::write(dir.join("fonts/System-Regular.ttf"), &system).unwrap();
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(["-C", &dir.display().to_string()])
                .args(args)
                .output()
                .expect("run git");
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        };
        git(&["init", "-q"]);
        git(&["add", "Console.ttf", "fonts/System-Regular.ttf"]);

        let shipped: std::collections::BTreeMap<String, Vec<u8>> =
            collect(&[dir.display().to_string()], &BTreeSet::new()).into_iter().collect();

        assert_eq!(
            shipped.keys().collect::<Vec<_>>(),
            [
                "share/fonts/Console-8x16.font",
                "share/fonts/Console.ttf",
                "share/fonts/system-regular.ttf",
                DESKTOP_FONT_LICENCE.0
            ],
            "the fonts did not land where the toolkits, the console and the desktop look"
        );
        assert_eq!(shipped["share/fonts/Console.ttf"], console, "the desktop's font was changed on its way in");
        assert_eq!(
            shipped["share/fonts/system-regular.ttf"], system,
            "a system font was changed on its way into the image"
        );
    }

    /// The image carries the system fonts' licence beside them, as the OFL
    /// asks of every copy.
    #[test]
    fn the_system_fonts_ship_with_their_licence() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let shipped: std::collections::BTreeMap<String, Vec<u8>> =
            collect(&[root.join("assets").display().to_string()], &BTreeSet::new())
                .into_iter()
                .collect();
        let fonts: Vec<&String> =
            shipped.keys().filter(|k| k.starts_with("share/fonts/") && k.ends_with(".ttf")).collect();
        assert!(!fonts.is_empty(), "the image ships no system font:\n{:?}", shipped.keys());
        let licence = fs::read(root.join("assets/fonts/OFL.txt")).expect("the fonts' licence");
        assert_eq!(
            shipped.get("share/fonts/ofl.txt"),
            Some(&licence),
            "{fonts:?} ship without the licence text beside them"
        );
    }

    /// The right-hand column of [`OPENED_BY`] is true of the tree.
    ///
    /// The claim is that one program opens the file, so a second program naming
    /// its path is an image losing a file it needs. The program roots only:
    /// the harness names doom's paths in its assertions, and it runs on the
    /// host.
    #[test]
    fn only_its_reader_opens_an_owned_file() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for (asset, reader) in OPENED_BY {
            let out = Command::new("git")
                .args(["-C", &root.display().to_string()])
                .args(["grep", "-l", &format!("/system/share/{asset}"), "--"])
                .args(crate::build::PROGRAM_ROOTS)
                .output()
                .expect("run git grep");
            let hits: Vec<String> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .map(str::to_string)
                .collect();
            assert!(!hits.is_empty(), "no program opens /system/share/{asset} at all");
            let dir = crate::hostws::rel(&root, &crate::build::program_dir(&root, reader));
            let strangers: Vec<&String> = hits.iter().filter(|p| !p.starts_with(&format!("{dir}/"))).collect();
            assert!(
                strangers.is_empty(),
                "OPENED_BY says only /system/bin/{reader} opens /system/share/{asset}, and {strangers:?} \
                 name it too — an image without {reader} would be built without a file it needs"
            );
        }
    }
}
