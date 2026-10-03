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
//!   and a child deeper than [`MAX_DEPTH`] below init is refused. It takes
//!   the child's pid ([`crate::proclife::pids`]).
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

use crate::proclife::table::{Lifecycle, Processes};
use crate::proclife::{teardown, Pid, Tid};

/// How far below init a process may be. Bounds the climb one teardown can
/// owe.
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
#[must_use = "an admitted spawn holds its place's publication and its pid until it lands or is refused"]
#[derive(PartialEq, Eq, Debug)]
pub enum Admit {
    Yes(Admitted),
    /// The place is being torn down or has gone.
    Gone,
    /// The child would be `depth` below init, past [`MAX_DEPTH`].
    TooDeep { depth: u32 },
    /// Every pid below [`Pid::MAX`] is issued.
    NoPid,
}

/// A spawn admitted under its place, which it keeps unpublished, and the pid
/// it took, until [`land_child`] lands it or [`refuse_child`] gives both back.
#[must_use = "an admitted spawn holds its place's publication and its pid until it lands or is refused"]
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct Admitted {
    /// `None` for init, which the kernel starts under no process.
    place: Option<Pid>,
    depth: u32,
    pid: Pid,
}

impl Admitted {
    /// The pid the child will have.
    pub fn pid(&self) -> Pid {
        self.pid
    }
}

/// The next publication a lowered count owes: `pid`, then its `parent`'s turn.
#[must_use = "a process whose count reached zero is published by whoever lowered it"]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Publish {
    pub pid: Pid,
    pub parent: Option<Pid>,
}

/// The question at the top of a spawn under `place`, or of init's under
/// none, before anything is built.
pub fn admit_child<T: Processes>(table: &mut T, place: Option<Pid>) -> Admit {
    let depth = match place {
        None => 0,
        Some(place) => {
            let Some(proc) = table.get(place) else { return Admit::Gone };
            if proc.tearing_down() {
                return Admit::Gone;
            }
            let depth = proc.node().depth + 1;
            if depth > MAX_DEPTH {
                return Admit::TooDeep { depth };
            }
            depth
        }
    };
    let Some(pid) = table.pids().take() else { return Admit::NoPid };
    if let Some(place) = place {
        table.get_mut(place).expect("admit_child: the place read above").node_mut().holds += 1;
    }
    Admit::Yes(Admitted { place, depth, pid })
}

/// Land the admitted child, in one hold: `insert` puts its entry in the table
/// at its pid with the node it is handed. A child whose place was claimed since
/// the admission is claimed for `code` before the hold ends, and the threads
/// answered are its, for its spawner to retire: none for any other child.
#[must_use = "a child claimed as it landed is its spawner's to retire"]
pub fn land_child<T: Processes, R>(
    table: &mut T,
    admitted: Admitted,
    code: i32,
    insert: impl FnOnce(&mut T, Node) -> R,
) -> (R, Vec<Tid>) {
    let Admitted { place, depth, pid } = admitted;
    let claimed = match place {
        None => false,
        Some(place) => {
            let proc = table
                .get_mut(place)
                .expect("land_child: an admitted spawn keeps its place unpublished, so in the table");
            proc.node_mut().children.push(pid);
            // The mutation this feature stages: the child lands as if its
            // place were live, after the walk read its children.
            proc.tearing_down() && !cfg!(feature = "mutate-place-skips-the-insert-recheck")
        }
    };
    let inserted = insert(table, Node { parent: place, depth, children: Vec::new(), holds: 1 });
    if !claimed {
        return (inserted, Vec::new());
    }
    assert!(
        teardown::claim_teardown(table, pid, code),
        "land_child: pid {pid}, inserted in this hold, was claimed by another",
    );
    // The mutation this feature stages: the child is claimed and nobody
    // retires it.
    if cfg!(feature = "mutate-landed-child-retires-nothing") {
        return (inserted, Vec::new());
    }
    let child = table.get(pid).expect("land_child: inserted in this hold");
    (inserted, teardown::retire_set(child, None))
}

/// A spawn refused after its admission: its pid goes back, and its hold on
/// the place goes.
pub fn refuse_child<T: Processes>(table: &mut T, admitted: Admitted) -> Option<Publish> {
    table.pids().give_back(admitted.pid);
    // The mutation this feature stages: the hold stays, and the place waits
    // for a child that will never be.
    if cfg!(feature = "mutate-refused-spawn-keeps-the-count") {
        return None;
    }
    lower(table, admitted.place?)
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
    use crate::proclife::model::World;
    use crate::proclife::Pids;

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
        assert_eq!(admit_child(&mut world, Some(at)), Admit::TooDeep { depth: MAX_DEPTH + 1 });
        let parent = world.get(at).unwrap().node().parent().unwrap();
        let Admit::Yes(admitted) = admit_child(&mut world, Some(parent)) else {
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
        assert_eq!(admit_child(&mut world, Some(place)), Admit::Gone);
        assert_eq!(admit_child(&mut world, Some(Pid(99))), Admit::Gone);
    }

    /// A child admitted before its place's claim and landed after it is
    /// claimed in the hold that lands it, its spawner is answered its thread
    /// to retire, and it holds the place's publication until its own.
    #[test]
    fn a_child_landed_under_a_place_claimed_since_its_admission_is_claimed_with_it() {
        let mut world = World::new();
        let init = world.spawn_process();
        let place = world.spawn_child(init);
        let Admit::Yes(admitted) = admit_child(&mut world, Some(place)) else { panic!("admitted") };
        assert!(claim(&mut world, place, 137, &mut Vec::new()));
        assert_eq!(teardown_done(&mut world, place), None, "published with a child admitted under it");
        let child = admitted.pid();
        let ((), retire) = land_child(&mut world, admitted, 137, |world, node| world.insert(child, node));
        assert_eq!(retire, [world.main_tid(child)], "the child's spawner was not answered its thread to retire");
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

    /// Once every pid below `Pid::MAX` is issued a spawn is refused by name
    /// and holds its place by nothing, and a refused spawn's pid is the next
    /// one admitted: refused spawns spend none.
    #[test]
    fn a_spawn_past_the_last_pid_is_refused_and_a_refused_spawn_spends_none() {
        let mut world = World::with_pids(Pids::issued_below(Pid(u32::MAX - 2)));
        let init = world.spawn_process();
        let Admit::Yes(admitted) = admit_child(&mut world, Some(init)) else { panic!("the last pid was refused") };
        let last = admitted.pid();
        assert_eq!(last, Pid(u32::MAX - 1));
        assert_eq!(admit_child(&mut world, Some(init)), Admit::NoPid);
        assert_eq!(refuse_child(&mut world, admitted), None);
        for _ in 0..3 {
            let Admit::Yes(again) = admit_child(&mut world, Some(init)) else {
                panic!("a refused spawn spent pid {last}");
            };
            assert_eq!(again.pid(), last);
            assert_eq!(refuse_child(&mut world, again), None);
        }
        let Admit::Yes(lands) = admit_child(&mut world, Some(init)) else { panic!("pid {last} was spent") };
        let ((), retire) = land_child(&mut world, lands, 137, |world, node| world.insert(last, node));
        assert_eq!(retire, []);
        assert_eq!(admit_child(&mut world, Some(init)), Admit::NoPid);
        assert_eq!(admit_child(&mut world, None), Admit::NoPid);
        // init's own hold and its child's are all that hold it.
        assert!(claim(&mut world, last, 0, &mut Vec::new()));
        assert_eq!(teardown_done(&mut world, last), Some(Publish { pid: last, parent: Some(init) }));
        assert!(claim(&mut world, init, 0, &mut Vec::new()));
        assert_eq!(teardown_done(&mut world, init), None);
        assert_eq!(published(&mut world, init, last), Some(Publish { pid: init, parent: None }));
    }
}
