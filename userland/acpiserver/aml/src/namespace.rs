//! The one namespace every definition block loads into (§5.3).
//!
//! Nodes live in an arena in the order they were made, and it is a stack:
//! a method's exit and a refused load take off everything made since they
//! began (§5.5.2.3), which is all theirs, a callee's being gone before its
//! caller makes more. A node's parent and an alias's source are therefore
//! older than it and outlive it. A node is named by its index and by a stamp
//! no other node ever has, so a reference to an object that is gone resolves
//! to nothing rather than to whatever was made at its index since.
//! Nothing here recurses over the tree's depth, which a table chooses, as it
//! does a path's length: every walk pays its caller's [`Toll`] for each scope
//! it climbs and each segment it looks up.
//!
//! The arena is held against the interpreter's [`Meter`] at its capacity,
//! and each node at [`ENTRY`] beside it, from its creation to its removal.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::name::{Path, Seg};
use crate::object::{Meter, Object};
use crate::Error;

/// What a walk pays for a scope or a segment: a step of the evaluation it is
/// part of, which refuses the walk at its bound.
pub(crate) type Toll<'a> = &'a mut dyn FnMut() -> Result<(), Error>;

/// The bytes one slot of the arena is held at.
const SLOT: usize = core::mem::size_of::<Node>();

/// The bytes a node's entry among its parent's children is held at: a whole
/// leaf of the map, eleven keys and values beside its link to the tree,
/// which a parent of one child allocates for it; a map of more holds fewer
/// bytes an entry.
const ENTRY: usize = (core::mem::size_of::<usize>() + 4 + 11 * core::mem::size_of::<(Seg, u32)>()).next_multiple_of(8);

/// The slots the arena starts with, and never shrinks below.
const SLOTS: usize = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct NodeId {
    index: u32,
    stamp: u64,
}

struct Node {
    seg: Seg,
    parent: Option<u32>,
    children: BTreeMap<Seg, u32>,
    /// An Alias (§19.6.4) acts exactly as its source, which already exists.
    alias: Option<u32>,
    object: Object,
    stamp: u64,
}

pub(crate) struct Namespace {
    nodes: Vec<Node>,
    /// The stamp of the next node made.
    next: u64,
    meter: Rc<Meter>,
}

impl Namespace {
    pub(crate) fn new(meter: Rc<Meter>) -> Self {
        let mut ns = Namespace { nodes: Vec::new(), next: 0, meter };
        ns.push(Node { seg: Seg(*b"\\___"), parent: None, children: BTreeMap::new(), alias: None, object: Object::Scope, stamp: 0 })
            .expect("an empty meter holds the arena's first slots");
        ns
    }

    pub(crate) fn root(&self) -> NodeId {
        NodeId { index: 0, stamp: 0 }
    }

    fn id(&self, index: u32) -> NodeId {
        NodeId { index, stamp: self.nodes[index as usize].stamp }
    }

    fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index as usize).filter(|n| n.stamp == id.stamp)
    }

    pub(crate) fn object(&self, id: NodeId) -> Option<&Object> {
        self.node(id).map(|n| &n.object)
    }

    pub(crate) fn set(&mut self, id: NodeId, object: Object) -> Result<(), Error> {
        let n = self.nodes.get_mut(id.index as usize).filter(|n| n.stamp == id.stamp);
        n.map(|n| n.object = object).ok_or(Error::NotFound(String::from("an object its method's exit destroyed")))
    }

    pub(crate) fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).and_then(|n| n.parent).map(|p| self.id(p))
    }

    /// The object named `seg` directly below `id`, an alias followed.
    pub(crate) fn child(&self, id: NodeId, seg: Seg) -> Option<NodeId> {
        let c = *self.node(id)?.children.get(&seg)?;
        Some(self.id(self.nodes[c as usize].alias.unwrap_or(c)))
    }

    /// The scope a path's segments are walked from: the root, or `scope`
    /// raised by its parent prefixes. A prefix above the root finds nothing
    /// (§5.3).
    fn start(&self, scope: NodeId, path: &Path, toll: Toll<'_>) -> Result<Option<NodeId>, Error> {
        if path.root {
            return Ok(Some(self.root()));
        }
        let mut at = scope;
        for _ in 0..path.up {
            toll()?;
            let Some(above) = self.parent(at) else { return Ok(None) };
            at = above;
        }
        Ok(Some(at))
    }

    /// The object a path names from `scope`, by §5.3's rules: a lone NameSeg
    /// is searched for in the scope and then each parent up to the root;
    /// anything else is looked up exactly.
    pub(crate) fn resolve(&self, scope: NodeId, path: &Path, toll: Toll<'_>) -> Result<Option<NodeId>, Error> {
        let Some(mut at) = self.start(scope, path, toll)? else { return Ok(None) };
        if path.searches() {
            loop {
                toll()?;
                if let Some(found) = self.child(at, path.segs[0]) {
                    return Ok(Some(found));
                }
                let Some(above) = self.parent(at) else { return Ok(None) };
                at = above;
            }
        }
        for &seg in &path.segs {
            toll()?;
            let Some(below) = self.child(at, seg) else { return Ok(None) };
            at = below;
        }
        Ok(Some(at))
    }

    /// Creates the object a path names: every segment but the last must
    /// exist, and the last must not (§5.3: "a name collision ... is
    /// considered fatal").
    pub(crate) fn create(&mut self, scope: NodeId, path: &Path, object: Object, toll: Toll<'_>) -> Result<NodeId, Error> {
        let (last, parents) = path.segs.split_last().ok_or(Error::Rule("a definition names no object"))?;
        let missing = || Error::NotFound(crate::name::text(path));
        let mut at = self.start(scope, path, toll)?.ok_or_else(missing)?;
        for &seg in parents {
            toll()?;
            at = self.child(at, seg).ok_or_else(missing)?;
        }
        if self.node(at).ok_or_else(missing)?.children.contains_key(last) {
            return Err(Error::Exists(self.path_of(at, Some(*last), toll)?));
        }
        self.push(Node { seg: *last, parent: Some(at.index), children: BTreeMap::new(), alias: None, object, stamp: 0 })
    }

    /// A node at the arena's end and among its parent's children, stamped,
    /// and held against the meter before either grows.
    fn push(&mut self, node: Node) -> Result<NodeId, Error> {
        let index = u32::try_from(self.nodes.len()).map_err(|_| Error::Bound("the namespace's node count"))?;
        if self.nodes.len() == self.nodes.capacity() {
            let more = self.nodes.capacity().max(SLOTS);
            self.meter.take(more * SLOT)?;
            self.nodes.reserve_exact(more);
        }
        if let Some(p) = node.parent {
            self.meter.take(ENTRY)?;
            self.nodes[p as usize].children.insert(node.seg, index);
        }
        let stamp = self.next;
        self.next += 1;
        self.nodes.push(Node { stamp, ..node });
        Ok(NodeId { index, stamp })
    }

    pub(crate) fn alias(&mut self, scope: NodeId, path: &Path, target: NodeId, toll: Toll<'_>) -> Result<NodeId, Error> {
        let id = self.create(scope, path, Object::Uninit, toll)?;
        self.nodes[id.index as usize].alias = Some(target.index);
        Ok(id)
    }

    /// How many nodes there are: what [`Namespace::unwind`] goes back to.
    pub(crate) fn mark(&self) -> usize {
        self.nodes.len()
    }

    /// Destroys every object made since `mark` was read (§5.5.2.3), newest
    /// first, and gives back the arena's slots past four for each node left.
    pub(crate) fn unwind(&mut self, mark: usize) {
        while self.nodes.len() > mark
            && let Some(n) = self.nodes.pop()
        {
            if let Some(p) = n.parent {
                self.nodes[p as usize].children.remove(&n.seg);
                self.meter.give(ENTRY);
            }
        }
        let (len, held) = (self.nodes.len(), self.nodes.capacity());
        if held > SLOTS.max(4 * len) {
            self.nodes.shrink_to(SLOTS.max(2 * len));
            self.meter.give((held - self.nodes.capacity()) * SLOT);
        }
    }

    /// The absolute path of a node, and of `child` below it when given.
    pub(crate) fn path_of(&self, id: NodeId, child: Option<Seg>, toll: Toll<'_>) -> Result<String, Error> {
        let mut segs: Vec<Seg> = child.into_iter().collect();
        let mut at = self.node(id);
        while let Some(n) = at {
            let Some(p) = n.parent else { break };
            toll()?;
            segs.push(n.seg);
            at = self.nodes.get(p as usize);
        }
        segs.reverse();
        Ok(crate::name::text(&Path { root: true, up: 0, segs }))
    }
}
