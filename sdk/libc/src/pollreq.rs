//! What `poll` asks a ring to watch and which watch answers each entry. It
//! reads nothing but what it is handed, so the host tests it
//! (`toyos-libc-copies`).

use alloc::vec::Vec;

use toyos_abi::inbox::{READABLE, WRITABLE};

const POLLIN: i16 = 1;
const POLLOUT: i16 = 4;

/// The entry whose watch answers for `entries[entry]`, each a descriptor and
/// its `events`: the first that names its descriptor.
pub(crate) fn watch_of(entries: &[(i32, i16)], entry: usize) -> usize {
    let (fd, _) = entries[entry];
    entries[..entry].iter().position(|&(other, _)| other == fd).unwrap_or(entry)
}

/// The watches a `poll` of `entries` submits, each the entry it is answered
/// under, its descriptor and its interest. One per descriptor, for every
/// entry's interest: a watch replaces its handle's earlier one, so a second on
/// the descriptor would leave the first entry unanswered.
pub(crate) fn watches(entries: &[(i32, i16)]) -> Vec<(usize, i32, u32)> {
    let mut interest = alloc::vec![0u32; entries.len()];
    for (entry, &(_, events)) in entries.iter().enumerate() {
        let watch = watch_of(entries, entry);
        if events & POLLIN != 0 {
            interest[watch] |= READABLE;
        }
        if events & POLLOUT != 0 {
            interest[watch] |= WRITABLE;
        }
    }
    (0..entries.len())
        .filter(|&entry| watch_of(entries, entry) == entry)
        .map(|entry| (entry, entries[entry].0, interest[entry]))
        .collect()
}
