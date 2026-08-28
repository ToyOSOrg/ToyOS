//! An independent judge of the allocator, derived from the Intel SDM rather than
//! from the allocator's own code.
//!
//! [`Tlb`] models one processor's tagged TLB from three facts of SDM Vol. 3A
//! §4.10: a translation is tagged with the current PCID and looked up only under
//! it (§4.10.1); loading CR3 with bit 63 set does not flush the incoming tag, so
//! an earlier space's translation under it may be reused (§4.10.4.1); `INVPCID`
//! type 2 clears every tag, which a shootdown performs on each CPU (§4.10.4.1).
//! Each space maps an address to its own frame, so one space consuming another's
//! translation is the cross-space read the boundary forbids. [`drive`] runs the
//! reachable attack through both the real [`PcidPool`](crate::PcidPool) and this
//! model, sharing no line with it: the owned allocator admits no cross-space
//! read, the reverted counting allocator does.

use crate::{Alloc, Pcid, PcidPool};

const TLB_CAP: usize = 256;

/// A cross-address-space read: a CPU running `loaded` under `pcid` consumed a
/// translation `cached` left under the same tag.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct CrossSpaceRead {
    pub pcid: u16,
    pub va: u64,
    pub cached: u64,
    pub loaded: u64,
}

pub struct Tlb {
    entries: [(u16, u64, u64); TLB_CAP],
    len: usize,
}

impl Default for Tlb {
    fn default() -> Self {
        Self::new()
    }
}

impl Tlb {
    pub const fn new() -> Self {
        Self { entries: [(0, 0, 0); TLB_CAP], len: 0 }
    }

    /// Space `as_id`, loaded under `pcid` with CR3 bit 63 set, touches `va`. A
    /// hit on another space's translation is the cross-space read it would be; a
    /// miss caches this space's own.
    pub fn access(&mut self, as_id: u64, pcid: u16, va: u64) -> Option<CrossSpaceRead> {
        for &(p, v, cached) in &self.entries[..self.len] {
            if p == pcid && v == va {
                return (cached != as_id).then_some(CrossSpaceRead {
                    pcid,
                    va,
                    cached,
                    loaded: as_id,
                });
            }
        }
        assert!(self.len < TLB_CAP, "oracle TLB is undersized for this scenario");
        self.entries[self.len] = (pcid, va, as_id);
        self.len += 1;
        None
    }

    /// `INVPCID` all-context on this CPU — a shootdown's local half.
    pub fn flush_all(&mut self) {
        self.len = 0;
    }
}

const SHARED_VA: u64 = 0x1_0000;
const INIT_AS: u64 = 0;

/// Run the reachable attack through the real allocator and the SDM model, and
/// return the first cross-space read the model observes.
///
/// `churn` short-lived spaces are allocated and freed between init's tag and the
/// attacker's; past `MAX_USER_PCID` of them drive the base counter through its
/// wrap. The flush the owned allocator asks for is applied to the model too —
/// the shootdown the kernel performs, and why the owned allocator is safe.
pub fn drive(churn: u32) -> Option<CrossSpaceRead> {
    let mut pool = PcidPool::new();
    let mut tlb = Tlb::new();

    let init_pcid = alloc_now(&mut pool, &mut tlb)?;
    if let Some(v) = tlb.access(INIT_AS, init_pcid.get(), SHARED_VA) {
        return Some(v);
    }

    for _ in 0..churn {
        if let Some(short) = alloc_now(&mut pool, &mut tlb) {
            pool.free(short);
        }
    }

    // Exhaustion refuses the attacker's spawn rather than aliasing a live space.
    let attacker_pcid = alloc_now(&mut pool, &mut tlb)?;
    tlb.access(u64::from(churn) + 1, attacker_pcid.get(), SHARED_VA)
}

/// Ask the pool for a tag, flushing and reclaiming on `NeedsFlush`. `None` is
/// exhaustion.
fn alloc_now(pool: &mut PcidPool, tlb: &mut Tlb) -> Option<Pcid> {
    loop {
        match pool.alloc() {
            Alloc::Ready(p) => return Some(p),
            Alloc::NeedsFlush => {
                tlb.flush_all();
                pool.reclaim();
            }
            Alloc::Exhausted => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::MAX_USER_PCID;

    /// Teeth: a tag reissued to a second live space with no flush between is read
    /// across the boundary.
    #[test]
    fn the_model_flags_a_live_reissue() {
        let mut tlb = Tlb::new();
        assert_eq!(tlb.access(0, 1, SHARED_VA), None);
        let read = tlb.access(1, 1, SHARED_VA).expect("a live reissue is a cross-space read");
        assert_eq!(read.cached, 0);
        assert_eq!(read.loaded, 1);
    }

    /// And it credits a flush: the same reissue with a shootdown between is clean.
    #[test]
    fn a_flush_between_two_owners_clears_the_alias() {
        let mut tlb = Tlb::new();
        tlb.access(0, 1, SHARED_VA);
        tlb.flush_all();
        assert_eq!(tlb.access(1, 1, SHARED_VA), None);
    }

    /// The fix, judged by the spec: at the wrap and past it, no cross-space read.
    #[test]
    #[cfg(not(feature = "counting-allocator"))]
    fn the_owned_allocator_admits_no_cross_space_read() {
        let wrap = MAX_USER_PCID as u32;
        for churn in [wrap - 1, wrap, wrap + 5, 3 * wrap] {
            assert_eq!(drive(churn), None, "churn={churn}");
        }
    }

    /// The reverted counting allocator, judged the same way, does: the wrap hands
    /// the attacker init's still-live tag.
    #[test]
    #[cfg(feature = "counting-allocator")]
    fn the_counting_allocator_produces_a_cross_space_read() {
        let read = drive(MAX_USER_PCID as u32 - 1).expect("the wrap must alias init");
        assert_eq!(read.cached, INIT_AS, "the attacker read init's translation");
    }
}
