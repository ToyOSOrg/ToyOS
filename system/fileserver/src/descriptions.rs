//! Open files and directories as POSIX has them: a **description** is the
//! file or directory, the access an open fixed and the offset; a connection
//! names descriptions by **id**, and two ids, on one connection or on two, may
//! name one description and share its offset. Id 0 is `toyos::fs::ROOT`, the
//! grant's directory, and never a description's.
//!
//! A description lives while an id or a stream holds it, and a file's node is
//! closed when the last goes: every removal answers the file that is then
//! no longer held, for the server to close on the volume.
//!
//! **A directory is held by its volume path**, which the server moves with
//! every rename it makes and marks removed at its removal: the volume keys
//! nothing else that could follow it.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use toyos::fs::Error;

use crate::volume::Node;

/// Ids one connection holds at once.
pub const MAX_IDS: usize = 1024;

/// What a description holds open.
#[derive(Debug, PartialEq, Eq)]
pub enum Held {
    File(Node),
    Dir {
        /// Where it is on the volume now.
        path: String,
        /// Removed since it was opened: it lists nothing and names nothing.
        removed: bool,
    },
}

/// One open of a file or a directory, shared by every id and stream that
/// names it.
#[derive(Debug, PartialEq, Eq)]
pub struct Description {
    pub held: Held,
    /// Its `Volume::identity` when it was opened, which its renames keep.
    pub identity: u64,
    pub write: bool,
    pub append: bool,
    /// Where the next read or write that names no offset begins.
    pub offset: u64,
}

/// A description, shared.
pub type Shared = Rc<RefCell<Description>>;

/// A file no description holds open any more: its node and its identity.
pub type Closed = (Node, u64);

/// The file `shared` held, when it was its last holder.
pub fn release(shared: Shared) -> Option<Closed> {
    let description = Rc::try_unwrap(shared).ok()?.into_inner();
    match description.held {
        Held::File(node) => Some((node, description.identity)),
        Held::Dir { .. } => None,
    }
}

impl Description {
    /// The open file's node: `IsADirectory` for a directory.
    pub fn node(&self) -> Result<Node, Error> {
        match self.held {
            Held::File(node) => Ok(node),
            Held::Dir { .. } => Err(Error::IsADirectory),
        }
    }
}

/// One connection's ids.
#[derive(Default)]
pub struct Ids {
    ids: BTreeMap<u64, Shared>,
    next: u64,
}

impl Ids {
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// A new id for `shared`, or `TooManyOpen` past [`MAX_IDS`].
    pub fn insert(&mut self, shared: Shared) -> Result<u64, Error> {
        if self.ids.len() >= MAX_IDS {
            return Err(Error::TooManyOpen);
        }
        self.next += 1;
        self.ids.insert(self.next, shared);
        Ok(self.next)
    }

    pub fn get(&self, id: u64) -> Result<&Shared, Error> {
        self.ids.get(&id).ok_or(Error::UnknownFile)
    }

    /// A second id on `id`'s description.
    pub fn dup(&mut self, id: u64) -> Result<u64, Error> {
        let shared = Rc::clone(self.get(id)?);
        self.insert(shared)
    }

    /// Forget `id`: the file, when it was its description's last holder.
    pub fn close(&mut self, id: u64) -> Result<Option<Closed>, Error> {
        self.ids.remove(&id).map(release).ok_or(Error::UnknownFile)
    }

    /// The descriptions `ids` name, in order, for another connection: every
    /// one known, and at most [`MAX_IDS`] of them.
    pub fn export(&self, ids: &[u64]) -> Result<Ids, Error> {
        if ids.is_empty() {
            return Err(Error::InvalidRequest);
        }
        let mut out = Ids::default();
        for &id in ids {
            out.insert(Rc::clone(self.get(id)?))?;
        }
        Ok(out)
    }

    /// Every id gone with its connection: the files no longer held.
    pub fn drain(&mut self) -> Vec<Closed> {
        core::mem::take(&mut self.ids).into_values().filter_map(release).collect()
    }

    /// Every directory description an id names, once per id.
    pub fn dirs(&self) -> impl Iterator<Item = &Shared> {
        self.ids.values().filter(|d| matches!(d.borrow().held, Held::Dir { .. }))
    }
}

/// The directory at `from`, never the volume's root, is at `to` now with
/// everything beneath it: move `path` with it.
pub fn moved(path: &mut String, from: &str, to: &str) {
    let beneath = path.len() > from.len() && path.starts_with(from) && path.as_bytes()[from.len()] == b'/';
    if path == from || beneath {
        *path = crate::volume::join(to, &path[from.len()..].trim_start_matches('/').to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(node: Node) -> Shared {
        Rc::new(RefCell::new(Description { held: Held::File(node), identity: node, write: true, append: false, offset: 0 }))
    }

    #[test]
    fn ids_on_one_description_share_its_offset_and_its_node_closes_with_the_last() {
        let mut ids = Ids::default();
        let first = ids.insert(opened(7)).unwrap();
        let second = ids.dup(first).unwrap();
        ids.get(first).unwrap().borrow_mut().offset = 42;
        assert_eq!(ids.get(second).unwrap().borrow().offset, 42, "a dup has an offset of its own");
        assert_eq!(ids.close(first), Ok(None), "the node closed while a dup still held it");
        assert_eq!(ids.close(second), Ok(Some((7, 7))));
        assert_eq!(ids.close(second), Err(Error::UnknownFile));
    }

    /// An export installs the same descriptions on another connection, in the
    /// order named, so the receiver's ids are 1.. and the offset is shared.
    #[test]
    fn an_export_shares_descriptions_across_connections_in_the_order_named() {
        let mut parent = Ids::default();
        let a = parent.insert(opened(1)).unwrap();
        let b = parent.insert(opened(2)).unwrap();
        let mut child = parent.export(&[b, a]).unwrap();
        assert_eq!(child.get(1).unwrap().borrow().node(), Ok(2));
        assert_eq!(child.get(2).unwrap().borrow().node(), Ok(1));
        child.get(1).unwrap().borrow_mut().offset = 9;
        assert_eq!(parent.get(b).unwrap().borrow().offset, 9, "the child moved an offset of its own");
        assert_eq!(parent.drain(), Vec::<Closed>::new(), "the parent's end closed nodes the child holds");
        let mut closed = child.drain();
        closed.sort_unstable();
        assert_eq!(closed, [(1, 1), (2, 2)]);
    }

    #[test]
    fn an_export_naming_nothing_or_an_unknown_id_installs_nothing() {
        let mut parent = Ids::default();
        let a = parent.insert(opened(1)).unwrap();
        assert!(matches!(parent.export(&[]), Err(Error::InvalidRequest)));
        assert!(matches!(parent.export(&[a, a + 1]), Err(Error::UnknownFile)));
        assert_eq!(parent.close(a), Ok(Some((1, 1))), "a refused export kept a holder");
    }

    /// A directory's path follows a rename of it or of a directory above it,
    /// and nothing that only shares its prefix.
    #[test]
    fn a_held_directory_moves_with_every_rename_above_it() {
        let cases = [
            ("a/b", "a", "c", "c/b"),
            ("a", "a", "x/y", "x/y"),
            ("a/b/c", "a/b", "z", "z/c"),
            ("ab/c", "a", "z", "ab/c"),
            ("a", "a/b", "z", "a"),
        ];
        for (held, from, to, now) in cases {
            let mut path = held.to_string();
            moved(&mut path, from, to);
            assert_eq!(path, now, "{held} after {from} -> {to}");
        }
    }

    #[test]
    fn a_connection_holds_at_most_max_ids() {
        let mut ids = Ids::default();
        let first = ids.insert(opened(1)).unwrap();
        for _ in 1..MAX_IDS {
            ids.dup(first).unwrap();
        }
        assert_eq!(ids.dup(first), Err(Error::TooManyOpen));
        assert_eq!(ids.insert(opened(2)), Err(Error::TooManyOpen));
    }
}
