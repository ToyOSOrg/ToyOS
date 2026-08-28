//! Which PCID a new address space is handed, and when a returned tag is safe to
//! reissue.
//!
//! A PCID tags the TLB entries a space caches (SDM Vol. 3A §4.10.1), and the
//! kernel's every user CR3 load sets bit 63, which tells the processor not to
//! flush the incoming tag's entries (§4.10.4.1) — so a tag handed to a second
//! live space lets it read the first's translations with no fault. A tag is
//! owned here: returned only when its space drops, and never reissued while a
//! CPU may still hold a translation under it. A returned tag is *quarantined*
//! until a machine-wide flush moves it to *free*; a tag at or above `next_fresh`
//! was never issued, so no CPU has cached it. Pure — the kernel supplies the
//! lock and the shootdown, and [`oracle`] judges the decision against the SDM.

#![no_std]
#![forbid(unsafe_code)]

pub mod oracle;

pub const KERNEL_PCID: u16 = 0;
pub const MIN_USER_PCID: u16 = 1;
pub const MAX_USER_PCID: u16 = 4095;

const USER_TAGS: usize = (MAX_USER_PCID - MIN_USER_PCID + 1) as usize;

/// A user PCID in `MIN_USER_PCID..=MAX_USER_PCID`. `Copy`: a value, not the
/// ownership — the kernel's non-`Copy` guard owns the return.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Pcid(u16);

impl Pcid {
    pub fn get(self) -> u16 {
        self.0
    }
}

/// What [`PcidPool::alloc`] decided.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[must_use = "an allocation outcome dropped unhandled leaks or aliases a tag"]
pub enum Alloc {
    /// A tag no live space holds and no CPU caches — safe to load with NOFLUSH.
    Ready(Pcid),
    /// Only returned tags remain: flush every CPU, [`PcidPool::reclaim`], retry.
    NeedsFlush,
    /// All 4095 user tags are held by live address spaces.
    Exhausted,
}

/// The owned-tag allocator. See the module docs for the invariant it keeps.
pub struct PcidPool {
    #[cfg_attr(feature = "counting-allocator", allow(dead_code))]
    free: [u16; USER_TAGS],
    #[cfg_attr(feature = "counting-allocator", allow(dead_code))]
    free_len: usize,
    #[cfg_attr(feature = "counting-allocator", allow(dead_code))]
    quarantine: [u16; USER_TAGS],
    #[cfg_attr(feature = "counting-allocator", allow(dead_code))]
    q_len: usize,
    next_fresh: u16,
}

impl Default for PcidPool {
    fn default() -> Self {
        Self::new()
    }
}

impl PcidPool {
    /// Every tag pristine: nothing returned, `next_fresh` at the first tag.
    pub const fn new() -> Self {
        Self {
            free: [0; USER_TAGS],
            free_len: 0,
            quarantine: [0; USER_TAGS],
            q_len: 0,
            next_fresh: MIN_USER_PCID,
        }
    }

    /// The tag a new address space takes — never a live or quarantined one.
    #[cfg(not(feature = "counting-allocator"))]
    pub fn alloc(&mut self) -> Alloc {
        if self.free_len > 0 {
            self.free_len -= 1;
            return Alloc::Ready(Pcid(self.free[self.free_len]));
        }
        if self.next_fresh <= MAX_USER_PCID {
            let tag = self.next_fresh;
            self.next_fresh += 1;
            return Alloc::Ready(Pcid(tag));
        }
        if self.q_len > 0 {
            return Alloc::NeedsFlush;
        }
        Alloc::Exhausted
    }

    /// Return a dropped space's tag to quarantine: a CPU may still hold a
    /// translation under it until a flush.
    #[cfg(not(feature = "counting-allocator"))]
    pub fn free(&mut self, pcid: Pcid) {
        debug_assert!(
            (MIN_USER_PCID..=MAX_USER_PCID).contains(&pcid.0),
            "free: {} is not a user tag",
            pcid.0
        );
        debug_assert!(!self.holds(pcid.0), "free: {} returned twice", pcid.0);
        self.quarantine[self.q_len] = pcid.0;
        self.q_len += 1;
    }

    /// Move every quarantined tag to the free list. The caller has flushed every
    /// CPU since the last entered quarantine, so none can still be cached.
    #[cfg(not(feature = "counting-allocator"))]
    pub fn reclaim(&mut self) {
        while self.q_len > 0 {
            self.q_len -= 1;
            self.free[self.free_len] = self.quarantine[self.q_len];
            self.free_len += 1;
        }
    }

    #[cfg(not(feature = "counting-allocator"))]
    fn holds(&self, tag: u16) -> bool {
        self.free[..self.free_len].contains(&tag)
            || self.quarantine[..self.q_len].contains(&tag)
    }

    /// The negative control: the bare monotonic counter that wrapped at 4095 and
    /// reissued a live tag. The isolation tests red on it.
    #[cfg(feature = "counting-allocator")]
    pub fn alloc(&mut self) -> Alloc {
        let pcid = self.next_fresh;
        if pcid <= MAX_USER_PCID {
            self.next_fresh = pcid + 1;
            Alloc::Ready(Pcid(pcid))
        } else {
            self.next_fresh = MIN_USER_PCID + 1;
            Alloc::Ready(Pcid(MIN_USER_PCID))
        }
    }

    #[cfg(feature = "counting-allocator")]
    pub fn free(&mut self, _pcid: Pcid) {}

    #[cfg(feature = "counting-allocator")]
    pub fn reclaim(&mut self) {}
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use std::collections::HashSet;
    #[cfg(not(feature = "counting-allocator"))]
    use std::vec::Vec;

    /// The exact attack: hold every tag live and ask for one more. Green on the
    /// fix; red under `--features counting-allocator`, which reissues live tag 1.
    #[test]
    fn two_live_address_spaces_never_share_a_pcid() {
        let mut pool = PcidPool::new();
        let mut live = HashSet::new();
        let mut refused = false;
        for _ in 0..=(MAX_USER_PCID as usize) {
            match pool.alloc() {
                Alloc::Ready(p) => {
                    assert!(
                        (MIN_USER_PCID..=MAX_USER_PCID).contains(&p.get()),
                        "tag {} outside the user range",
                        p.get()
                    );
                    assert!(
                        live.insert(p.get()),
                        "the allocator reissued tag {}, still held by a live address space",
                        p.get()
                    );
                }
                Alloc::Exhausted => refused = true,
                Alloc::NeedsFlush => panic!("nothing was freed, so nothing awaits a flush"),
            }
        }
        assert_eq!(live.len(), USER_TAGS, "every user tag should be live");
        assert!(refused, "the tag past the last must be refused, never reissued");
    }

    #[test]
    #[cfg(not(feature = "counting-allocator"))]
    fn a_freed_tag_waits_for_a_flush_before_it_is_reissued() {
        let mut pool = PcidPool::new();
        let mut held = Vec::new();
        for _ in 0..USER_TAGS {
            match pool.alloc() {
                Alloc::Ready(p) => held.push(p),
                other => panic!("expected a fresh tag, got {other:?}"),
            }
        }
        let returned = held.pop().unwrap();
        pool.free(returned);

        assert_eq!(pool.alloc(), Alloc::NeedsFlush, "a quarantined tag is not ready");
        pool.reclaim();
        assert_eq!(pool.alloc(), Alloc::Ready(returned), "after a flush it is handed back");
    }

    #[test]
    #[cfg(not(feature = "counting-allocator"))]
    fn churn_never_aliases_two_live_tags() {
        let mut pool = PcidPool::new();
        let mut live: Vec<Pcid> = Vec::new();
        let mut seen = HashSet::new();

        for step in 0..50_000u32 {
            if step % 3 != 0 || live.is_empty() {
                match pool.alloc() {
                    Alloc::Ready(p) => {
                        assert!(seen.insert(p.get()), "tag {} is already live", p.get());
                        live.push(p);
                    }
                    Alloc::NeedsFlush => pool.reclaim(),
                    Alloc::Exhausted => {}
                }
            } else if let Some(p) = live.pop() {
                seen.remove(&p.get());
                pool.free(p);
            }
        }
    }
}
