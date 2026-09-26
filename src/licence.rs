//! The licence gate: everything that ships as part of ToyOS is under a licence
//! [`ALLOWED`] names, or is an [`EXCEPTIONS`] row that says why it is not.
//!
//! **What ships is read out of the build, never listed.** The crates are every
//! crate [`crate::build::shipped`] says an image of the three modes is built
//! from, each with the features the build gives it, plus
//! [`crate::libc::CRATE`] with [`crate::libc::FEATURES`], and std from the fork
//! checkout this tree pins with every feature on, since bootstrap picks std's.
//! `cargo metadata` resolves each workspace once per feature set, and the gate
//! walks the *normal* edges from the shipped roots: a build-dependency runs on
//! the host and a dev-dependency builds a test, and neither is linked into the
//! image. Every edge is walked whatever its target `cfg`, so the set judged is a
//! superset of what one image links, and a crate only another platform pulls in
//! can red here. Git and path packages are judged exactly as registry ones.
//!
//! **A committed file is judged by its [`COMMITTED_FILES`] row**,
//! whose [`Terms`] column is its licence. A file ships when it is under a
//! shipped asset directory or a shipped path package's directory, or when such
//! a package [`embeds`] it. Every file under a shipped asset directory must have
//! a row.
//!
//! **A `NOTICE` section covers what no row can name**: each carries one
//! `SPDX-License-Identifier:` line or is in [`PROSE`]. A section whose files all
//! have rows must agree with them and is judged through them. Any other ships
//! when a file it names is under a shipped directory. A section that names no
//! tracked file is refused.
//!
//! **An exception is named, reasoned, and matched exactly**: by package name,
//! section path or file *and* by the licence text it was written against, so a
//! licence that changes reds again. An exception nothing matches is refused as
//! stale. One that is [`Standing::OnlyUnder`] a `cfg` rests on no guest target
//! satisfying that `cfg`, which `tests::no_guest_target_satisfies_an_only_under_cfg`
//! holds against the ToyOS target's `cfg`s. What the gate checks is that every
//! edge into the package is under exactly that `cfg`.
//!
//! **What it does not read**: a file a shipped package embeds through a path it
//! computes, or names on another line than its `include_bytes!`; a registry or
//! git crate's vendored non-Rust source, judged by the crate's `license` field
//! alone; a path package's vendored source that has neither a row nor a
//! section. And std's graph is re-locked when the gate runs, not read from a
//! committed lock, so two runs at one head can resolve it differently.
//!
//! The expression grammar is SPDX's (`OR`, `AND`, `WITH`, parentheses) plus
//! cargo's legacy `/` for `OR`. An identifier outside the allowlist is refused
//! whether SPDX knows it or not, so no licence list is needed to refuse the
//! unrecognised.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::build::Features;

/// What a shipped crate or file may be under. `OR` passes if any branch does,
/// `AND` only if every part does.
pub const ALLOWED: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "0BSD",
    "CC0-1.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "BSL-1.0",
    "MPL-2.0",
];

/// `WITH` pairs that pass. An exception only ever adds permissions, but a pair
/// passes only when it is named here.
const ALLOWED_WITH: &[(&str, &str)] = &[("Apache-2.0", "LLVM-exception")];

/// Allowed for a [`Terms::Font`] row, and nowhere else.
const FONTS_ONLY: &[&str] = &["OFL-1.1"];

/// Surfaced by name in every verdict, not refused.
const NAMED: &[&str] = &["MPL-2.0"];

/// The licence a committed file is under: [`COMMITTED_FILES`]'s
/// fourth column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Terms {
    /// An SPDX expression.
    Spdx(&'static str),
    /// A font's glyphs, or a raster of them, under an SPDX expression: the one
    /// place [`FONTS_ONLY`] passes.
    Font(&'static str),
}

impl Terms {
    fn expr(self) -> &'static str {
        match self {
            Terms::Spdx(e) | Terms::Font(e) => e,
        }
    }
}

/// What an exception is matched against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Subject {
    /// A package, by name.
    Crate(&'static str),
    /// A `NOTICE` section, by the path its heading starts with.
    Notice(&'static str),
    /// A [`COMMITTED_FILES`] row, by its path.
    File(&'static str),
}

/// Why an exception stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    /// It ships, and whether it may is the owner's to rule, which this issue
    /// file tracks.
    PendingOwner(&'static str),
    /// Every edge into it is taken under this `cfg`, exactly as cargo prints
    /// it, and no guest target satisfies that `cfg`. The gate walks every edge
    /// whatever its `cfg`, so this is what a package it cannot link says; it
    /// matches only a finding whose every incoming edge is under this `cfg`.
    OnlyUnder(&'static str),
}

/// A shipped thing whose licence is not allowed, left in by name.
pub struct Exception {
    pub subject: Subject,
    /// The licence text exactly as the package, section or row declares it;
    /// empty for a package that declares none.
    pub licence: &'static str,
    pub standing: Standing,
    pub reason: &'static str,
}

const DOOM_LEAVES: &str =
    "issues/filesystem/a-package-is-a-directory-under-apps-and-the-installer-is-a-program.md";

const fn pending(
    subject: Subject,
    licence: &'static str,
    issue: &'static str,
    reason: &'static str,
) -> Exception {
    Exception {
        subject,
        licence,
        standing: Standing::PendingOwner(issue),
        reason,
    }
}

/// Every current exception.
pub const EXCEPTIONS: &[Exception] = &[
    pending(
        Subject::Crate("doom"),
        "GPL-2.0-only",
        DOOM_LEAVES,
        "/system/bin/doom links doomgeneric, id's Doom source by way of Chocolate Doom (NOTICE); \
         it leaves the image as a package at the stage where the apps leave this repository",
    ),
    pending(
        Subject::Notice("userland/doom"),
        "GPL-2.0-or-later",
        DOOM_LEAVES,
        "doomgeneric, the C half of the doom crate, fetched at build time and compiled into it; \
         it leaves with doom",
    ),
    pending(
        Subject::File("assets/DOOM1.WAD"),
        "LicenseRef-id-Software-DOOM1-Shareware",
        DOOM_LEAVES,
        "id's shareware terms: redistributable unmodified and not for consideration, so an image \
         carrying it may not be sold (NOTICE); it leaves with doom",
    ),
    pending(
        Subject::File("assets/soundfont.sf2"),
        "LicenseRef-GeneralUser-GS-2.0",
        DOOM_LEAVES,
        "GeneralUser GS's own permissive licence, no standard one, with its author's caveat on \
         where the samples came from (NOTICE); doom alone opens it, and it leaves with doom",
    ),
    Exception {
        subject: Subject::Crate("windows-sys"),
        licence: "",
        standing: Standing::OnlyUnder("cfg(target_os = \"windows\")"),
        reason: "std's empty in-tree stand-in for windows-sys, which declares no licence",
    },
    Exception {
        subject: Subject::Crate("windows-link"),
        licence: "",
        standing: Standing::OnlyUnder("cfg(any(windows, target_os = \"cygwin\"))"),
        reason: "std's in-tree stand-in for windows-link, which declares no licence",
    },
];

/// Every committed file whose terms somebody had to establish, with the digest
/// of what is committed and where the terms are recorded.
///
/// A third column of `NOTICE` means that file carries this same digest, so the
/// obligation and the bytes cannot drift apart; anything else names the file
/// that carries the attribution, or says why it is ours. The fourth is the
/// licence [`judge`] holds the file to wherever an image ships it.
///
/// `src/sourcegate.rs` holds the rows to the tree. **Two populations, and each
/// is a spelling too.** Everything `git` tracks
/// that carries a NUL in its first 8000 bytes, which is git's own heuristic for
/// a file nobody can read in review; and everything under `assets/`, text or
/// not, because that directory is where third-party material arrives. A
/// third-party *text* file anywhere else — a ninth Phosphor SVG one directory
/// over — is reached by neither, and the corpus by count alone and no digest;
/// `issues/build/the-third-party-corpus-is-in-no-machine-read-ledger.md` is
/// what is left of that gap.
pub const COMMITTED_FILES: &[(&str, &str, &str, Terms)] = &[
    // The ACPI tables QEMU 11.1.0 published to a `Profile::Headless` guest,
    // read out of guest physical memory over the monitor. Firmware output, not
    // third-party source: `toyos-acpi/tests/fixtures.rs` decodes them against
    // what that boot's kernel logged.
    (
        "toyos-acpi/fixtures/qemu-11.1.0/apic.bin",
        "441794f0b4bd74feb6f4fc1adf82048023a612ba676dc308c8173e449c0ccdbb",
        "ours: QEMU's own MADT, captured by the commit that added toyos-acpi",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "toyos-acpi/fixtures/qemu-11.1.0/dmar.bin",
        "30df13af55b4b10bb3c2644d26480aa7ee302deaaf141a7a1ff2a3e053030290",
        "ours: QEMU's own DMAR, captured by the commit that added toyos-acpi",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "toyos-acpi/fixtures/qemu-11.1.0/facp.bin",
        "410716dfb169eaba296ed3c336028843b3cf9fce6ca7c179bb242199cfec9d1a",
        "ours: QEMU's own FADT, captured by the commit that added toyos-acpi",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "toyos-acpi/fixtures/qemu-11.1.0/hpet.bin",
        "8a486edc412b6e5f1b906ebf3fcfd6a647c8987d8ec437e4cbb808f34d7e2775",
        "ours: QEMU's own HPET table, captured by the commit that added toyos-acpi",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "toyos-acpi/fixtures/qemu-11.1.0/mcfg.bin",
        "5274632ea7572e49d97249c05ada4b2f597ad0603ba801538ddef4bd91add994",
        "ours: QEMU's own MCFG, captured by the commit that added toyos-acpi",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "toyos-acpi/fixtures/qemu-11.1.0/rsdp.bin",
        "8e3493811dfa7d164fc2076846908139df2f962eea2a5b2e45405949cbd82bf9",
        "ours: QEMU's own RSDP, captured by the commit that added toyos-acpi",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "toyos-acpi/fixtures/qemu-11.1.0/waet.bin",
        "21cf099f063f6422353ea0c9100bbe47a97d7be87449c12f201a8a3bb49b7f4e",
        "ours: QEMU's own WAET, captured by the commit that added toyos-acpi",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "toyos-acpi/fixtures/qemu-11.1.0/xsdt.bin",
        "701b192931e243a094a83f59f9b82d28204f1b3a0d11ac4d813e7769966427df",
        "ours: QEMU's own XSDT, captured by the commit that added toyos-acpi",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "toyos-acpi/fixtures/ovmf-pure-efi/root-bridge-0.bin",
        "eb00e68be746a09ac7f0ce1ca492ce8c858e3af1152112b49ffb4708883acbfb",
        "ours: OVMF's answer on a q35 guest, read off that boot's own loader log",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "toyos-acpi/fixtures/thinkpad-t14/root-bridge-0.bin",
        "a734078ed9ca3971ce804fd9ecc97b7794f816058f9ae17e47e0d2bcb63af0f3",
        "ours: the T14's answer, read off that boot's own loader log on the stick",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    // A bcachefs volume upstream's own tools wrote, gzipped. The bytes inside
    // it are this repository's test material; `NOTICE` carries the raw digest,
    // the commands, and the fsck that called it clean.
    (
        "bcachefs/tests/fixtures/crc32c.img.gz",
        "7be2c99db0e68c784ea59ef394454a084fa57868b14027528ab6b8b21031840e",
        "NOTICE",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "assets/DOOM1.WAD",
        "1d7d43be501e67d927e415e0b8f3e29c3bf33075e859721816f652a526cac771",
        "NOTICE",
        Terms::Spdx("LicenseRef-id-Software-DOOM1-Shareware"),
    ),
    (
        "assets/JetBrainsMono-Regular.ttf",
        "e6fd0d7e91550b3ed2b735d4312474362c4716edc4fc0577a0f61ed782d5aed1",
        "NOTICE",
        Terms::Font("OFL-1.1"),
    ),
    (
        "assets/icons/arrow-down-right-bold.svg",
        "8e107bfe4c746c762c97a7dbb6472db4669947ccdb5498fa843448ce8f6b69f4",
        "NOTICE",
        Terms::Spdx("MIT"),
    ),
    (
        "assets/icons/crosshair-simple-bold.svg",
        "d1c42a390a49ef683b42e7aa2f45da0cf7f0ebdecf6a4977b2c2e2f2226fd594",
        "NOTICE",
        Terms::Spdx("MIT"),
    ),
    (
        "assets/icons/cursor-bold.svg",
        "cc39efe6482c577e6a3ccedf6efcad26769a9eb0bc2868fb18f9a22de43bf172",
        "NOTICE",
        Terms::Spdx("MIT"),
    ),
    (
        "assets/icons/file-bold.svg",
        "b74d67f0af33fc62e83fbb2c7c8189f37713f482f06685088d46f8592f0e660f",
        "NOTICE",
        Terms::Spdx("MIT"),
    ),
    (
        "assets/icons/folder-bold.svg",
        "3f564dd4a0d27706ff9cb2d9738cef9ac1009b70f82d1da6d3bac119e41949a0",
        "NOTICE",
        Terms::Spdx("MIT"),
    ),
    (
        "assets/icons/minus-bold.svg",
        "ec912ee836d44c0e94e2493e7efbf9c4b93ee9c0dd9193d19cf86448b7e31dcd",
        "NOTICE",
        Terms::Spdx("MIT"),
    ),
    (
        "assets/icons/square-bold.svg",
        "d8284370bac0b7ccb3760fc1cd214f36f716e2724e4abd63eb2696bc345bdc45",
        "NOTICE",
        Terms::Spdx("MIT"),
    ),
    (
        "assets/icons/x-bold.svg",
        "394ad30f37b493b58cc7c26e816d7ec0bf7acc4a583fa39c9aed438c19b5fc57",
        "NOTICE",
        Terms::Spdx("MIT"),
    ),
    (
        "assets/hello.rs",
        "f30395cdbe2fff2c6ff6fe6dd270ded78a6b236ecfb598b9476cb71dc6cea214",
        "ours: the smallest guest binary's source, built by tests/common/compile.rs",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "assets/soundfont.sf2",
        "89a13a5c907b5cc83c15679e07e6dcb06fd72102937e092dc4a582f1aa5905c3",
        "NOTICE",
        Terms::Spdx("LicenseRef-GeneralUser-GS-2.0"),
    ),
    (
        "assets/wallpaper.jpg",
        "b6f0c89bf966cfb458333b280614f0c7723615e42e340b9d43a760a64fe05976",
        "ours: `cargo run -- --regen-wallpaper` writes it from src/wallpaper.rs",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "doom.jpg",
        "ae22f71dc732580bd4f789937c9fe564969029413fc2092f27bdae8d1ceaf8e3",
        "a screenshot of this system running doom, in README.md; \
         issues/build/doom-jpg-shows-ids-art-under-no-recorded-terms.md",
        Terms::Spdx("NOASSERTION"),
    ),
    (
        "first-boot.jpg",
        "41a65f4bf1f752bcc9da717e3c8f7f776bfbc214b2354b11a3c1e0278a9be22e",
        "ours: the T14's first boot photographed, in README.md",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "kernel/src/drivers/panic_console/font8x16.bin",
        "e1bea9791e07a0e2509196c6cb4563d44cafca1f19b0ef660319b0fa53546a3e",
        "NOTICE",
        Terms::Font("OFL-1.1"),
    ),
    (
        "aavmf/AAVMF_CODE.fd",
        "47765fe344818cbc464b1c14ae658fb4b854f5c2ceffa982411731eb4865594d",
        "NOTICE",
        Terms::Spdx("BSD-2-Clause-Patent"),
    ),
    (
        "aavmf/AAVMF_VARS.fd",
        "b3b855c5a80310168051164986855692d1bdb06e67619856177965cd87c6774f",
        "NOTICE",
        Terms::Spdx("BSD-2-Clause-Patent"),
    ),
    (
        "ovmf/DEBUGX64_OVMF.fd",
        "800ff5af1220d1232d4da7173ccddbb74a9217600bd8935903d9d534801778b4",
        "NOTICE",
        Terms::Spdx("BSD-2-Clause-Patent"),
    ),
    (
        "ovmf/OVMF_CODE-pure-efi.fd",
        "9de33971d47958f42af86584b502f83256120b2482e4f7ed14db32fd68e92922",
        "NOTICE",
        Terms::Spdx("BSD-2-Clause-Patent"),
    ),
    (
        "ovmf/OVMF_VARS-pure-efi.fd",
        "c653de93db67e4f2213a35598efb379a13ef4a12c241e003699d4d7afd193635",
        "NOTICE",
        Terms::Spdx("BSD-2-Clause-Patent"),
    ),
    (
        "tests/fixtures/gbae-v0.2.0-toyos-x86_64.tar.gz",
        "99fcd8a7263b5c25cd90cead1baaa7200ef272100fc2226e008a4e8205ba2916",
        "NOTICE",
        Terms::Spdx("MIT"),
    ),
    (
        "toyos-elf/tests/fixtures/toyos-ld-headers.bin",
        "6243d543a15941133514c1a8a24c79d118060caeae7e985870a67d9fc3021354",
        "ours: the first 4096 bytes of a toyos-ld output (toyos-elf/tests/real.rs)",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
    (
        "toyos-symbols/tests/fixtures/input-test.bin",
        "6a08f75ee01bdbd1e77c9b3affd6185e981d86995da432c90ed107676f08eb83",
        "ours: a ToyOS binary this build produced (toyos-symbols/tests/real.rs)",
        Terms::Spdx("MIT OR Apache-2.0"),
    ),
];

/// `NOTICE` sections that name no files, and why.
const PROSE: &[(&str, &str)] = &[(
    "Rust crates and the forks",
    "every crate, fork or not, is judged by the crate half of this gate",
)];

// --- Expressions -------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Expr {
    Id(String),
    With(String, String),
    And(Vec<Expr>),
    Or(Vec<Expr>),
}

fn tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    for c in text.chars() {
        if c.is_whitespace() || matches!(c, '(' | ')' | '/') {
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            if !c.is_whitespace() {
                out.push(if c == '/' {
                    "OR".to_string()
                } else {
                    c.to_string()
                });
            }
        } else {
            word.push(c);
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

fn parse(text: &str) -> Result<Expr, String> {
    let tokens = tokens(text);
    let mut at = 0;
    let expr = parse_or(&tokens, &mut at)?;
    if at != tokens.len() {
        return Err(format!("{text:?}: unexpected {:?}", tokens[at]));
    }
    Ok(expr)
}

fn parse_or(tokens: &[String], at: &mut usize) -> Result<Expr, String> {
    let mut parts = vec![parse_and(tokens, at)?];
    while tokens.get(*at).is_some_and(|t| t == "OR") {
        *at += 1;
        parts.push(parse_and(tokens, at)?);
    }
    Ok(if parts.len() == 1 {
        parts.remove(0)
    } else {
        Expr::Or(parts)
    })
}

fn parse_and(tokens: &[String], at: &mut usize) -> Result<Expr, String> {
    let mut parts = vec![parse_with(tokens, at)?];
    while tokens.get(*at).is_some_and(|t| t == "AND") {
        *at += 1;
        parts.push(parse_with(tokens, at)?);
    }
    Ok(if parts.len() == 1 {
        parts.remove(0)
    } else {
        Expr::And(parts)
    })
}

fn is_operand(token: &str) -> bool {
    !matches!(token, "OR" | "AND" | "WITH" | "(" | ")")
}

fn parse_with(tokens: &[String], at: &mut usize) -> Result<Expr, String> {
    let first = match tokens.get(*at).map(String::as_str) {
        Some("(") => {
            *at += 1;
            let inner = parse_or(tokens, at)?;
            if tokens.get(*at).map(String::as_str) != Some(")") {
                return Err("an unclosed `(`".to_string());
            }
            *at += 1;
            return Ok(inner);
        }
        Some(id) if is_operand(id) => id.to_string(),
        Some(other) => return Err(format!("{other:?} where a licence belongs")),
        None => return Err("an expression that ends where a licence belongs".to_string()),
    };
    *at += 1;
    if tokens.get(*at).is_some_and(|t| t == "WITH") {
        *at += 1;
        match tokens.get(*at) {
            Some(exception) if is_operand(exception) => {
                *at += 1;
                return Ok(Expr::With(first, exception.clone()));
            }
            _ => return Err(format!("`{first} WITH` names no exception")),
        }
    }
    Ok(Expr::Id(first))
}

/// The leaves that keep `expr` from passing; empty when it passes.
fn refused(expr: &Expr, font: bool) -> Vec<String> {
    match expr {
        Expr::Id(id) => {
            let ok = ALLOWED.contains(&id.as_str()) || (font && FONTS_ONLY.contains(&id.as_str()));
            if ok {
                vec![]
            } else {
                vec![id.clone()]
            }
        }
        Expr::With(id, exception) => {
            if ALLOWED_WITH.contains(&(id.as_str(), exception.as_str())) {
                vec![]
            } else {
                vec![format!("{id} WITH {exception}")]
            }
        }
        Expr::And(parts) => parts.iter().flat_map(|p| refused(p, font)).collect(),
        Expr::Or(parts) => {
            let each: Vec<Vec<String>> = parts.iter().map(|p| refused(p, font)).collect();
            if each.iter().any(Vec::is_empty) {
                vec![]
            } else {
                each.concat()
            }
        }
    }
}

/// Whether `expr` names one of [`NAMED`] on a branch that passes.
fn names(expr: &Expr) -> bool {
    match expr {
        Expr::Id(id) => NAMED.contains(&id.as_str()),
        Expr::With(..) => false,
        Expr::And(parts) => parts.iter().any(names),
        Expr::Or(parts) => parts
            .iter()
            .filter(|p| refused(p, false).is_empty())
            .all(names),
    }
}

/// Why `licence` does not pass, or `None` when it does.
fn judge_licence(licence: &str, font: bool) -> Option<String> {
    match parse(licence) {
        Err(why) => Some(format!("not an SPDX expression: {why}")),
        Ok(expr) => {
            let leaves = refused(&expr, font);
            (!leaves.is_empty()).then(|| format!("not allowed: {}", leaves.join(", ")))
        }
    }
}

// --- Findings ----------------------------------------------------------------

/// Which kind of thing a finding is about, for matching a [`Subject`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Crate,
    Notice,
    File,
}

/// One shipped thing that does not pass.
#[derive(Debug, PartialEq)]
struct Finding {
    subject: String,
    kind: Kind,
    /// What exceptions match on: the package name, section path or file.
    key: String,
    /// The licence text, empty when none is declared.
    licence: String,
    why: String,
    /// Who pulls it in, root first: one chain per workspace that does.
    via: Vec<String>,
    /// The `cfg` of every edge into it from a shipped package, `None` for an
    /// unconditional one and for a root.
    into: BTreeSet<Option<String>>,
}

impl Finding {
    /// A finding about a section or a file, which nothing reaches by an edge.
    fn at(kind: Kind, key: &str, licence: &str, why: String) -> Self {
        let (subject, via) = match kind {
            Kind::Notice => (format!("NOTICE section {key}"), "NOTICE"),
            Kind::File => (format!("file {key}"), "COMMITTED_FILES"),
            Kind::Crate => unreachable!("a crate's finding carries the chain that reached it"),
        };
        Finding {
            subject,
            kind,
            key: key.to_string(),
            licence: licence.to_string(),
            why,
            via: vec![via.to_string()],
            into: BTreeSet::from([None]),
        }
    }
}

/// What one run found: refusals, and the lines a green verdict still prints.
#[derive(Default)]
struct Report {
    findings: Vec<Finding>,
    named: BTreeSet<String>,
    notes: Vec<String>,
}

impl Report {
    /// Record `f`, folded into an earlier finding on the same thing: the
    /// kernel and the bootloader resolve one path crate twice.
    fn find(&mut self, f: Finding) {
        match self
            .findings
            .iter_mut()
            .find(|e| (e.kind, &e.key, &e.licence) == (f.kind, &f.key, &f.licence))
        {
            Some(earlier) => {
                earlier.into.extend(f.into);
                earlier.via.extend(f.via);
            }
            None => self.findings.push(f),
        }
    }

    /// Judge `finding`'s licence, and record it as a refusal or an MPL name.
    fn judge(&mut self, finding: Finding, font: bool) {
        match judge_licence(&finding.licence, font) {
            Some(why) => self.find(Finding { why, ..finding }),
            None if parse(&finding.licence).is_ok_and(|e| names(&e)) => {
                let (subject, licence, via) = (finding.subject, finding.licence, finding.via);
                self.named.insert(format!("{subject} is {licence}, via {}", via.join("; ")));
            }
            None => {}
        }
    }
}

// --- Crates ------------------------------------------------------------------

/// One normal edge: the dependency, and the `cfg` it is taken under when every
/// declaration of it is target-conditional.
type Edge<'a> = (&'a str, Option<String>);

/// The package graph `cargo metadata` resolved.
struct Graph<'a> {
    packages: BTreeMap<&'a str, &'a Value>,
    normal: BTreeMap<&'a str, Vec<Edge<'a>>>,
}

fn str_of<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key).and_then(Value::as_str)
}

impl<'a> Graph<'a> {
    fn new(metadata: &'a Value) -> Result<Self, String> {
        let packages = metadata["packages"]
            .as_array()
            .ok_or("cargo metadata printed no packages")?
            .iter()
            .map(|p| Ok((str_of(p, "id").ok_or("a package with no id")?, p)))
            .collect::<Result<_, String>>()?;
        let nodes = metadata["resolve"]["nodes"]
            .as_array()
            .ok_or("cargo metadata printed no resolve graph")?;
        let mut normal = BTreeMap::new();
        for node in nodes {
            let id = str_of(node, "id").ok_or("a node with no id")?;
            let deps = node["deps"].as_array().ok_or("a node with no deps")?;
            let mut out = Vec::new();
            for dep in deps {
                let kinds = dep["dep_kinds"]
                    .as_array()
                    .ok_or("a dep with no dep_kinds")?;
                let normal: Vec<&Value> = kinds.iter().filter(|k| k["kind"].is_null()).collect();
                if normal.is_empty() {
                    continue;
                }
                let cfg = normal
                    .iter()
                    .map(|k| str_of(k, "target"))
                    .collect::<Option<Vec<_>>>()
                    .map(|targets| targets.join(" | "));
                out.push((str_of(dep, "pkg").ok_or("a dep with no pkg")?, cfg));
            }
            normal.insert(id, out);
        }
        Ok(Self { packages, normal })
    }

    /// The id of the package whose manifest is `manifest`.
    fn id_of(&self, manifest: &Path) -> Option<&'a str> {
        self.packages
            .iter()
            .find(|(_, p)| str_of(p, "manifest_path").map(Path::new) == Some(manifest))
            .map(|(id, _)| *id)
    }

    fn edges(&self, id: &str) -> &[Edge<'a>] {
        self.normal.get(id).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Every package `roots` reach over normal edges, each with a chain to it:
    /// an unconditional one wherever there is one.
    fn reach(&self, roots: &[&'a str]) -> BTreeMap<&'a str, String> {
        let mut chains: BTreeMap<&str, String> = roots
            .iter()
            .map(|r| (*r, self.name(r).to_string()))
            .collect();
        for conditional in [false, true] {
            let mut queue: VecDeque<&str> = chains.keys().copied().collect();
            while let Some(id) = queue.pop_front() {
                for (dep, cfg) in self.edges(id) {
                    if chains.contains_key(dep) || (cfg.is_some() && !conditional) {
                        continue;
                    }
                    let edge = cfg.as_ref().map(|c| format!(" [{c}]")).unwrap_or_default();
                    let chain = format!("{} → {}{edge}", chains[id], self.name(dep));
                    chains.insert(dep, chain);
                    queue.push_back(dep);
                }
            }
        }
        chains
    }

    fn name(&self, id: &str) -> &'a str {
        str_of(self.packages[id], "name").unwrap_or("?")
    }
}

/// The manifests of `metadata`'s workspace members: the packages it resolved
/// with the features it was asked for. A path dependency outside the workspace
/// is in `packages` too, resolved with its dependent's features, so it is not
/// one of these.
fn members(metadata: &Value) -> Result<BTreeSet<PathBuf>, String> {
    let graph = Graph::new(metadata)?;
    metadata["workspace_members"]
        .as_array()
        .ok_or("cargo metadata printed no workspace members")?
        .iter()
        .map(|id| {
            let package = id.as_str().and_then(|id| graph.packages.get(id));
            package
                .and_then(|p| str_of(p, "manifest_path"))
                .map(PathBuf::from)
                .ok_or_else(|| format!("workspace member {id} is no package"))
        })
        .collect()
}

/// The path packages a document's shipped roots reach: their directories, and
/// their build scripts.
#[derive(Debug, Default)]
struct Local {
    dirs: BTreeSet<PathBuf>,
    scripts: BTreeSet<PathBuf>,
}

/// Judge every package `roots` reach in one metadata document, and return the
/// path packages among them.
fn judge_crates(metadata: &Value, roots: &[PathBuf], report: &mut Report) -> Result<Local, String> {
    let graph = Graph::new(metadata)?;
    let ids = roots
        .iter()
        .map(|root| {
            let manifest = root.join("Cargo.toml");
            graph
                .id_of(&manifest)
                .ok_or_else(|| format!("cargo metadata has no package at {}", manifest.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let reached = graph.reach(&ids);
    let mut into: BTreeMap<&str, BTreeSet<Option<String>>> =
        ids.iter().map(|id| (*id, BTreeSet::from([None]))).collect();
    for from in reached.keys() {
        for (dep, cfg) in graph.edges(from) {
            into.entry(dep).or_default().insert(cfg.clone());
        }
    }
    let mut local = Local::default();
    for (dep, via) in &reached {
        let package = graph.packages[dep];
        let name = graph.name(dep);
        let version = str_of(package, "version").unwrap_or("?");
        let manifest = str_of(package, "manifest_path").unwrap_or("?");
        let source = str_of(package, "source");
        if source.is_none() {
            if let Some(dir) = Path::new(manifest).parent() {
                local.dirs.insert(dir.to_path_buf());
            }
            let targets = package["targets"].as_array().map(Vec::as_slice).unwrap_or(&[]);
            local.scripts.extend(
                targets
                    .iter()
                    .filter(|t| t["kind"].as_array().is_some_and(|k| k.contains(&"custom-build".into())))
                    .filter_map(|t| str_of(t, "src_path").map(PathBuf::from)),
            );
        }
        let finding = |licence: &str, why: String| Finding {
            subject: format!("crate {name} {version} ({})", source.unwrap_or(manifest)),
            kind: Kind::Crate,
            key: name.to_string(),
            licence: licence.to_string(),
            why,
            via: vec![via.clone()],
            into: into[dep].clone(),
        };
        match (str_of(package, "license"), str_of(package, "license_file")) {
            (Some(licence), _) => report.judge(finding(licence, String::new()), false),
            (None, Some(file)) => report.find(finding(
                "",
                format!("declares only a licence file, {file}, which no gate reads"),
            )),
            (None, None) => report.find(finding("", "declares no licence".to_string())),
        }
    }
    Ok(local)
}

// --- Committed files ---------------------------------------------------------

/// Where committed files ship from, as root-relative paths.
#[derive(Default)]
struct Shipping {
    /// The shipped configs' asset directories.
    assets: BTreeSet<String>,
    /// The shipped path packages' directories.
    packages: BTreeSet<String>,
    /// Every file name a shipped path package [`embeds`].
    named: BTreeSet<String>,
}

fn under(dirs: &BTreeSet<String>, file: &str) -> bool {
    dirs.iter().any(|d| file.starts_with(&format!("{d}/")))
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

impl Shipping {
    fn ships(&self, file: &str) -> bool {
        under(&self.assets, file) || under(&self.packages, file) || self.named.contains(file_name(file))
    }
}

type Row = (&'static str, &'static str, &'static str, Terms);

/// The names among `names` that `text` embeds in an image: any its build
/// script names, and any on a line of Rust source that `include_bytes!` or
/// `include_str!` is on.
fn embeds<'a>(text: &str, script: bool, names: &BTreeSet<&'a str>) -> BTreeSet<&'a str> {
    text.lines()
        .filter(|l| script || l.contains("include_bytes!") || l.contains("include_str!"))
        .flat_map(|line| names.iter().copied().filter(move |n| line.contains(n)))
        .collect()
}

/// Judge every committed file an image ships by its row in `ledger`, and refuse
/// a file under a shipped asset directory that has none.
fn judge_files(ledger: &[Row], tracked: &[String], shipping: &Shipping, report: &mut Report) {
    for file in tracked.iter().filter(|f| under(&shipping.assets, f)) {
        if !ledger.iter().any(|(path, ..)| path == file) {
            report.find(Finding::at(
                Kind::File,
                file,
                "",
                "is under a shipped asset directory and no COMMITTED_FILES row gives its licence"
                    .to_string(),
            ));
        }
    }
    for (path, _, _, terms) in ledger {
        if shipping.ships(path) {
            let font = matches!(terms, Terms::Font(_));
            report.judge(Finding::at(Kind::File, path, terms.expr(), String::new()), font);
        }
    }
}

// --- NOTICE ------------------------------------------------------------------

/// One `NOTICE` section: its heading, the path the heading starts with, and the
/// SPDX lines it carries.
#[derive(Debug, PartialEq)]
struct Section {
    heading: String,
    path: String,
    spdx: Vec<String>,
}

const SPDX_TAG: &str = "SPDX-License-Identifier:";

/// The `-`-underlined sections of `text`, in order.
fn sections(text: &str) -> Vec<Section> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<Section> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let underline = lines
            .get(i + 1)
            .is_some_and(|u| u.len() >= 3 && u.chars().all(|c| c == '-'));
        if underline && !line.trim().is_empty() {
            out.push(Section {
                heading: line.trim().to_string(),
                path: line.split_whitespace().next().unwrap_or("").to_string(),
                spdx: Vec::new(),
            });
        } else if let (Some(section), Some(expr)) =
            (out.last_mut(), line.trim().strip_prefix(SPDX_TAG))
        {
            section.spdx.push(expr.trim().to_string());
        }
    }
    out
}

/// Judge `NOTICE`'s sections. `files` holds the tracked files each section's
/// path names.
fn judge_notice(
    sections: &[Section],
    files: &BTreeMap<String, Vec<String>>,
    ledger: &[Row],
    shipping: &Shipping,
    report: &mut Report,
) {
    let refuse = |report: &mut Report, path: &str, why: String| {
        report.find(Finding::at(Kind::Notice, path, "", why));
    };
    for section in sections {
        let path = section.path.as_str();
        let prose = PROSE.iter().find(|(heading, _)| *heading == section.heading);
        let expr = match (prose, section.spdx.as_slice()) {
            (Some((heading, why)), []) => {
                report.notes.push(format!("NOTICE section {heading:?} names no files: {why}"));
                continue;
            }
            (Some(_), _) => {
                refuse(report, path, "is PROSE and carries an SPDX line".to_string());
                continue;
            }
            (None, [expr]) => expr,
            (None, _) => {
                let n = section.spdx.len();
                refuse(report, path, format!("carries {n} `{SPDX_TAG}` lines, not one"));
                continue;
            }
        };
        let named = files.get(path).map(Vec::as_slice).unwrap_or(&[]);
        if named.is_empty() {
            refuse(report, path, "names no file git tracks".to_string());
            continue;
        }
        let mut rest = Vec::new();
        for file in named {
            match ledger.iter().find(|(p, ..)| p == file) {
                Some((_, _, _, terms)) if terms.expr() != expr => refuse(
                    report,
                    path,
                    format!("says {expr} and COMMITTED_FILES says {} for {file}", terms.expr()),
                ),
                Some(_) => {}
                None => rest.push(file),
            }
        }
        if rest.is_empty() {
            continue;
        }
        if !rest.iter().any(|f| shipping.ships(f)) {
            report.notes.push(format!("NOTICE section {path} ({expr}) is not shipped"));
            continue;
        }
        report.judge(Finding::at(Kind::Notice, path, expr, String::new()), false);
    }
}

// --- The verdict -------------------------------------------------------------

/// Match `report` against `exceptions`: the refusals left, and the lines to print.
fn verdict(report: Report, exceptions: &[Exception]) -> Result<String, String> {
    let mut used = vec![false; exceptions.len()];
    let mut red = Vec::new();
    let mut excepted = Vec::new();
    for f in &report.findings {
        let matched = exceptions.iter().position(|e| {
            let key = match e.subject {
                Subject::Crate(name) => f.kind == Kind::Crate && name == f.key,
                Subject::Notice(path) => f.kind == Kind::Notice && path == f.key,
                Subject::File(path) => f.kind == Kind::File && path == f.key,
            };
            let standing = match e.standing {
                Standing::PendingOwner(_) => true,
                Standing::OnlyUnder(cfg) => {
                    !f.into.is_empty() && f.into.iter().all(|c| c.as_deref() == Some(cfg))
                }
            };
            key && standing && e.licence == f.licence
        });
        let line = format!(
            "{} is {:?}: {}; pulled in by {}",
            f.subject,
            f.licence,
            f.why,
            f.via.join("; ")
        );
        match matched {
            Some(i) => {
                used[i] = true;
                let standing = match exceptions[i].standing {
                    Standing::PendingOwner(issue) => format!("pending the owner, {issue}"),
                    Standing::OnlyUnder(cfg) => format!("reached only under {cfg}"),
                };
                excepted.push(format!(
                    "{line}\n    EXCEPTED, {standing}: {}",
                    exceptions[i].reason
                ));
            }
            None => red.push(line),
        }
    }
    for (e, used) in exceptions.iter().zip(used) {
        if !used {
            red.push(format!(
                "exception {:?} ({:?}) matches nothing shipped: delete it",
                e.subject, e.licence
            ));
        }
    }
    let mut out = String::new();
    for line in report.notes.iter().chain(&report.named).chain(&excepted) {
        out.push_str(&format!("  {line}\n"));
    }
    if red.is_empty() {
        println!("{out}");
        Ok(format!(
            "{} exception(s) stand, and nothing else is refused",
            excepted.len()
        ))
    } else {
        eprintln!("{out}");
        Err(format!("{} refusal(s):\n  {}", red.len(), red.join("\n  ")))
    }
}

// --- Reading the tree --------------------------------------------------------

fn run(cmd: &mut Command, what: &str) -> Result<Vec<u8>, String> {
    let out = cmd.output().map_err(|e| format!("{what}: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "{what} exited {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// `cargo metadata` over the workspace holding `manifest`.
fn metadata(
    root: &Path,
    manifest: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<Value, String> {
    let mut cmd = Command::new("cargo");
    cmd.args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(manifest)
        .args(args)
        .envs(env.iter().copied())
        .current_dir(root);
    let out = run(
        &mut cmd,
        &format!("cargo metadata --manifest-path {}", manifest.display()),
    )?;
    serde_json::from_slice(&out).map_err(|e| format!("cargo metadata printed no JSON: {e}"))
}

/// The tracked files `pathspecs` name, root-relative.
fn ls_files(root: &Path, pathspecs: &[String]) -> Result<Vec<String>, String> {
    let out = run(
        Command::new("git")
            .args(["ls-files", "-z", "--"])
            .args(pathspecs)
            .current_dir(root),
        "git ls-files",
    )?;
    Ok(String::from_utf8_lossy(&out)
        .split('\0')
        .filter(|f| !f.is_empty())
        .map(String::from)
        .collect())
}

/// The fork's `library/`, checked out at the commit this tree pins. A checkout
/// whose `rust/` was never initialised — a CI runner's — fetches that commit
/// alone.
fn std_library(root: &Path) -> Result<PathBuf, String> {
    let fork = crate::sysroot::fork_checkout(root);
    if !fork.join("library/Cargo.toml").exists() {
        run(
            Command::new("git")
                .args(["submodule", "update", "--init", "--depth", "1", "rust"])
                .current_dir(root),
            "git submodule update --init --depth 1 rust",
        )?;
    }
    Ok(fork.join("library"))
}

/// One metadata document, and the shipped crates judged out of it.
struct Doc {
    metadata: Value,
    features: Features,
    members: BTreeSet<PathBuf>,
    roots: Vec<PathBuf>,
}

/// The gate `cargo run -- --ci host` runs.
pub fn judge(root: &Path) -> Result<String, String> {
    // Cargo names every manifest by its canonical path.
    let root = &std::fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let shipped = crate::build::shipped(root)?;
    let mut report = Report::default();

    let mut roots: Vec<(PathBuf, Features)> = shipped.crates.into_iter().collect();
    roots.push((
        root.join(crate::libc::CRATE),
        Features::With(crate::libc::FEATURES),
    ));
    let mut docs: Vec<Doc> = Vec::new();
    for (dir, features) in roots {
        let dir = std::fs::canonicalize(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let manifest = dir.join("Cargo.toml");
        if let Some(doc) = docs
            .iter_mut()
            .find(|d| d.features == features && d.members.contains(&manifest))
        {
            doc.roots.push(dir);
            continue;
        }
        let mut args = features.args();
        args.push("--locked");
        let metadata = metadata(root, &manifest, &args, &[])?;
        let members = members(&metadata)?;
        if !members.contains(&manifest) {
            return Err(format!("{} is no member of its own workspace", manifest.display()));
        }
        docs.push(Doc { metadata, features, members, roots: vec![dir] });
    }

    // Bootstrap re-locks `library/Cargo.lock` to this tree's `toyos-abi` and
    // `toyos` versions on every std build and puts the fork's file back, so the
    // committed lock is stale by design: it is re-locked into a scratch copy,
    // and the fork's is never written. `RUSTC_BOOTSTRAP` because the fork's
    // manifests use cargo features a stable cargo otherwise refuses.
    let library = std_library(root)?;
    let scratch = root.join("target/licence");
    std::fs::create_dir_all(&scratch).map_err(|e| format!("create {}: {e}", scratch.display()))?;
    let lock = scratch.join("Cargo.lock");
    std::fs::copy(library.join("Cargo.lock"), &lock)
        .map_err(|e| format!("copy std's Cargo.lock: {e}"))?;
    let lockfile = format!("resolver.lockfile-path={:?}", lock.display().to_string());
    let mut args = Features::AnyDeclared.args();
    args.extend(["--config", &lockfile]);
    let std_doc = metadata(
        root,
        &library.join("Cargo.toml"),
        &args,
        &[("RUSTC_BOOTSTRAP", "1")],
    )?;
    docs.push(Doc {
        members: members(&std_doc)?,
        metadata: std_doc,
        features: Features::AnyDeclared,
        roots: vec![library.join("std")],
    });

    let mut local = Local::default();
    for doc in &docs {
        let found = judge_crates(&doc.metadata, &doc.roots, &mut report)?;
        local.dirs.extend(found.dirs);
        local.scripts.extend(found.scripts);
    }

    let relative = |dir: &Path| dir.strip_prefix(root).ok().map(|d| d.display().to_string());
    let mut shipping = Shipping {
        assets: shipped.assets.iter().filter_map(|d| relative(d)).collect(),
        packages: local.dirs.iter().filter_map(|d| relative(d)).filter(|d| !d.is_empty()).collect(),
        named: BTreeSet::new(),
    };
    let tracked = ls_files(root, &[])?;
    let names: BTreeSet<&str> = COMMITTED_FILES.iter().map(|(p, ..)| file_name(p)).collect();
    let sources = tracked.iter().filter(|f| under(&shipping.packages, f) && f.ends_with(".rs"));
    let read = sources
        .map(|f| (root.join(f), false))
        .chain(local.scripts.iter().map(|s| (s.clone(), true)));
    for (file, script) in read {
        let text = std::fs::read_to_string(&file)
            .map_err(|e| format!("read {}: {e}", file.display()))?;
        shipping.named.extend(embeds(&text, script, &names).into_iter().map(String::from));
    }
    judge_files(COMMITTED_FILES, &tracked, &shipping, &mut report);

    let notice =
        std::fs::read_to_string(root.join("NOTICE")).map_err(|e| format!("read NOTICE: {e}"))?;
    let sections = sections(&notice);
    let mut files = BTreeMap::new();
    for section in &sections {
        let named = ls_files(root, &[format!(":(glob){}", section.path)])?;
        files.insert(section.path.clone(), named);
    }
    judge_notice(&sections, &files, COMMITTED_FILES, &shipping, &mut report);

    verdict(report, EXCEPTIONS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn verdict_of(licence: &str) -> Option<String> {
        judge_licence(licence, false)
    }

    #[test]
    fn a_disallowed_licence_is_refused_by_name() {
        let why = verdict_of("GPL-2.0-only").expect("GPL passed");
        assert!(why.contains("GPL-2.0-only"), "{why}");
        assert!(verdict_of("LGPL-2.1-or-later").is_some());
        assert!(verdict_of("AGPL-3.0-only").is_some());
        assert!(
            verdict_of("Frobnicate-1.0").is_some(),
            "an unrecognised licence passed"
        );
        assert!(verdict_of("OFL-1.1").is_some(), "OFL passed outside a font");
        assert_eq!(judge_licence("OFL-1.1", true), None);
        assert_eq!(verdict_of("MPL-2.0"), None);
    }

    #[test]
    fn an_or_passes_when_any_branch_does() {
        assert_eq!(verdict_of("MIT OR Apache-2.0"), None);
        assert_eq!(verdict_of("GPL-2.0-only OR MIT"), None);
        assert_eq!(verdict_of("MIT/Apache-2.0"), None);
        assert_eq!(
            verdict_of("Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT"),
            None
        );
        let why = verdict_of("GPL-2.0-only OR LGPL-2.1-only").expect("no branch is allowed");
        assert!(
            why.contains("GPL-2.0-only") && why.contains("LGPL-2.1-only"),
            "{why}"
        );
    }

    #[test]
    fn an_and_passes_only_when_every_part_does() {
        assert_eq!(verdict_of("(MIT OR Apache-2.0) AND Unicode-DFS-2016"), None);
        assert_eq!(
            verdict_of("MIT AND Apache-2.0 WITH LLVM-exception AND (MIT OR Apache-2.0)"),
            None
        );
        let why = verdict_of("MIT AND GPL-3.0-only").expect("one part is GPL");
        assert!(
            why.contains("GPL-3.0-only") && !why.contains("MIT"),
            "{why}"
        );
        assert!(verdict_of("MIT AND (GPL-2.0-only OR LGPL-2.1-only)").is_some());
        assert!(verdict_of("GPL-2.0-only WITH Classpath-exception-2.0").is_some());
    }

    #[test]
    fn a_malformed_expression_is_refused_not_guessed() {
        for bad in [
            "",
            "MIT OR",
            "(MIT",
            "MIT AND AND ISC",
            "Apache-2.0 WITH",
            "MIT ISC",
        ] {
            let why = verdict_of(bad).unwrap_or_else(|| panic!("{bad:?} passed"));
            assert!(why.starts_with("not an SPDX expression"), "{bad:?}: {why}");
        }
    }

    #[test]
    fn mpl_passes_and_is_named() {
        assert!(names(&parse("MPL-2.0").unwrap()));
        assert!(
            !names(&parse("MPL-2.0 OR MIT").unwrap()),
            "an MIT branch needs no MPL"
        );
        assert!(names(&parse("MIT AND MPL-2.0").unwrap()));
    }


    /// The kernel's `--kernel-feature` picks any feature it declares, so its
    /// graph is resolved with all of them; a `[programs]` row's
    /// `no-default-features` and libc's features reach metadata as the build
    /// passes them.
    #[test]
    fn metadata_is_given_the_features_the_build_gives() {
        assert_eq!(Features::AnyDeclared.args(), ["--all-features"]);
        assert_eq!(Features::NoDefault.args(), ["--no-default-features"]);
        assert_eq!(
            Features::With(crate::libc::FEATURES).args(),
            ["--features", "std-runtime"]
        );
        assert!(Features::Default.args().is_empty());
    }

    type Package<'a> = (&'a str, Option<&'a str>, Option<&'a str>, Option<&'a str>);
    type Dep<'a> = (&'a str, &'a str, Option<&'a str>);

    /// A metadata document of `packages`, each `(name, license, source,
    /// license_file)`, and `edges`, each `(from, to, kind)`, where a kind
    /// starting `cfg(` is a normal edge taken under that target. The first
    /// package is the one workspace member.
    fn doc(packages: &[Package], edges: &[Dep]) -> Value {
        let packages: Vec<Value> = packages
            .iter()
            .map(|(name, license, source, file)| {
                json!({
                    "id": name, "name": name, "version": "1.0.0",
                    "license": license, "license_file": file, "source": source,
                    "manifest_path": format!("/t/{name}/Cargo.toml"),
                })
            })
            .collect();
        let nodes: Vec<Value> = packages
            .iter()
            .map(|p| {
                let id = p["id"].as_str().unwrap();
                let deps: Vec<Value> = edges
                    .iter()
                    .filter(|(from, _, _)| *from == id)
                    .map(|(_, to, kind)| {
                        let (kind, target) = match kind {
                            Some(cfg) if cfg.starts_with("cfg(") => (None, Some(cfg)),
                            other => (*other, None),
                        };
                        json!({"name": to, "pkg": to, "dep_kinds": [{"kind": kind, "target": target}]})
                    })
                    .collect();
                json!({"id": id, "deps": deps})
            })
            .collect();
        let member = packages[0]["id"].clone();
        json!({"packages": packages, "workspace_members": [member], "resolve": {"nodes": nodes}})
    }

    fn crates(doc: &Value) -> Report {
        let mut report = Report::default();
        judge_crates(doc, &[PathBuf::from("/t/app")], &mut report).expect("judged");
        report
    }

    const GIT: Option<&str> = Some("git+https://github.com/ToyOSOrg/fork?branch=toyos#abc");
    const CRATES_IO: Option<&str> = Some("registry+https://github.com/rust-lang/crates.io-index");

    #[test]
    fn a_graph_is_walked_over_normal_edges_from_the_shipped_root() {
        let d = doc(
            &[
                ("app", Some("MIT OR Apache-2.0"), None, None),
                ("fine", Some("MIT/Apache-2.0"), CRATES_IO, None),
                ("gpl", Some("GPL-3.0-only"), CRATES_IO, None),
                ("builder", Some("GPL-3.0-only"), CRATES_IO, None),
                ("tester", Some("AGPL-3.0-only"), CRATES_IO, None),
                ("unreached", Some("GPL-3.0-only"), CRATES_IO, None),
                ("bare", None, CRATES_IO, None),
                ("filed", None, CRATES_IO, Some("LICENSE")),
            ],
            &[
                ("app", "fine", None),
                ("fine", "gpl", None),
                ("app", "builder", Some("build")),
                ("app", "tester", Some("dev")),
                ("fine", "bare", None),
                ("fine", "filed", None),
            ],
        );
        let report = crates(&d);
        let keys: Vec<&str> = report.findings.iter().map(|f| f.key.as_str()).collect();
        assert_eq!(keys, ["bare", "filed", "gpl"], "{:?}", report.findings);
        let gpl = &report.findings[2];
        assert_eq!(gpl.via, ["app → fine → gpl"]);
        assert_eq!(gpl.into, BTreeSet::from([None]));
        assert_eq!(gpl.licence, "GPL-3.0-only");
        assert_eq!(report.findings[0].why, "declares no licence");
        assert!(
            report.findings[1]
                .why
                .contains("only a licence file, LICENSE"),
            "{}",
            report.findings[1].why
        );
    }

    /// A path dependency from outside the workspace is in `packages`, resolved
    /// with its dependent's features, and is not what that document was asked
    /// to resolve.
    #[test]
    fn only_a_workspace_member_is_a_documents_own() {
        let d = doc(
            &[("app", Some("MIT"), None, None), ("dep", Some("MIT"), None, None)],
            &[("app", "dep", None)],
        );
        assert_eq!(
            members(&d).unwrap(),
            BTreeSet::from([PathBuf::from("/t/app/Cargo.toml")])
        );
    }

    #[test]
    fn a_cfg_edge_is_walked_and_excepted_only_while_every_edge_in_is_under_it() {
        let packages = [
            ("app", Some("MIT"), None, None),
            ("mid", Some("MIT"), CRATES_IO, None),
            ("shim", None, None, None),
        ];
        let d = doc(
            &packages,
            &[("app", "mid", None), ("mid", "shim", Some("cfg(windows)"))],
        );
        let report = crates(&d);
        assert_eq!(report.findings.len(), 1, "a cfg edge was not walked");
        assert_eq!(report.findings[0].via, ["app → mid → shim [cfg(windows)]"]);
        assert_eq!(
            report.findings[0].into,
            BTreeSet::from([Some("cfg(windows)".to_string())])
        );

        let only = [Exception {
            subject: Subject::Crate("shim"),
            licence: "",
            standing: Standing::OnlyUnder("cfg(windows)"),
            reason: "r",
        }];
        assert!(verdict(report, &only).is_ok());

        // A second way in, unconditional or under another cfg, is not what the
        // exception was written against.
        for second in [None, Some("cfg(target_os = \"toyos\")")] {
            let d = doc(
                &packages,
                &[
                    ("app", "mid", None),
                    ("mid", "shim", Some("cfg(windows)")),
                    ("app", "shim", second),
                ],
            );
            let why = verdict(crates(&d), &only).unwrap_err();
            assert!(
                why.contains("crate shim") && why.contains("matches nothing shipped"),
                "{second:?}: {why}"
            );
        }
    }

    /// Whether `cfg(…)` holds for a target whose `rustc --print cfg` is `set`.
    fn holds(cfg: &str, set: &BTreeSet<String>) -> bool {
        let inner = cfg.strip_prefix("cfg(").and_then(|c| c.strip_suffix(')'));
        predicate(inner.unwrap_or_else(|| panic!("{cfg:?} is no cfg(…)")), set)
    }

    fn predicate(p: &str, set: &BTreeSet<String>) -> bool {
        let p = p.trim();
        for (op, all) in [("any(", false), ("all(", true), ("not(", true)] {
            let Some(args) = p.strip_prefix(op).and_then(|a| a.strip_suffix(')')) else {
                continue;
            };
            let (mut parts, mut depth, mut start) = (Vec::new(), 0, 0);
            for (i, c) in args.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => depth -= 1,
                    ',' if depth == 0 => {
                        parts.push(&args[start..i]);
                        start = i + 1;
                    }
                    _ => {}
                }
            }
            parts.push(&args[start..]);
            let each: Vec<bool> =
                parts.iter().filter(|a| !a.trim().is_empty()).map(|a| predicate(a, set)).collect();
            return match op {
                "not(" => !each.iter().all(|b| *b),
                _ if all => each.iter().all(|b| *b),
                _ => each.iter().any(|b| *b),
            };
        }
        let key: String = p.chars().filter(|c| !c.is_whitespace()).collect();
        set.contains(&key)
    }

    /// `rustc +toyos --print cfg --target x86_64-unknown-toyos`, verbatim.
    const TOYOS_CFG: &str = r#"debug_assertions
fmt_debug="full"
overflow_checks
panic="unwind"
relocation_model="pic"
target_abi=""
target_arch="x86_64"
target_endian="little"
target_env=""
target_feature="fxsr"
target_feature="sse"
target_feature="sse2"
target_feature="x87"
target_has_atomic
target_has_atomic="16"
target_has_atomic="32"
target_has_atomic="64"
target_has_atomic="8"
target_has_atomic="ptr"
target_has_atomic_load_store
target_has_atomic_load_store="16"
target_has_atomic_load_store="32"
target_has_atomic_load_store="64"
target_has_atomic_load_store="8"
target_has_atomic_load_store="ptr"
target_has_atomic_primitive_alignment="16"
target_has_atomic_primitive_alignment="32"
target_has_atomic_primitive_alignment="64"
target_has_atomic_primitive_alignment="8"
target_has_atomic_primitive_alignment="ptr"
target_has_reliable_f128
target_has_reliable_f16
target_has_reliable_f16_math
target_has_threads
target_object_format="elf"
target_os="toyos"
target_pointer_width="64"
target_thread_local
target_vendor="unknown"
ub_checks"#;

    /// The ToyOS target satisfies no [`Standing::OnlyUnder`] `cfg`, and a
    /// Windows one satisfies every one, so the reader is not blind.
    #[test]
    fn no_guest_target_satisfies_an_only_under_cfg() {
        let toyos: BTreeSet<String> = TOYOS_CFG.lines().map(String::from).collect();
        let windows: BTreeSet<String> = ["windows", "target_os=\"windows\"", "target_family=\"windows\""]
            .map(String::from)
            .into();
        let mut checked = 0;
        for e in EXCEPTIONS {
            if let Standing::OnlyUnder(cfg) = e.standing {
                assert!(!holds(cfg, &toyos), "the ToyOS target satisfies {cfg}");
                assert!(holds(cfg, &windows), "a Windows target does not satisfy {cfg}");
                checked += 1;
            }
        }
        assert_eq!(checked, 2);
        assert!(holds("cfg(not(windows))", &toyos));
        assert!(holds("cfg(all(target_os = \"toyos\", target_arch = \"x86_64\"))", &toyos));
    }

    #[test]
    fn a_git_dependency_is_judged_like_any_other() {
        let d = doc(
            &[
                ("app", Some("MIT"), None, None),
                ("fork", Some("LGPL-2.1-only"), GIT, None),
            ],
            &[("app", "fork", None)],
        );
        let report = crates(&d);
        assert_eq!(report.findings.len(), 1);
        assert!(
            report.findings[0]
                .subject
                .contains("git+https://github.com/ToyOSOrg/fork"),
            "{:?}",
            report.findings
        );
        let d = doc(
            &[
                ("app", Some("MIT"), None, None),
                ("fork", Some("MIT OR Apache-2.0"), GIT, None),
            ],
            &[("app", "fork", None)],
        );
        assert!(crates(&d).findings.is_empty());
    }

    #[test]
    fn a_root_the_metadata_does_not_hold_is_refused() {
        let d = doc(&[("other", Some("MIT"), None, None)], &[]);
        let mut report = Report::default();
        let why = judge_crates(&d, &[PathBuf::from("/t/app")], &mut report).unwrap_err();
        assert!(why.contains("/t/app/Cargo.toml"), "{why}");
    }

    const LEDGER: &[Row] = &[
        ("assets/font.ttf", "", "NOTICE", Terms::Font("OFL-1.1")),
        ("assets/game.wad", "", "NOTICE", Terms::Spdx("LicenseRef-Shareware")),
        ("assets/icons/a.svg", "", "NOTICE", Terms::Spdx("MIT")),
        ("assets/ours.jpg", "", "ours", Terms::Spdx("MIT OR Apache-2.0")),
        ("assets/notes.txt", "", "ours", Terms::Spdx("OFL-1.1")),
        ("kernel/raster.bin", "", "NOTICE", Terms::Font("OFL-1.1")),
        ("tests/fixture.bin", "", "ours", Terms::Spdx("GPL-3.0-only")),
        ("elsewhere/embedded.otf", "", "ours", Terms::Spdx("GPL-2.0-only")),
    ];

    fn tracked() -> Vec<String> {
        [
            "assets/font.ttf",
            "assets/game.wad",
            "assets/icons/a.svg",
            "assets/ours.jpg",
            "assets/notes.txt",
            "assets/stray.otf",
            "kernel/raster.bin",
            "kernel/src/main.rs",
            "tests/fixture.bin",
            "tests/corpus/x.c",
            "elsewhere/embedded.otf",
            "userland/app/src/main.rs",
        ]
        .map(String::from)
        .to_vec()
    }

    fn shipping() -> Shipping {
        Shipping {
            assets: BTreeSet::from(["assets".to_string()]),
            packages: BTreeSet::from(["kernel".to_string(), "userland/app".to_string()]),
            named: BTreeSet::from(["embedded.otf".to_string()]),
        }
    }

    fn keys(report: &Report) -> Vec<(&str, &str)> {
        report
            .findings
            .iter()
            .map(|f| (f.key.as_str(), f.licence.as_str()))
            .collect()
    }

    /// A file ships from an asset directory, a package's directory, or a
    /// package's mention of it; one under an asset directory with no row is
    /// refused rather than passed unread.
    #[test]
    fn a_committed_file_is_judged_by_its_row_when_it_ships() {
        let mut report = Report::default();
        judge_files(LEDGER, &tracked(), &shipping(), &mut report);
        assert_eq!(
            keys(&report),
            [
                ("assets/stray.otf", ""),
                ("assets/game.wad", "LicenseRef-Shareware"),
                ("assets/notes.txt", "OFL-1.1"),
                ("elsewhere/embedded.otf", "GPL-2.0-only"),
            ],
            "{:?}",
            report.findings
        );
        assert!(report.findings[0].why.contains("no COMMITTED_FILES row"));
    }

    /// A build script embeds what it names; other Rust source only what an
    /// `include_bytes!` or `include_str!` line names, so a comment is no embed.
    #[test]
    fn an_embed_is_what_a_build_script_or_an_include_names() {
        let names = BTreeSet::from(["x.ttf", "y.bin", "z.fd"]);
        let script = "let ttf = fs::read(\"../../assets/x.ttf\");";
        assert_eq!(embeds(script, true, &names), BTreeSet::from(["x.ttf"]));
        assert!(embeds(script, false, &names).is_empty());
        let source = "// boots z.fd\nstatic F: &[u8] = include_bytes!(\"y.bin\");";
        assert_eq!(embeds(source, false, &names), BTreeSet::from(["y.bin"]));
    }

    /// OFL passes on a [`Terms::Font`] row — a `.ttf` or a raster of one — and
    /// on no other, whatever the file is called.
    #[test]
    fn a_font_licence_does_not_reach_a_row_that_is_no_font() {
        let mut report = Report::default();
        judge_files(LEDGER, &[], &shipping(), &mut report);
        let ofl: Vec<&str> = report
            .findings
            .iter()
            .filter(|f| f.licence == "OFL-1.1")
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(ofl, ["assets/notes.txt"], "{:?}", report.findings);
    }

    const NOTICE: &str = "\
Title
=====

assets/font.ttf — a font
------------------------

    SPDX-License-Identifier: OFL-1.1

assets/game.wad — a game
------------------------

    SPDX-License-Identifier: LicenseRef-Shareware

tests/corpus/ — test input
--------------------------

    SPDX-License-Identifier: LGPL-2.1-or-later

userland/app — vendored C
-------------------------

    SPDX-License-Identifier: GPL-2.0-or-later

Rust crates and the forks
-------------------------

prose.
";

    fn notice(text: &str) -> Report {
        let files = BTreeMap::from([
            ("assets/font.ttf".to_string(), vec!["assets/font.ttf".to_string()]),
            ("assets/game.wad".to_string(), vec!["assets/game.wad".to_string()]),
            ("tests/corpus/".to_string(), vec!["tests/corpus/x.c".to_string()]),
            ("userland/app".to_string(), vec!["userland/app/src/main.rs".to_string()]),
        ]);
        let mut report = Report::default();
        judge_notice(&sections(text), &files, LEDGER, &shipping(), &mut report);
        report
    }

    /// A section over files with rows is judged through them; one over files
    /// without is judged itself when one ships.
    #[test]
    fn a_notice_section_is_judged_when_what_no_row_names_ships() {
        let report = notice(NOTICE);
        assert_eq!(
            keys(&report),
            [("userland/app", "GPL-2.0-or-later")],
            "{:?}",
            report.findings
        );
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("tests/corpus/") && n.contains("not shipped")),
            "{:?}",
            report.notes
        );
        assert!(
            report
                .notes
                .iter()
                .any(|n| n.contains("Rust crates and the forks")),
            "{:?}",
            report.notes
        );
    }

    #[test]
    fn a_notice_section_that_disagrees_with_a_row_is_refused() {
        let report = notice(&NOTICE.replace("LicenseRef-Shareware", "MIT"));
        assert_eq!(keys(&report).len(), 2, "{:?}", report.findings);
        assert!(
            report.findings[0].key == "assets/game.wad"
                && report.findings[0].why.contains("COMMITTED_FILES says LicenseRef-Shareware"),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn a_notice_section_without_one_spdx_line_or_without_files_is_refused() {
        let missing = NOTICE.replace("    SPDX-License-Identifier: GPL-2.0-or-later\n", "");
        let report = notice(&missing);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.key == "userland/app" && f.why.contains("0 `SPDX")),
            "{:?}",
            report.findings
        );
        let stale = NOTICE.replace("userland/app — vendored C", "userland/gone — vendored");
        let report = notice(&stale);
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.key == "userland/gone" && f.why.contains("names no file")),
            "{:?}",
            report.findings
        );
    }

    #[test]
    fn an_exception_matches_by_name_and_licence_and_a_stale_one_reds() {
        let finding = |licence: &str| Finding {
            subject: "crate x".into(),
            kind: Kind::Crate,
            key: "x".into(),
            licence: licence.into(),
            why: "not allowed".into(),
            via: vec!["app → x".into()],
            into: BTreeSet::from([None]),
        };
        let exception = [pending(Subject::Crate("x"), "GPL-2.0-only", "i", "r")];
        let report = Report {
            findings: vec![finding("GPL-2.0-only")],
            ..Report::default()
        };
        assert!(verdict(report, &exception).is_ok());
        let report = Report {
            findings: vec![finding("GPL-3.0-only")],
            ..Report::default()
        };
        let why = verdict(report, &exception).unwrap_err();
        assert!(
            why.contains("GPL-3.0-only") && why.contains("matches nothing shipped"),
            "{why}"
        );
        for other in [Subject::Notice("x"), Subject::File("x")] {
            let report = Report {
                findings: vec![finding("GPL-2.0-only")],
                ..Report::default()
            };
            assert!(
                verdict(report, &[pending(other, "GPL-2.0-only", "i", "r")]).is_err(),
                "{other:?} excused a crate"
            );
        }
    }

    #[test]
    fn every_pending_exception_cites_an_issue_that_exists() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for e in EXCEPTIONS {
            if let Standing::PendingOwner(issue) = e.standing {
                assert!(root.join(issue).is_file(), "{:?} cites {issue}, which is no file", e.subject);
            }
        }
    }
}
