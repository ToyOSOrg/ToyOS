//! Open files as POSIX has them: a **description** is the file, the access an
//! open fixed and the offset; a connection names descriptions by **id**, and
//! two ids, on one connection or on two, may name one description and share
//! its offset.
//!
//! A description lives while an id or a stream holds it, and its node is
//! closed when the last goes: every removal answers the node that is then
//! no longer held, for the server to close on the volume.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use toyos::fs::Error;

use crate::volume::Node;

/// Ids one connection holds at once.
pub const MAX_IDS: usize = 1024;

/// One open of a file, shared by every id and stream that names it.
#[derive(Debug, PartialEq, Eq)]
pub struct Description {
    pub node: Node,
    /// The file's `Volume::identity` when it was opened, which its renames keep.
    pub identity: u64,
    pub write: bool,
    pub append: bool,
    /// Where the next read or write that names no offset begins.
    pub offset: u64,
}

/// A description, shared.
pub type Shared = Rc<RefCell<Description>>;

/// The node `shared` held, when it was its last holder.
pub fn release(shared: Shared) -> Option<Node> {
    Rc::try_unwrap(shared).ok().map(|d| d.into_inner().node)
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

    /// Forget `id`: the node, when it was its description's last holder.
    pub fn close(&mut self, id: u64) -> Result<Option<Node>, Error> {
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

    /// Every id gone with its connection: the nodes no longer held.
    pub fn drain(&mut self) -> Vec<Node> {
        core::mem::take(&mut self.ids).into_values().filter_map(release).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opened(node: Node) -> Shared {
        Rc::new(RefCell::new(Description { node, identity: node, write: true, append: false, offset: 0 }))
    }

    #[test]
    fn ids_on_one_description_share_its_offset_and_its_node_closes_with_the_last() {
        let mut ids = Ids::default();
        let first = ids.insert(opened(7)).unwrap();
        let second = ids.dup(first).unwrap();
        ids.get(first).unwrap().borrow_mut().offset = 42;
        assert_eq!(ids.get(second).unwrap().borrow().offset, 42, "a dup has an offset of its own");
        assert_eq!(ids.close(first), Ok(None), "the node closed while a dup still held it");
        assert_eq!(ids.close(second), Ok(Some(7)));
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
        assert_eq!(child.get(1).unwrap().borrow().node, 2);
        assert_eq!(child.get(2).unwrap().borrow().node, 1);
        child.get(1).unwrap().borrow_mut().offset = 9;
        assert_eq!(parent.get(b).unwrap().borrow().offset, 9, "the child moved an offset of its own");
        assert_eq!(parent.drain(), Vec::<Node>::new(), "the parent's end closed nodes the child holds");
        let mut closed = child.drain();
        closed.sort_unstable();
        assert_eq!(closed, [1, 2]);
    }

    #[test]
    fn an_export_naming_nothing_or_an_unknown_id_installs_nothing() {
        let mut parent = Ids::default();
        let a = parent.insert(opened(1)).unwrap();
        assert!(matches!(parent.export(&[]), Err(Error::InvalidRequest)));
        assert!(matches!(parent.export(&[a, a + 1]), Err(Error::UnknownFile)));
        assert_eq!(parent.close(a), Ok(Some(1)), "a refused export kept a holder");
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
