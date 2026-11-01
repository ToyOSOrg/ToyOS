//! A directory's listing in [`toyos::fs::READDIR`]'s batches, resumed from a
//! cursor that names a place in the directory rather than a count of entries
//! already sent: removing or adding a name between two batches neither skips
//! nor repeats any other.
//!
//! **The cursor is a hash of a name**, 63 bits so it is never
//! [`READDIR_END`], and a batch is every entry from it in hash order. A batch
//! never ends between two names of one hash, so a collision costs a larger
//! batch and never an entry.

use toyos::fs::{encode_entry, entry_bytes, Entry, Error, READDIR_END};

use crate::volume::Listed;

/// Where `name` is in its directory's listing.
fn place(name: &str) -> u64 {
    // FNV-1a, 64 bits.
    let hash = name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    hash >> 1
}

/// The entries of `listed` from `cursor`, encoded in at most `room` bytes,
/// and the cursor after them: [`READDIR_END`] after the last. `LimitExceeded`
/// when the first group of one hash does not fit, and `NameTooLong` for a name
/// the encoding cannot carry.
pub fn batch(listed: &[Listed], cursor: u64, room: usize) -> Result<(Vec<u8>, u64), Error> {
    let mut placed: Vec<(u64, &Listed)> = listed.iter().map(|l| (place(&l.name), l)).filter(|(at, _)| *at >= cursor).collect();
    placed.sort_by(|(a, x), (b, y)| a.cmp(b).then_with(|| x.name.cmp(&y.name)));
    let mut out = Vec::new();
    let mut start = 0;
    while start < placed.len() {
        let at = placed[start].0;
        let group = placed[start..].iter().take_while(|(h, _)| *h == at).count();
        let bytes: usize = placed[start..start + group].iter().map(|(_, l)| entry_bytes(&l.name)).sum();
        if out.len() + bytes > room {
            return match out.is_empty() {
                true => Err(Error::LimitExceeded),
                false => Ok((out, at)),
            };
        }
        for (_, listed) in &placed[start..start + group] {
            let entry = Entry { kind: listed.meta.kind.wire(), size: listed.meta.size, node: listed.identity, name: &listed.name };
            encode_entry(&mut out, &entry).ok_or(Error::NameTooLong)?;
        }
        start += group;
    }
    Ok((out, READDIR_END))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::volume::{Kind, Meta};

    fn listed(names: &[&str]) -> Vec<Listed> {
        names
            .iter()
            .enumerate()
            .map(|(i, name)| Listed { name: name.to_string(), meta: Meta { kind: Kind::File, size: 0, mtime: 0 }, identity: i as u64 + 1 })
            .collect()
    }

    /// Every name of the batches, in order.
    fn names(batch: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        toyos::fs::for_each_entry(batch, |e| out.push(e.name.to_string())).unwrap();
        out
    }

    /// The whole listing, `room` bytes a batch, with `between` run on the
    /// directory before each batch after the first.
    fn walk(dir: &mut Vec<Listed>, room: usize, mut between: impl FnMut(&mut Vec<Listed>)) -> Vec<String> {
        let (mut seen, mut cursor, mut first) = (Vec::new(), 0, true);
        loop {
            if !first {
                between(dir);
            }
            first = false;
            let (bytes, next) = batch(dir, cursor, room).unwrap();
            seen.extend(names(&bytes));
            if next == READDIR_END {
                return seen;
            }
            cursor = next;
        }
    }

    #[test]
    fn small_batches_list_every_entry_once() {
        let all: Vec<String> = (0..200).map(|i| format!("file-{i}")).collect();
        let mut dir = listed(&all.iter().map(String::as_str).collect::<Vec<_>>());
        let mut seen = walk(&mut dir, 64, |_| {});
        seen.sort();
        let mut want = all.clone();
        want.sort();
        assert_eq!(seen, want);
    }

    /// The removal pattern of a tree delete: each batch's names are removed
    /// before the next is asked for. A cursor counting entries would skip as
    /// many as were removed.
    #[test]
    fn removing_what_was_listed_skips_nothing_after_it() {
        let all: Vec<String> = (0..100).map(|i| format!("n{i}")).collect();
        let mut dir = listed(&all.iter().map(String::as_str).collect::<Vec<_>>());
        let mut listed_so_far: Vec<String> = Vec::new();
        let (mut cursor, mut seen) = (0, Vec::new());
        loop {
            dir.retain(|l| !listed_so_far.contains(&l.name));
            let (bytes, next) = batch(&dir, cursor, 100).unwrap();
            let got = names(&bytes);
            listed_so_far.extend(got.iter().cloned());
            seen.extend(got);
            if next == READDIR_END {
                break;
            }
            cursor = next;
        }
        assert_eq!(seen.len(), 100, "{seen:?}");
    }

    #[test]
    fn a_name_added_mid_listing_costs_no_other_entry() {
        let all: Vec<String> = (0..50).map(|i| format!("x{i}")).collect();
        let mut dir = listed(&all.iter().map(String::as_str).collect::<Vec<_>>());
        let mut added = 0;
        let seen = walk(&mut dir, 80, |d| {
            added += 1;
            d.extend(listed(&[&format!("late{added}")]));
        });
        for name in &all {
            assert_eq!(seen.iter().filter(|s| *s == name).count(), 1, "{name}");
        }
    }

    #[test]
    fn a_first_entry_past_the_room_is_refused_and_an_empty_directory_ends_at_once() {
        let dir = listed(&["a-rather-long-name"]);
        assert_eq!(batch(&dir, 0, 10), Err(Error::LimitExceeded));
        assert_eq!(batch(&[], 0, 10), Ok((Vec::new(), READDIR_END)));
        assert_ne!(place("anything"), READDIR_END);
    }
}
