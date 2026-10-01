//! Where a process stands under the others, and what its end takes with it.
//!
//! **Every process has one parent and any number of children, and an end takes
//! the whole subtree.** A spawn's parent is its *place*: the spawner, or a
//! process whose `self` the spawner was handed. The kernel starts init with no
//! parent, init starts everything else, and nothing reparents.
//!
//! Three questions, each asked under `PROCESS_TABLE` and each one lock hold:
//!
//! - **May a child be placed here?** [`admit_child`] at the top of a spawn,
//!   before anything is built: a place being torn down takes nothing more,
//!   and a child deeper than [`MAX_DEPTH`] below init is refused. It is the
//!   last refusal: past it the spawn moves its caller's handles, so it lands.
//! - **Who does an end take?** [`claim`] one process at a time: its claim is
//!   what closes admission under it, and the same hold reads its children.
//!   A child admitted before the claim and landed after it is claimed in the
//!   hold that lands it ([`land_child`]), and its spawner ends it.
//! - **When is an end published?** Once its own teardown is done and every
//!   child's end is published: a count admission raises, and a child's
//!   publication, a refused spawn or the process's own teardown lowers
//!   ([`teardown_done`], [`published`], [`refuse_child`]). The thread that
//!   lowers it to zero publishes, then climbs to the parent: at most
//!   [`MAX_DEPTH`] + 1 publications, child before parent.

use alloc::vec::Vec;

use crate::table::{Lifecycle, Processes};
use crate::{teardown, Pid};

/// How far below init a process may be. Bounds the climb one teardown can
/// owe, which runs with preemption off.
pub const MAX_DEPTH: u32 = 64;

/// One process's place in the tree.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Node {
    parent: Option<Pid>,
    depth: u32,
    /// Placed under it and not yet published.
    children: Vec<Pid>,
    /// Its own teardown, and each child admitted under it and not yet
    /// published or refused.
    holds: u32,
}

impl Node {
    /// A process placed under none: init at depth 0, and a kernel thread's.
    pub fn root() -> Self {
        Self { parent: None, depth: 0, children: Vec::new(), holds: 1 }
    }

    pub fn parent(&self) -> Option<Pid> {
        self.parent
    }

    pub fn depth(&self) -> u32 {
        self.depth
    }
}

/// A spawn's first answer.
#[must_use = "an admitted spawn holds its place's publication until it lands or is refused"]
#[derive(PartialEq, Eq, Debug)]
pub enum Admit {
    Yes(Admitted),
    /// The place is being torn down or has gone.
    Gone,
    /// The child would be `depth` below init, past [`MAX_DEPTH`].
    TooDeep { depth: u32 },
}

/// A spawn admitted under its place, which it keeps unpublished until
/// [`land_child`] lands it or [`refuse_child`] lets the place go.
#[must_use = "an admitted spawn holds its place's publication until it lands or is refused"]
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Admitted {
    place: Pid,
    depth: u32,
}

impl Admitted {
    pub fn place(&self) -> Pid {
        self.place
    }
}

/// The next publication a lowered count owes: `pid`, then its `parent`'s turn.
#[must_use = "a process whose count reached zero is published by whoever lowered it"]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Publish {
    pub pid: Pid,
    pub parent: Option<Pid>,
}

/// Whom a landed child's end is owed to.
#[must_use = "a child claimed as it landed is its spawner's to end"]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Landed {
    /// Its place's, which is not claimed: that end's walk will take it.
    Placed,
    /// Its spawner's: the place was claimed since the admission and its walk
    /// has read its children, so the child was claimed in the hold that
    /// landed it.
    Claimed,
}

/// The question at the top of a spawn under `place`, before anything is built.
pub fn admit_child<T: Processes>(table: &mut T, place: Pid) -> Admit {
    let Some(proc) = table.get_mut(place) else { return Admit::Gone };
    if proc.tearing_down() {
        return Admit::Gone;
    }
    let depth = proc.node().depth + 1;
    if depth > MAX_DEPTH {
        return Admit::TooDeep { depth };
    }
    proc.node_mut().holds += 1;
    Admit::Yes(Admitted { place, depth })
}

/// Land `child` under its admitted place, in one hold: `insert` puts its
/// entry in the table with the node it is handed, and a child whose place was
/// claimed since the admission is claimed for `code` before the hold ends.
pub fn land_child<T: Processes, R>(
    table: &mut T,
    admitted: Admitted,
    child: Pid,
    code: i32,
    insert: impl FnOnce(&mut T, Node) -> R,
) -> (R, Landed) {
    let place = table
        .get_mut(admitted.place)
        .expect("land_child: an admitted spawn keeps its place unpublished, so in the table");
    // The mutation this feature stages: the child lands as if its place were
    // live, after the walk read its children.
    let claimed = place.tearing_down() && !cfg!(feature = "mutate-place-skips-the-insert-recheck");
    place.node_mut().children.push(child);
    let node = Node { parent: Some(admitted.place), depth: admitted.depth, children: Vec::new(), holds: 1 };
    let inserted = insert(table, node);
    if !claimed {
        return (inserted, Landed::Placed);
    }
    assert!(
        teardown::claim_teardown(table, child, code),
        "land_child: pid {child}, inserted in this hold, was claimed by another",
    );
    (inserted, Landed::Claimed)
}

/// A spawn whose build failed after its admission: its hold on the place goes.
pub fn refuse_child<T: Processes>(table: &mut T, admitted: Admitted) -> Option<Publish> {
    // The mutation this feature stages: the hold stays, and the place waits
    // for a child that will never be.
    if cfg!(feature = "mutate-refused-spawn-keeps-the-count") {
        return None;
    }
    lower(table, admitted.place)
}

/// Claim `pid` for `code` and owe its children to the walk, all in this one
/// hold. `false` for a process gone or claimed by another end, whose own walk
/// owes its children.
#[must_use = "a caller that did not win the claim must not retire anything"]
pub fn claim<T: Processes>(table: &mut T, pid: Pid, code: i32, owed: &mut Vec<Pid>) -> bool {
    if !teardown::claim_teardown(table, pid, code) {
        return false;
    }
    let proc = table.get(pid).expect("claim: the entry the claim just succeeded on");
    owed.extend_from_slice(&proc.node().children);
    true
}

/// The last one out of `pid` has torn it down: its own hold goes.
pub fn teardown_done<T: Processes>(table: &mut T, pid: Pid) -> Option<Publish> {
    // The mutation this feature stages: the end is published with its own
    // teardown, whatever is still unpublished below it, and no child's
    // publication climbs.
    if cfg!(feature = "mutate-publish-before-the-children") {
        let parent = table.get(pid).expect("teardown_done: a process torn down is in the table").node().parent;
        return Some(Publish { pid, parent });
    }
    lower(table, pid)
}

/// `child` is published: it leaves `parent`'s children, and its hold goes.
pub fn published<T: Processes>(table: &mut T, parent: Pid, child: Pid) -> Option<Publish> {
    if cfg!(feature = "mutate-publish-before-the-children") {
        if let Some(proc) = table.get_mut(parent) {
            proc.node_mut().children.retain(|&c| c != child);
        }
        return None;
    }
    let proc = table
        .get_mut(parent)
        .expect("published: a parent is unpublished, so in the table, while a child holds it");
    proc.node_mut().children.retain(|&c| c != child);
    lower(table, parent)
}

fn lower<T: Processes>(table: &mut T, pid: Pid) -> Option<Publish> {
    let node = table
        .get_mut(pid)
        .expect("lower: a process is in the table until it is published, and a hold keeps it unpublished")
        .node_mut();
    node.holds = node.holds.checked_sub(1).expect("lower: a process lowered past its last hold");
    (node.holds == 0).then_some(Publish { pid, parent: node.parent })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::World;

    /// A chain from init down to `MAX_DEPTH`: the last is admitted, and one
    /// more below it is refused naming the depth it would have had.
    #[test]
    fn a_chain_is_refused_at_max_depth_plus_one_below_init() {
        let mut world = World::new();
        let init = world.spawn_process();
        let mut at = init;
        for depth in 1..=MAX_DEPTH {
            at = world.spawn_child(at);
            assert_eq!(world.get(at).unwrap().node().depth(), depth);
        }
        assert_eq!(admit_child(&mut world, at), Admit::TooDeep { depth: MAX_DEPTH + 1 });
        let parent = world.get(at).unwrap().node().parent().unwrap();
        let Admit::Yes(admitted) = admit_child(&mut world, parent) else {
            panic!("a child at MAX_DEPTH itself was refused");
        };
        assert_eq!(refuse_child(&mut world, admitted), None);
    }

    #[test]
    fn a_place_being_torn_down_admits_nothing() {
        let mut world = World::new();
        let init = world.spawn_process();
        let place = world.spawn_child(init);
        assert!(claim(&mut world, place, 137, &mut Vec::new()));
        assert_eq!(admit_child(&mut world, place), Admit::Gone);
        assert_eq!(admit_child(&mut world, Pid(99)), Admit::Gone);
    }

    /// A child admitted before its place's claim and landed after it is
    /// claimed in the hold that lands it, and holds the place's publication
    /// until its own.
    #[test]
    fn a_child_landed_under_a_place_claimed_since_its_admission_is_claimed_with_it() {
        let mut world = World::new();
        let init = world.spawn_process();
        let place = world.spawn_child(init);
        let Admit::Yes(admitted) = admit_child(&mut world, place) else { panic!("admitted") };
        assert!(claim(&mut world, place, 137, &mut Vec::new()));
        assert_eq!(teardown_done(&mut world, place), None, "published with a child admitted under it");
        let child = world.reserve_pid();
        let ((), landed) = land_child(&mut world, admitted, child, 137, |world, node| world.insert(child, node));
        assert_eq!(landed, Landed::Claimed);
        assert_eq!(world.get(child).unwrap().teardown_code(), Some(137));
        assert_eq!(teardown_done(&mut world, child), Some(Publish { pid: child, parent: Some(place) }));
        assert_eq!(published(&mut world, place, child), Some(Publish { pid: place, parent: Some(init) }));
    }

    /// A published child leaves its parent's children, so a parent's walk
    /// reads only what is still unpublished below it.
    #[test]
    fn a_published_child_leaves_its_parents_children() {
        let mut world = World::new();
        let init = world.spawn_process();
        let child = world.spawn_child(init);
        assert!(claim(&mut world, child, 0, &mut Vec::new()));
        assert_eq!(teardown_done(&mut world, child), Some(Publish { pid: child, parent: Some(init) }));
        assert_eq!(published(&mut world, init, child), None);
        let mut owed = Vec::new();
        assert!(claim(&mut world, init, 137, &mut owed));
        assert_eq!(owed, [], "init's walk owes pid {child}, which was published");
    }
}
