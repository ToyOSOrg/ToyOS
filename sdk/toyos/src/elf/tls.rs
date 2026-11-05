//! A thread-local datum's place inside its module's `PT_TLS`.

use crate::elf::layout::TlsSegment;

/// `S + A` inside one module's TLS segment, `0..=p_memsz`: made only by
/// [`TlsOffset::of`] against a [`TlsSegment`] a parse derived.
///
/// The end is inclusive for the reason [`crate::elf::Extent::offset`]'s is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TlsOffset(u64);

impl TlsOffset {
    /// `value + addend`, when it lies inside `segment`.
    pub(crate) const fn of(value: u64, addend: i64, segment: TlsSegment) -> Option<TlsOffset> {
        match value.checked_add_signed(addend) {
            Some(at) if at <= segment.memsz() => Some(TlsOffset(at)),
            _ => None,
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}
