//! The one namespace every definition block loads into (§5.3).
//!
//! Nodes live in two arenas, each in the order its nodes were made and each
//! a stack. **The static arena holds what a definition block defines while
//! it loads** (§5.5.2: "Global objects are those NameSpace objects created at
//! table load time"), at boot or at run time by `Load`; a refused load takes
//! off everything it made. **The method arena holds what a method invocation
//! defines**, and a method's exit takes off everything made since it began
//! (§5.5.2.3), which is all its own, a callee's being gone before its caller
//! makes more. A block a method loads interleaves with the method's own
//! nodes in time, which is why one stack cannot hold both: the method's exit
//! would take the block with it.
//!
//! A node's parent and an alias's source are older than it or static, and
//! outlive it: a static node is refused below a method's, and so is a static
//! alias of one. A node is named by its arena, its index and a stamp no other
//! node ever has, so a reference to an object that is gone resolves to
//! nothing rather than to whatever was made at its index since. Nothing here
//! recurses over the tree's depth, which a table chooses, as it does a
//! path's length: every walk pays its caller's [`Toll`] for each scope it
//! climbs and each segment it looks up.
//!
//! Each arena is held against the interpreter's [`Meter`] at its capacity,
//! and each node at [`ENTRY`] beside it, from its creation to its removal: an
//! exit or a refusal leaves its arena the slots it had at the [`Mark`], and
//! a parent whose last child it took no map. An arena therefore moves as
//! often as a method or load that grows it runs, and a move pays the
//! [`Toll`] for the bytes it copies.
//!
//! The static arena's order is the order its nodes were declared in, and a
//! [`Walk`] reads sibling order from it alone: a parent's map keeps its
//! children by name. A walk is read between evaluations, when the method
//! arena is empty.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use crate::name::{Path, Seg};
use crate::object::{Meter, Object};
use crate::{Error, WORK_PER_STEP};

/// What a walk pays for a scope or a segment: a step of the evaluation it is
/// part of, which refuses the walk at its bound.
pub(crate) type Toll<'a> = &'a mut dyn FnMut() -> Result<(), Error>;

/// The bytes one slot of an arena is held at.
const SLOT: usize = core::mem::size_of::<Node>();

/// The bytes a node's entry among its parent's children is held at: a whole
/// leaf of the map, eleven keys and values beside its link to the tree,
/// which a parent of one child allocates for it; a map of more holds fewer
/// bytes an entry.
const ENTRY: usize = (core::mem::size_of::<usize>() + 4 + 11 * core::mem::size_of::<(Seg, Link)>()).next_multiple_of(8);

/// The slots an arena starts with.
const SLOTS: usize = 16;

/// The deepest a node lies: each level a slot and an entry its parent holds
/// it at, against the meter.
pub(crate) const DEEPEST: usize = crate::MAX_LIVE / (SLOT + ENTRY);

/// Which arena a node is in, and so what ends it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Arena {
    /// A definition block's: it lives as long as the namespace.
    Static = 0,
    /// A method invocation's: it ends with the invocation.
    Method = 1,
}

/// A node by its place, whatever it holds now: its index, its arena in the
/// top bit, in the four bytes an entry of a parent's map holds it in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Link(u32);

/// The bit of a [`Link`] that says the method arena.
const METHOD: u32 = 1 << 31;

impl Link {
    fn new(arena: Arena, index: u32) -> Link {
        Link(index | if arena == Arena::Method { METHOD } else { 0 })
    }

    fn arena(self) -> Arena {
        if self.0 & METHOD == 0 { Arena::Static } else { Arena::Method }
    }

    fn index(self) -> usize {
        (self.0 & !METHOD) as usize
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct NodeId {
    link: Link,
    stamp: u64,
}

/// How many nodes an arena has and how many slots hold them: what
/// [`Namespace::unwind`] goes back to.
pub(crate) struct Mark {
    arena: Arena,
    nodes: usize,
    slots: usize,
}

struct Node {
    seg: Seg,
    parent: Option<Link>,
    children: BTreeMap<Seg, Link>,
    /// An Alias (§19.6.4) acts exactly as its source, which already exists
    /// and outlives it.
    alias: Option<Link>,
    object: Object,
    stamp: u64,
}

pub(crate) struct Namespace {
    arenas: [Vec<Node>; 2],
    /// The stamp of the next node made.
    next: u64,
    meter: Rc<Meter>,
}

impl Namespace {
    pub(crate) fn new(meter: Rc<Meter>) -> Self {
        let mut ns = Namespace { arenas: [Vec::new(), Vec::new()], next: 0, meter };
        let root = Node { seg: Seg(*b"\\___"), parent: None, children: BTreeMap::new(), alias: None, object: Object::Scope, stamp: 0 };
        let held = "an empty meter holds the arena's first slots";
        ns.push(Arena::Static, root, &mut || Ok(())).expect(held);
        ns
    }

    pub(crate) fn root(&self) -> NodeId {
        NodeId { link: Link::new(Arena::Static, 0), stamp: 0 }
    }

    fn at(&self, link: Link) -> &Node {
        &self.arenas[link.arena() as usize][link.index()]
    }

    fn at_mut(&mut self, link: Link) -> &mut Node {
        &mut self.arenas[link.arena() as usize][link.index()]
    }

    fn id(&self, link: Link) -> NodeId {
        NodeId { link, stamp: self.at(link).stamp }
    }

    fn node(&self, id: NodeId) -> Option<&Node> {
        self.arenas[id.link.arena() as usize].get(id.link.index()).filter(|n| n.stamp == id.stamp)
    }

    pub(crate) fn object(&self, id: NodeId) -> Option<&Object> {
        self.node(id).map(|n| &n.object)
    }

    pub(crate) fn set(&mut self, id: NodeId, object: Object) -> Result<(), Error> {
        let n = self.arenas[id.link.arena() as usize].get_mut(id.link.index()).filter(|n| n.stamp == id.stamp);
        n.map(|n| n.object = object).ok_or(Error::NotFound(String::from("an object its method's exit destroyed")))
    }

    pub(crate) fn parent(&self, id: NodeId) -> Option<NodeId> {
        self.node(id).and_then(|n| n.parent).map(|p| self.id(p))
    }

    /// The object named `seg` directly below `id`, an alias followed.
    pub(crate) fn child(&self, id: NodeId, seg: Seg) -> Option<NodeId> {
        let c = *self.node(id)?.children.get(&seg)?;
        Some(self.id(self.at(c).alias.unwrap_or(c)))
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

    /// Creates the object a path names, in `arena`: every segment but the
    /// last must exist, and the last must not (§5.3: "a name collision ... is
    /// considered fatal"); a static object below a method's is refused.
    pub(crate) fn create(&mut self, arena: Arena, scope: NodeId, path: &Path, object: Object, toll: Toll<'_>) -> Result<NodeId, Error> {
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
        if arena == Arena::Static && at.link.arena() == Arena::Method {
            return Err(Error::Rule("a definition block defines an object below one a method made, which it would outlive"));
        }
        let node = Node { seg: *last, parent: Some(at.link), children: BTreeMap::new(), alias: None, object, stamp: 0 };
        self.push(arena, node, toll)
    }

    /// A node at its arena's end and among its parent's children, stamped,
    /// and held against the meter before either grows. An arena that is
    /// full doubles, which moves it: a step for every [`WORK_PER_STEP`]
    /// bytes moved, as for any copy.
    fn push(&mut self, arena: Arena, node: Node, toll: Toll<'_>) -> Result<NodeId, Error> {
        let nodes = &self.arenas[arena as usize];
        let index = u32::try_from(nodes.len()).ok().filter(|&i| i < METHOD).ok_or(Error::Bound("the namespace's node count"))?;
        if nodes.len() == nodes.capacity() {
            let held = nodes.capacity();
            for _ in 0..held * SLOT / WORK_PER_STEP {
                toll()?;
            }
            let more = held.max(SLOTS);
            self.meter.take(more * SLOT)?;
            self.arenas[arena as usize].reserve_exact(more);
        }
        let link = Link::new(arena, index);
        if let Some(p) = node.parent {
            self.meter.take(ENTRY)?;
            self.at_mut(p).children.insert(node.seg, link);
        }
        let stamp = self.next;
        self.next += 1;
        self.arenas[arena as usize].push(Node { stamp, ..node });
        Ok(NodeId { link, stamp })
    }

    pub(crate) fn alias(&mut self, arena: Arena, scope: NodeId, path: &Path, target: NodeId, toll: Toll<'_>) -> Result<NodeId, Error> {
        if arena == Arena::Static && target.link.arena() == Arena::Method {
            return Err(Error::Rule("a definition block makes an Alias of an object a method made, which it would outlive"));
        }
        let id = self.create(arena, scope, path, Object::Uninit, toll)?;
        self.at_mut(id.link).alias = Some(target.link);
        Ok(id)
    }

    /// How many nodes the static arena holds: every one a load made, and the
    /// predefined ones.
    pub(crate) fn defined(&self) -> u32 {
        self.arenas[Arena::Static as usize].len() as u32
    }

    /// The static arena's slots, which the meter holds it at.
    pub(crate) fn slots(&self) -> usize {
        self.arenas[Arena::Static as usize].capacity()
    }

    pub(crate) fn mark(&self, arena: Arena) -> Mark {
        let nodes = &self.arenas[arena as usize];
        Mark { arena, nodes: nodes.len(), slots: nodes.capacity() }
    }

    /// Destroys every object made in the mark's arena since it was read
    /// (§5.5.2.3), newest first, and gives back the slots that arena has
    /// grown by since.
    pub(crate) fn unwind(&mut self, mark: Mark) {
        let arena = mark.arena as usize;
        while self.arenas[arena].len() > mark.nodes
            && let Some(n) = self.arenas[arena].pop()
        {
            if let Some(p) = n.parent {
                let children = &mut self.at_mut(p).children;
                children.remove(&n.seg);
                // A map keeps a leaf for the last entry it loses, which
                // nothing is held for.
                if children.is_empty() {
                    *children = BTreeMap::new();
                }
                self.meter.give(ENTRY);
            }
        }
        let held = self.arenas[arena].capacity();
        self.arenas[arena].shrink_to(mark.slots);
        self.meter.give((held - self.arenas[arena].capacity()) * SLOT);
    }

    /// The absolute path of a node, and of `child` below it when given.
    pub(crate) fn path_of(&self, id: NodeId, child: Option<Seg>, toll: Toll<'_>) -> Result<String, Error> {
        let mut segs: Vec<Seg> = child.into_iter().collect();
        let mut at = self.node(id);
        while let Some(n) = at {
            let Some(p) = n.parent else { break };
            toll()?;
            segs.push(n.seg);
            at = Some(self.at(p));
        }
        segs.reverse();
        Ok(crate::name::text(&Path { root: true, up: 0, segs }))
    }
}

/// Every object of the static arena below the root, each after its parent
/// and siblings in the order they were declared, an Alias left out: its
/// source is met at its own place, and an Alias of an ancestor would
/// otherwise never end.
///
/// It is held against the meter while it lasts, at eight bytes a node and
/// five a level of the deepest: both are as many as the tables chose.
pub(crate) struct Walk<'a> {
    nodes: &'a [Node],
    meter: &'a Meter,
    /// A node's first child and its next sibling, by index; 0, the root's,
    /// is none.
    first: Vec<u32>,
    next: Vec<u32>,
    at: u32,
    depth: u32,
    path: String,
    held: usize,
}

/// The bytes of a path `depth` names long, `\AAAA.BBBB`.
const fn path_len(depth: usize) -> usize {
    5 * depth
}

impl Namespace {
    pub(crate) fn walk(&self) -> Result<Walk<'_>, Error> {
        let nodes = &self.arenas[Arena::Static as usize][..];
        let count = nodes.len();
        let links = 2 * count * core::mem::size_of::<u32>();
        self.meter.take(links)?;
        let mut walk = Walk { nodes, meter: &self.meter, first: Vec::new(), next: Vec::new(), at: 0, depth: 1, path: String::new(), held: links };
        for links in [&mut walk.first, &mut walk.next] {
            links.reserve_exact(count);
            links.resize(count, 0);
        }
        // A parent is older than its child and static as it is, so its depth
        // is known first: `next` holds each node's depth until the links are
        // made.
        let mut deepest = 0;
        for (i, n) in nodes.iter().enumerate() {
            if let Some(p) = n.parent {
                walk.next[i] = walk.next[p.index()] + 1;
                deepest = deepest.max(walk.next[i]);
            }
        }
        let path = path_len(deepest as usize);
        self.meter.take(path)?;
        walk.held += path;
        walk.path.reserve_exact(path);
        // Newest first, each node goes to the front of its parent's list,
        // which leaves the oldest there.
        for (i, n) in nodes.iter().enumerate().rev() {
            walk.next[i] = 0;
            if let (Some(p), None) = (n.parent, n.alias) {
                walk.next[i] = walk.first[p.index()];
                walk.first[p.index()] = i as u32;
            }
        }
        walk.at = walk.first[0];
        Ok(walk)
    }
}

impl Drop for Walk<'_> {
    fn drop(&mut self) {
        self.meter.give(self.held);
    }
}

impl Walk<'_> {
    /// The next object's depth, a child of the root's being 1, its place in
    /// the static arena, its name and the object.
    pub(crate) fn step(&mut self) -> Option<(u32, u32, Seg, &Object)> {
        let i = self.at as usize;
        if i == 0 {
            return None;
        }
        let (n, depth) = (&self.nodes[i], self.depth);
        self.path.truncate(path_len(depth as usize - 1));
        self.path.push(if depth == 1 { '\\' } else { '.' });
        self.path.extend(n.seg.0.iter().map(|&c| char::from(c)));
        if self.first[i] != 0 {
            self.at = self.first[i];
            self.depth += 1;
        } else {
            // The next sibling of this node or of the nearest ancestor that
            // has one: each node is climbed from once in a whole walk.
            let mut up = i;
            while up != 0 && self.next[up] == 0 {
                up = self.nodes[up].parent.map_or(0, |p| p.index());
                self.depth -= 1;
            }
            self.at = self.next[up];
        }
        Some((depth, i as u32, n.seg, &n.object))
    }

    /// The absolute path of the object last stepped to.
    pub(crate) fn path(&self) -> &str {
        &self.path
    }
}
