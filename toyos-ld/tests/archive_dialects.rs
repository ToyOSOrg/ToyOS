//! `ar` has no specification, and the two dialects in circulation disagree
//! about where a member's name is kept. toyos-ld has to read both: the host
//! that builds ToyOS decides which one the C archive in `userland/doom` arrives
//! in, and nothing in the tree names that host.
//!
//! * **GNU/System V** — the name is in the header's 16-byte name field,
//!   slash-terminated; a name too long for it goes in a `//` member and the
//!   header holds `/<offset>`. The symbol table is a first member named `/`.
//! * **BSD/Darwin (cctools)** — `#1/<n>` in the name field means the name is
//!   the first `n` bytes of the member *data*, NUL-padded, and `n` is counted
//!   in the member's size. The symbol table is a member named `__.SYMDEF` or
//!   `__.SYMDEF SORTED`, which is itself long enough to arrive through `#1/`.
//!
//! What makes this worth a test rather than a comment is the failure mode. A
//! reader that takes the header's name field literally does not fault on a BSD
//! archive: it sees `#1/24`, decides that is not an object, drops the member,
//! and links an archive that defines nothing — and the linker then reports one
//! undefined symbol from a container it never said it could not read. Both
//! cases below therefore assert on the *link*, not on a member list.
//!
//! The dialects also differ in ways this file deliberately does not smooth
//! over: BSD pads names and members to 8 bytes where GNU pads to 2, so the
//! member data a BSD reader hands back is the same object under a different
//! byte offset and, for an odd-sized member, a different length. The output
//! must not be able to tell.

mod common;

use common::{archive, bsd_archive, Case, ObjBuilder, RET};
use object::{SymbolKind, SymbolScope};

/// Members whose names are too long for a GNU header's 16 bytes — which is the
/// length `cc` gives them (`<hash>-<source>.o`) and the length at which the two
/// dialects stop agreeing about anything.
fn members() -> Vec<(&'static str, Vec<u8>)> {
    let mut out: Vec<(&'static str, Vec<u8>)> = Vec::new();
    for (i, name) in ["375a3c652c72663f-doomgeneric.o", "375a3c652c72663f-i_video.o", "short.o"]
        .into_iter()
        .enumerate()
    {
        let mut b = ObjBuilder::new();
        b.data(&format!("DATUM_{i}"), &[i as u8; 8], SymbolScope::Linkage);
        b.text(&format!("member_fn_{i}"), &[RET], SymbolScope::Linkage);
        // An odd-length member: GNU pads it to 2, BSD to 8, so the member after
        // it sits at a different offset in the two containers.
        b.text(&format!("member_odd_{i}"), &[RET, RET, RET], SymbolScope::Compilation);
        out.push((name, b.finish()));
    }
    out
}

/// A `_start` that both calls into the archive and takes the address of a datum
/// through the GOT — the second is how a Rust `extern static` reaches a C
/// definition, and it is the reference that reported `DG_ScreenBuffer` when the
/// archive turned out to hold nothing.
fn start_object() -> Vec<u8> {
    let mut b = ObjBuilder::new();
    let data: Vec<_> = (0..3).map(|i| b.undefined(&format!("DATUM_{i}"), SymbolKind::Data)).collect();
    let calls: Vec<_> =
        (0..3).map(|i| b.undefined(&format!("member_fn_{i}"), SymbolKind::Text)).collect();
    b.got_loader("_loads", &data, SymbolScope::Compilation);
    b.caller("_start", &calls, SymbolScope::Linkage);
    b.finish()
}

fn linked(lib: Vec<u8>, tag: &str) -> Vec<u8> {
    Case::new(tag).input("start.o", start_object()).input("libm.a", lib).link()
}

/// The fixture is the claim, so it is checked before anything is concluded
/// from it: a `bsd_archive` that quietly grew a GNU header would make the case
/// below pass while testing nothing.
#[test]
fn the_bsd_fixture_is_bsd() {
    let bytes = bsd_archive(&members());
    assert_eq!(&bytes[..8], b"!<arch>\n");

    // The symbol table is the first member and is BSD's, not GNU's.
    assert_eq!(&bytes[8..11], b"#1/", "the first member's name is not extended BSD-style");
    let symdef_name_len: usize =
        std::str::from_utf8(&bytes[11..24]).unwrap().trim().parse().unwrap();
    let symdef_name = &bytes[68..68 + symdef_name_len];
    assert!(
        symdef_name.starts_with(b"__.SYMDEF SORTED\0"),
        "first member is {:?}, not a BSD symbol table",
        String::from_utf8_lossy(symdef_name),
    );

    // Every member arrives through `#1/`, and none of GNU's own members exists.
    let headers = bytes.windows(3).filter(|w| *w == b"#1/").count();
    assert_eq!(headers, members().len() + 1, "not every member carries a BSD extended name");
    assert!(
        !bytes.windows(3).any(|w| w == b"//\x20"),
        "the fixture carries GNU's long-name member",
    );

    // Names in the member data, not in the header — the whole difference.
    for (name, _) in members() {
        assert!(
            bytes.windows(name.len()).any(|w| w == name.as_bytes()),
            "{name} is not in the archive at all",
        );
    }
}

/// The negative control for the reader: with the BSD dialect unread, every
/// member is dropped and this link fails on the symbols the archive defines.
#[test]
fn a_bsd_archive_resolves_its_members() {
    let out = linked(bsd_archive(&members()), "bsd");
    assert!(!out.is_empty());
}

/// The oracle: the same objects in the two dialects are the same program. This
/// is what says the reader recovered the member *data* and not merely a name —
/// a member handed back with its `#1/` name still on the front, or with BSD's
/// 8-byte tail padding counted as content, parses as an object and produces a
/// different binary.
#[test]
fn the_two_dialects_link_identically() {
    assert_eq!(
        linked(archive(&members()), "gnu-vs-bsd-gnu"),
        linked(bsd_archive(&members()), "gnu-vs-bsd-bsd"),
        "the same members in the two ar dialects linked to different binaries",
    );
}
