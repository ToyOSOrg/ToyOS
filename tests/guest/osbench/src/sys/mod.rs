//! What std does not offer portably, as each OS offers it: the cheapest
//! syscall, a futex, a clock every process shares, anonymous mappings, IPC
//! between processes, a process's own accounting and the machine's
//! conditions. Both modules export the same names; Linux adds affinity,
//! which ToyOS has none of, and the readers of its cgroup and autogroup, and
//! ToyOS the diagnostics the tail group reads (`diag`).

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod imp;

#[cfg(target_os = "toyos")]
#[path = "toyos.rs"]
mod imp;

pub use imp::*;

/// What a process can read of its own accounting: Linux its context
/// switches and page faults; ToyOS nothing.
pub struct Counts {
    pub switches: Option<u64>,
    pub faults: Option<u64>,
}

impl Counts {
    /// The accounting from `before` to now, per one of `ops` operations,
    /// in hundredths, as result fields.
    pub fn per_op(before: &Counts, ops: u64) -> Vec<(&'static str, String)> {
        let now = counts();
        let ops = ops.max(1);
        let hundredths = |d: u64| format!("{}.{:02}", d / ops, d * 100 / ops % 100);
        let mut fields = Vec::new();
        if let (Some(a), Some(b)) = (before.switches, now.switches) {
            fields.push(("switches_per_op", hundredths(b - a)));
        }
        if let (Some(a), Some(b)) = (before.faults, now.faults) {
            fields.push(("faults_per_op", hundredths(b - a)));
        }
        fields
    }
}

/// Why a call did not do its work: a facility the system does not offer, by
/// the one answer that says so, or any other failure.
#[derive(Clone)]
pub enum Refused {
    Absent(String),
    Failed(String),
}

impl Refused {
    /// Report `bench` as unsupported for an absence, and refused for anything
    /// else.
    pub fn report(self, bench: &str) {
        match self {
            Refused::Absent(why) => crate::report::unsupported(bench, why),
            Refused::Failed(why) => crate::report::refuse(bench, why),
        }
    }

    /// What it says, whichever it is, for a call that already succeeded once
    /// and so is failing now.
    pub fn why(self) -> String {
        match self {
            Refused::Absent(why) | Refused::Failed(why) => why,
        }
    }
}

/// A whole anonymous read-write mapping, unmapped on drop.
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
}

impl Mapping {
    /// A mapping backed by huge pages wherever the system gives them.
    pub fn huge(len: usize) -> Mapping {
        Mapping { ptr: map_anon_huge(len), len }
    }

    pub fn ptr(&self) -> *mut u8 {
        self.ptr
    }

    /// The mapping as words. Anonymous memory reads as zero, so every word is
    /// initialised.
    pub fn words(&mut self) -> &mut [u64] {
        // SAFETY: `ptr` is page-aligned, `len` bytes long and owned by `self`.
        unsafe { std::slice::from_raw_parts_mut(self.ptr.cast::<u64>(), self.len / 8) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: nothing borrows the mapping past `self`.
        unsafe { unmap(self.ptr, self.len) }
    }
}
