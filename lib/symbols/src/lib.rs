//! The pure half of a backtrace frame that reads no ELF, shared by the kernel
//! and by userland's namer: how much of a record's budget a demangled name
//! gets, and a killed program's frame record ([`frame`]). Reading an image's
//! symbols or build-id is `toyos::elf`'s, which the kernel does not link.
//!
//! `kernel/src/symbols.rs` keeps the rest of the kernel's own frames — a
//! `SymbolTable` holding raw pointers into the kernel image, because the
//! resolve path is reached from the fault handler and the panic handler and
//! may not allocate, take a lock or do I/O. Nothing here needs any of that:
//! every function is of the bytes a caller already has.
//!
//! `no_std`, no allocation, no `unsafe`, and no panic on any input: the kernel
//! runs it on its crash path.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

pub mod frame;

use toyos_abi::log::MAX_RECORD_MESSAGE;
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
/// path, the tail is the function.
const SYMBOL_KEPT: usize = SYMBOL_BUDGET - MARKER_MAX;
pub const SYMBOL_HEAD: usize = SYMBOL_KEPT / 2;
pub const SYMBOL_TAIL: usize = SYMBOL_KEPT - SYMBOL_HEAD;

/// The whole of the claim, in one place a compiler checks: a frame line fits a
/// record whatever the symbol was.
const _: () = assert!(widest(SYMBOL_HEAD, SYMBOL_TAIL) + FRAME_TEXT <= MAX_RECORD_MESSAGE);

/// And the three numbers the prose around here states, pinned so it cannot
/// drift from them again — which it did the first time `FRAME_OVERHEAD` moved.
const _: () = assert!(SYMBOL_BUDGET == 944 && SYMBOL_HEAD == 451 && SYMBOL_TAIL == 452);

/// A raw symbol as a frame prints it: demangled, and rendered head-and-tail
/// when it is wider than a record can carry. `toyos-elide` is the mechanism
/// and the argument; its own tests check the seams, on the host, against
/// characters that straddle both of them.
pub fn demangled(raw: &str) -> Elided<rustc_demangle::Demangle<'_>, SYMBOL_HEAD, SYMBOL_TAIL> {
    Elided(rustc_demangle::demangle(raw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::string::ToString;

    #[test]
    fn a_short_name_is_untouched() {
        assert_eq!(demangled("core::panic::PanicInfo").to_string(), "core::panic::PanicInfo");
    }

    /// `SYMBOL_HEAD + SYMBOL_TAIL` is 903 bytes, so a name past that is elided
    /// — the wiring this crate owns and `toyos-elide` does not: which two
    /// numbers `Elided` gets.
    #[test]
    fn a_name_past_head_plus_tail_is_elided() {
        let long = "x".repeat(SYMBOL_HEAD + SYMBOL_TAIL + 1);
        let rendered = demangled(&long).to_string();
        assert!(rendered.contains("bytes elided"));
        assert!(rendered.len() <= SYMBOL_BUDGET);
    }
}
