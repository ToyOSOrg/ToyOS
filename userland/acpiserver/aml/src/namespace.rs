//! The one namespace every definition block loads into (§5.3).
//!
//! Nodes live in an arena and are named by index and generation, so a
//! reference to an object a method created and its exit destroyed
//! (§5.5.2.3) resolves to nothing rather than to whatever reused its slot.
//! Nothing here recurses over the tree's depth, which a table chooses.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::name::{Path, Seg};
use crate::object::Object;
use crate::Error;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct NodeId {
    index: u32,
    generation: u32,
}

struct Node {
    seg: Seg,
    parent: Option<NodeId>,
    children: BTreeMap<Seg, NodeId>,
    /// An Alias (§19.6.4) acts exactly as its source, which already exists.
    alias: Option<NodeId>,
    object: Object,
    generation: u32,
    live: bool,
}

pub(crate) struct Namespace {
    nodes: Vec<Node>,
    free: Vec<u32>,
}

impl Namespace {
    pub(crate) fn new() -> Self {
        let root = Node {
            seg: Seg(*b"\\___"),
            parent: None,
            children: BTreeMap::new(),
            alias: None,
            object: Object::Scope,
            generation: 0,
            live: true,
        };
        Namespace { nodes: alloc::vec![root], free: Vec::new() }
    }

    pub(crate) fn root(&self) -> NodeId {
        NodeId { index: 0, generation: 0 }
    }

    fn node(&self, id: NodeId) -> Option<&Node> {
        self.nodes.get(id.index as usize).filter(|n| n.live && n.generation == id.generation)
    }

    pub(crate) fn object(&self, id: NodeId) -> Option<&Object> {
        self.node(id).map(|n| &n.object)
    }

    pub(crate) fn set(&mut self, id: NodeId, object: Object) -> Result<(), Error> {
        let n = self.nodes.get_mut(id.index as usize).filter(|n| n.live && n.generation == id.generation);
        n.map(|n| n.object = object).ok_or(Error::NotFound(String::from("an object its method's exit destroyed")))
    }

    pub(crate) fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).and_then(|n| n.parent)
    }

    /// The object named `seg` directly below `id`, an alias followed.
    pub(crate) fn child(&self, id: NodeId, seg: Seg) -> Option<NodeId> {
        let c = *self.node(id)?.children.get(&seg)?;
        let n = self.node(c)?;
        Some(n.alias.unwrap_or(c)).filter(|&t| self.node(t).is_some())
    }

    /// The scope a path's segments are walked from: the root, or `scope`
    /// raised by its parent prefixes. A prefix above the root finds nothing
    /// (§5.3).
    fn start(&self, scope: NodeId, path: &Path) -> Option<NodeId> {
        if path.root {
            return Some(self.root());
        }
        let mut at = scope;
        for _ in 0..path.up {
            at = self.parent(at)?;
        }
        Some(at)
    }

    /// The object a path names from `scope`, by §5.3's rules: a lone NameSeg
    /// is searched for in the scope and then each parent up to the root;
    /// anything else is looked up exactly.
    pub(crate) fn resolve(&self, scope: NodeId, path: &Path) -> Option<NodeId> {
        let mut at = self.start(scope, path)?;
        if path.searches() {
            loop {
                if let Some(found) = self.child(at, path.segs[0]) {
                    return Some(found);
                }
                at = self.parent(at)?;
            }
        }
        for &seg in &path.segs {
            at = self.child(at, seg)?;
        }
        Some(at)
    }

    /// Creates the object a path names: every segment but the last must
    /// exist, and the last must not (§5.3: "a name collision ... is
    /// considered fatal").
    pub(crate) fn create(&mut self, scope: NodeId, path: &Path, object: Object) -> Result<NodeId, Error> {
        let (last, parents) = path.segs.split_last().ok_or(Error::Rule("a definition names no object"))?;
        let missing = || Error::NotFound(crate::name::text(path));
        let mut at = self.start(scope, path).ok_or_else(missing)?;
        for &seg in parents {
            at = self.child(at, seg).ok_or_else(missing)?;
        }
        if self.node(at).is_some_and(|n| n.children.contains_key(last)) {
            return Err(Error::Exists(self.path_of(at, Some(*last))));
        }
        let node = Node {
            seg: *last,
            parent: Some(at),
            children: BTreeMap::new(),
            alias: None,
            object,
            generation: 0,
            live: true,
        };
        let id = match self.free.pop() {
            Some(index) => {
                let slot = &mut self.nodes[index as usize];
                let generation = slot.generation.wrapping_add(1);
                *slot = Node { generation, ..node };
                NodeId { index, generation }
            }
            None => {
                let index = u32::try_from(self.nodes.len()).map_err(|_| Error::Bound("the namespace's node count"))?;
                self.nodes.push(node);
                NodeId { index, generation: 0 }
            }
        };
        let parent = self.nodes.get_mut(at.index as usize).ok_or(Error::Rule("a parent vanished"))?;
        parent.children.insert(*last, id);
        Ok(id)
    }

    pub(crate) fn alias(&mut self, scope: NodeId, path: &Path, target: NodeId) -> Result<(), Error> {
        let id = self.create(scope, path, Object::Uninit)?;
        if let Some(n) = self.nodes.get_mut(id.index as usize) {
            n.alias = Some(target);
        }
        Ok(())
    }

    /// Destroys an object and everything below it (§5.5.2.3).
    pub(crate) fn remove(&mut self, id: NodeId) {
        let Some(n) = self.node(id) else { return };
        if let Some(p) = n.parent {
            let seg = n.seg;
            if let Some(parent) = self.nodes.get_mut(p.index as usize) {
                parent.children.remove(&seg);
            }
        }
        let mut doomed = alloc::vec![id];
        while let Some(d) = doomed.pop() {
            let Some(n) = self.nodes.get_mut(d.index as usize).filter(|n| n.live && n.generation == d.generation)
            else {
                continue;
            };
            n.live = false;
            n.object = Object::Uninit;
            doomed.extend(core::mem::take(&mut n.children).into_values());
            self.free.push(d.index);
        }
    }

    /// The absolute path of a node, and of `child` below it when given.
    pub(crate) fn path_of(&self, id: NodeId, child: Option<Seg>) -> String {
        let mut segs: Vec<Seg> = child.into_iter().collect();
        let mut at = id;
        while let Some(n) = self.node(at) {
            let Some(p) = n.parent else { break };
            segs.push(n.seg);
            at = p;
        }
        segs.reverse();
        crate::name::text(&Path { root: true, up: 0, segs })
    }
}
