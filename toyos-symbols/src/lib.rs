//! The pure half of a kernel backtrace frame: where an ELF file's symbol
//! tables live, and how much of the record's budget a demangled name gets.
//!
//! `kernel/src/symbols.rs` keeps the rest — a `SymbolTable` holding raw
//! pointers into either the kernel image or pages it owns, because the
//! resolve path is reached from the fault handler and the panic handler and
//! may not allocate, take a lock or do I/O. Nothing here needs any of that:
//! [`locate`] is a function of the bytes a caller already has, and the budget
//! constants are a function of two other crates' constants. Both halves are
//! tested here, on the host, against a real binary's own symbol table —
//! `tests/real.rs` names how it was produced and how to reproduce it.
//!
//! `no_std`, no allocation, no `unsafe`. [`locate`] borrows straight out of
//! its caller's bytes rather than copying them, which is what lets the
//! kernel's table point directly at the ELF instead of a copy of it.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

use toyos_abi::log::MAX_RECORD_MESSAGE;
use toyos_elf::section::{SectionTable, SHT_SYMTAB};
use toyos_elide::{widest, Elided, MARKER_MAX};

/// A backtrace frame's own text: `    ` + `{addr:#x}` + `  ` + `+` +
/// `{offset:#x}`, with both numbers at their widest — `0x` and sixteen hex
/// digits is every `u64` there is.
pub const FRAME_TEXT: usize = 4 + 18 + 2 + 1 + 18;

/// What is left of a record's message for the symbol, once the frame's own text
/// has taken its share. The slack is deliberate and it is in the safe
/// direction: a symbol a byte over this loses a byte from its middle, where a
/// *line* a byte over the record's bound loses its tail.
pub const FRAME_OVERHEAD: usize = 48;
const _: () = assert!(FRAME_OVERHEAD >= FRAME_TEXT);
pub const SYMBOL_BUDGET: usize = MAX_RECORD_MESSAGE - FRAME_OVERHEAD;

/// How much of the budget the head keeps; [`SYMBOL_TAIL`] is the rest.
///
/// **The marker comes out of the budget first**, because it is part of what
/// gets rendered: an earlier split spent the whole budget on head and tail and
/// then wrote `...[N bytes elided]...` between them, which put the line back
/// over the record's bound and cost it the tail this exists to keep.
///
/// An even split of what is left, because a backtrace with only one end of a
/// name in it names nothing either way: the head is the crate and the module
/// path, the tail is the function, and `screen_late_panic` asserts on the tail
/// for that reason.
const SYMBOL_KEPT: usize = SYMBOL_BUDGET - MARKER_MAX;
pub const SYMBOL_HEAD: usize = SYMBOL_KEPT / 2;
pub const SYMBOL_TAIL: usize = SYMBOL_KEPT - SYMBOL_HEAD;

/// The whole of the claim, in one place a compiler checks: a frame line fits a
/// record whatever the symbol was.
const _: () = assert!(widest(SYMBOL_HEAD, SYMBOL_TAIL) + FRAME_TEXT <= MAX_RECORD_MESSAGE);

/// And the three numbers the prose around here states, pinned so it cannot
/// drift from them again — which it did the first time `FRAME_OVERHEAD` moved.
const _: () = assert!(SYMBOL_BUDGET == 944 && SYMBOL_HEAD == 451 && SYMBOL_TAIL == 452);

/// A demangled symbol, rendered head-and-tail when it is wider than a record
/// can carry. `toyos-elide` is the mechanism and the argument.
///
/// **Nothing in the guest suite reaches this at the shipped bound, and saying
/// so is the point of this comment.** `screen_late_panic`'s
/// `late_panic::Nest` demangles to 288 bytes against a budget of 944, so
/// that gate proves the panel keeps a symbol's tail and proves nothing about
/// the elision — the tree's own widest symbol is under a third of what
/// triggers it.
/// `toyos-elide`'s own tests are where the seams are checked, on the host,
/// against characters that straddle both of them.
pub fn symbol_text<D>(name: D) -> Elided<D, SYMBOL_HEAD, SYMBOL_TAIL> {
    Elided(name)
}

/// `[offset, offset + len)` of `data`, or `None` when that is not wholly inside
/// it. Both numbers came out of the file, so the addition is checked.
pub fn file_range(data: &[u8], offset: u64, len: u64) -> Option<&[u8]> {
    let start = usize::try_from(offset).ok()?;
    let end = usize::try_from(offset.checked_add(len)?).ok()?;
    data.get(start..end)
}

/// Bytes the section header table occupies. Cannot overflow: both factors are
/// `u16`, and `e_shentsize` is honoured rather than assumed.
pub fn shdr_table_len(ehdr: &toyos_elf::FileHeader) -> u64 {
    ehdr.shnum as u64 * ehdr.shentsize as u64
}

/// `.symtab` and its `.strtab`, as byte slices straight out of `data` — no
/// copying, only bounds checks. `None` when the file has no symbol table, or
/// when a declared extent does not fit inside `data`; both are "no symbols",
/// which the kernel's caller turns into a table that names nothing rather
/// than a refused boot — a kernel that cannot find its own symbols still
/// boots and prints bare addresses.
///
/// Every extent here came out of the file, so every one of them is bounded
/// against it rather than trusted: a section running past EOF would otherwise
/// be read as whatever follows the image in memory.
pub fn locate(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let ehdr = toyos_elf::FileHeader::parse(data).ok()?;
    let shdrs = file_range(data, ehdr.shoff, shdr_table_len(&ehdr))?;
    let (syms, strs) = SectionTable::new(shdrs).symbols(SHT_SYMTAB)?;
    let symtab = file_range(data, syms.offset, syms.size)?;
    let strtab = file_range(data, strs.offset, strs.size)?;
    Some((symtab, strtab))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    #[test]
    fn a_short_name_is_untouched() {
        assert_eq!(symbol_text("core::panic::PanicInfo").to_string(), "core::panic::PanicInfo");
    }

    /// `SYMBOL_HEAD + SYMBOL_TAIL` is 903 bytes, so a name past that is elided
    /// — the wiring this crate owns and `toyos-elide` does not: which two
    /// numbers `Elided` gets.
    #[test]
    fn a_name_past_head_plus_tail_is_elided() {
        let long = "x".repeat(SYMBOL_HEAD + SYMBOL_TAIL + 1);
        let rendered = symbol_text(long.as_str()).to_string();
        assert!(rendered.contains("bytes elided"));
        assert!(rendered.len() <= SYMBOL_BUDGET);
    }

    #[test]
    fn file_range_refuses_an_extent_the_addition_overflows() {
        assert_eq!(file_range(b"abc", 1, u64::MAX), None);
    }

    #[test]
    fn file_range_refuses_an_extent_past_the_end() {
        assert_eq!(file_range(b"abcdef", 3, 10), None);
    }

    #[test]
    fn file_range_answers_an_extent_wholly_inside() {
        assert_eq!(file_range(b"abcdef", 2, 3), Some(&b"cde"[..]));
    }

    #[test]
    fn locate_refuses_bytes_too_short_for_a_file_header() {
        assert_eq!(locate(&[]), None);
        assert_eq!(locate(&[0u8; 10]), None);
    }
}
