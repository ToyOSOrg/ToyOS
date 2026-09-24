//! Who holds each span of one block device, and whose flush answers for the
//! writes its disk lost.
//!
//! **A block has one holder.** [`Holds::hold`] refuses a span a live hold
//! overlaps, and [`Holds::release`] gives it back.
//!
//! **A flush answers for its writer's own writes.** A device's flush is the
//! whole device's, and a disk that lost writes it had reported complete says
//! only that it did, as a count that never decreases (`toyos_xhci::flush`).
//! Each writer — a held span, or the one writer that holds none — keeps an
//! account of the writes it had reported since the last flush that succeeded,
//! against that count. A flush that succeeds settles every account, whoever
//! asked for it: writes reported under the count it ran under are durable, and
//! writes reported under an older count were lost. It fails for its own writer
//! if that writer lost writes — once, whoever flushed first.
//!
//! **A loss belongs to the blocks, not to the hold.** A span released owing a
//! report keeps its account, and the next hold of any block of it takes the
//! account over for the whole of that hold; what the new hold does not cover
//! keeps it too. So a loss may be told to more than one later holder of the
//! blocks it touched, and is never told to none: [`Holds::untold`] is what is
//! left for nobody.
//!
//! Pure: `alloc`, and nothing else.

#![cfg_attr(not(test), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

use alloc::vec::Vec;

/// Whose a write or a flush is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Writer {
    /// The hold whose span begins at this block.
    Span(u64),
    /// The writer that holds no span.
    Unspanned,
}

/// A flush that succeeded failed its writer: writes of `holder`'s — `None`
/// for the writer that holds no span — were reported before the disk lost
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Lost<H> {
    pub holder: Option<H>,
}

/// One writer's writes since the last flush that succeeded.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Hash)]
struct Account {
    /// The disk's loss count its writes since then were reported under, the
    /// oldest where two accounts merged; `None` for none.
    unflushed: Option<u64>,
    /// Writes of its were reported before the disk lost them, and no flush
    /// of its has said so yet.
    lost: bool,
}

impl Account {
    /// A write reported complete while the disk's count was `losses`.
    fn wrote(&mut self, losses: u64) {
        if self.unflushed.is_some_and(|at| at < losses) {
            self.lost = true;
        }
        self.unflushed = Some(losses);
    }

    /// A flush of the device succeeded while its count was `losses`: every
    /// write reported before it is durable, except one reported before a loss.
    fn settled(&mut self, losses: u64) {
        if self.unflushed.take().is_some_and(|at| at < losses) {
            self.lost = true;
        }
    }

    /// Whether its writer's flush fails for a loss: once.
    fn told(&mut self) -> bool {
        core::mem::take(&mut self.lost)
    }

    /// Writes of its are lost, or would be found lost by a flush now, and no
    /// flush has said so.
    fn owes(&self, losses: u64) -> bool {
        self.lost || self.unflushed.is_some_and(|at| at < losses)
    }

    fn clean(&self) -> bool {
        !self.lost && self.unflushed.is_none()
    }

    /// `other`'s writes are this account's too.
    fn absorb(&mut self, other: Self) {
        self.lost |= other.lost;
        self.unflushed = match (self.unflushed, other.unflushed) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
    }
}

/// `first..end` in device blocks, never empty: held by `holder`, or released
/// owing its account a report when `holder` is `None`.
#[derive(Clone, Debug)]
struct Span<H> {
    first: u64,
    end: u64,
    holder: Option<H>,
    account: Account,
}

impl<H> Span<H> {
    fn overlaps(&self, first: u64, end: u64) -> bool {
        self.first < end && first < self.end
    }
}

/// Every span of one device and every writer's account. No two spans overlap,
/// held or released.
#[derive(Clone, Debug)]
pub struct Holds<H> {
    spans: Vec<Span<H>>,
    unspanned: Account,
}

impl<H: Copy> Default for Holds<H> {
    fn default() -> Self {
        Self::new()
    }
}

impl<H: Copy> Holds<H> {
    pub const fn new() -> Self {
        Self { spans: Vec::new(), unspanned: Account { unflushed: None, lost: false } }
    }

    /// Holds `first..end` for `holder`, or refuses with the holder of a block
    /// of it. A released span it overlaps hands it its account.
    ///
    /// # Panics
    /// On an empty span: the caller refuses one before asking.
    pub fn hold(&mut self, first: u64, end: u64, holder: H) -> Result<(), H> {
        assert!(first < end, "an empty span is refused before it is held");
        if let Some(held) = self
            .spans
            .iter()
            .find_map(|span| span.holder.filter(|_| span.overlaps(first, end)))
        {
            return Err(held);
        }
        let mut account = Account::default();
        let mut kept = Vec::with_capacity(self.spans.len() + 2);
        for span in self.spans.drain(..) {
            if !span.overlaps(first, end) {
                kept.push(span);
                continue;
            }
            account.absorb(span.account);
            // What the new hold does not cover keeps the account as it was.
            if span.first < first {
                kept.push(Span { end: first, ..span.clone() });
            }
            if end < span.end {
                kept.push(Span { first: end, ..span });
            }
        }
        kept.push(Span { first, end, holder: Some(holder), account });
        self.spans = kept;
        Ok(())
    }

    /// The hold beginning at `first` ends; an account owing a report stays
    /// with its blocks.
    ///
    /// # Panics
    /// When no hold begins at `first`.
    pub fn release(&mut self, first: u64) {
        let at = self.held_at(first);
        if self.spans[at].account.clean() {
            self.spans.swap_remove(at);
        } else {
            self.spans[at].holder = None;
        }
    }

    /// A write of `writer`'s was reported complete while the disk's count was
    /// `losses`.
    ///
    /// # Panics
    /// When `writer` names no hold.
    pub fn wrote(&mut self, writer: Writer, losses: u64) {
        self.account(writer).wrote(losses);
    }

    /// A flush through `writer` succeeded while the disk's count was `losses`:
    /// every account is settled, and the flush fails if `writer`'s own lost
    /// writes no flush of its has reported.
    ///
    /// # Panics
    /// When `writer` names no hold.
    pub fn flushed(&mut self, writer: Writer, losses: u64) -> Result<(), Lost<H>> {
        self.unspanned.settled(losses);
        for span in &mut self.spans {
            span.account.settled(losses);
        }
        self.spans.retain(|span| span.holder.is_some() || !span.account.clean());
        let holder = match writer {
            Writer::Unspanned => None,
            Writer::Span(first) => self.spans[self.held_at(first)].holder,
        };
        match self.account(writer).told() {
            true => Err(Lost { holder }),
            false => Ok(()),
        }
    }

    /// Whether writes the disk lost by the count `losses` are reported to no
    /// writer yet.
    pub fn untold(&self, losses: u64) -> bool {
        self.unspanned.owes(losses) || self.spans.iter().any(|span| span.account.owes(losses))
    }

    fn held_at(&self, first: u64) -> usize {
        self.spans
            .iter()
            .position(|span| span.first == first && span.holder.is_some())
            .unwrap_or_else(|| panic!("no hold begins at block {first}"))
    }

    fn account(&mut self, writer: Writer) -> &mut Account {
        match writer {
            Writer::Unspanned => &mut self.unspanned,
            Writer::Span(first) => {
                let at = self.held_at(first);
                &mut self.spans[at].account
            }
        }
    }
}

#[cfg(test)]
mod tests;
