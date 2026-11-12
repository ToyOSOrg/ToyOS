//! One interrupt of a `-c` shell and one child it starts, in every order.
//!
//! The kernel's side as `toyos_proclife::quit` decides it: a quit ends a
//! process that never listened, and otherwise puts the reason in its notice
//! and asks the children there; a child that lands starts holding what its
//! parent's notice holds unread. The shell's side is [`Asked`] itself, its
//! notice read only through the methods that take `read`. The child never
//! listens, as `cat`, or listens at some step and reads each reason at once.

use toyos_abi::quit::Reason;

use crate::quit::Asked;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Step {
    /// The interrupt reaches the shell.
    Quit,
    /// The child the shell started lands.
    Land,
    /// The shell's main thread looks at the child ([`Asked::landed`]).
    Look,
    /// The shell's listener thread takes what the notice holds.
    Heard,
    /// The child listens, reading what its notice holds.
    Listens,
}

#[derive(Default)]
struct Child {
    landed: bool,
    listened: bool,
    /// A reason it inherited and has not read.
    holds: Option<Reason>,
    ended: bool,
    /// Reasons it read.
    read: u8,
}

struct World {
    shell: Asked,
    /// The shell's notice: the shell listened at its start.
    notice: Option<Reason>,
    child: Child,
}

impl World {
    fn new() -> Self {
        let mut shell = Asked::new();
        shell.one_command();
        Self { shell, notice: None, child: Child::default() }
    }

    /// A quit reaching the child, which ends one that has not listened.
    fn ask_child(&mut self) {
        let child = &mut self.child;
        match (child.landed && !child.ended, child.listened) {
            (false, _) => {}
            (true, false) => child.ended = true,
            (true, true) => child.read += 1,
        }
    }

    fn step(&mut self, step: Step) {
        let notice = &mut self.notice;
        match step {
            Step::Quit => {
                assert_eq!(notice.replace(Reason::Interrupt), None, "one interrupt");
                self.ask_child();
            }
            Step::Land => {
                self.child.landed = true;
                self.child.holds = *notice;
            }
            Step::Look => {
                if self.shell.landed(|| notice.take()) {
                    self.ask_child();
                }
            }
            Step::Heard => assert_eq!(self.shell.take(|| notice.take()), None, "a -c shell waits from its start"),
            Step::Listens => {
                self.child.listened = true;
                self.child.read += u8::from(self.child.holds.take().is_some());
            }
        }
    }
}

/// Every order of `steps` in which the child lands before the shell looks at
/// it and before it listens.
fn orders(steps: &[Step]) -> Vec<Vec<Step>> {
    fn permute(rest: &mut Vec<Step>, order: &mut Vec<Step>, out: &mut Vec<Vec<Step>>) {
        if rest.is_empty() {
            out.push(order.clone());
        }
        for i in 0..rest.len() {
            let step = rest.remove(i);
            order.push(step);
            permute(rest, order, out);
            order.pop();
            rest.insert(i, step);
        }
    }
    let mut out = Vec::new();
    permute(&mut steps.to_vec(), &mut Vec::new(), &mut out);
    out.retain(|order| {
        let land = at(order, Step::Land);
        [Step::Look, Step::Listens].iter().all(|step| !order.contains(step) || at(order, *step) > land)
    });
    out
}

fn at(order: &[Step], step: Step) -> usize {
    order.iter().position(|s| *s == step).expect("a step of the order")
}

/// The world after `order` and the listener's last take, which a reason in
/// the notice always wakes.
fn run(order: &[Step]) -> World {
    let mut world = World::new();
    for step in order {
        world.step(*step);
    }
    world.step(Step::Heard);
    world
}

/// The race this shell had: an interrupt that came while it started its
/// child, which the kernel's walk did not reach, left the child running.
#[test]
fn a_child_that_never_listens_ends_with_the_interrupt_in_every_order() {
    let orders = orders(&[Step::Quit, Step::Land, Step::Look, Step::Heard]);
    assert_eq!(orders.len(), 12);
    for order in orders {
        assert!(run(&order).child.ended, "{order:?}: the child outlived the interrupt");
    }
}

/// A child that listens reads the interrupt or ends with it, and reads it
/// twice only where it listened between its landing and the shell's look,
/// which nothing the shell does tells from a child the walk asked.
#[test]
fn a_child_that_listens_reads_the_interrupt_once_but_where_the_shell_cannot_tell() {
    let orders = orders(&[Step::Quit, Step::Land, Step::Look, Step::Heard, Step::Listens]);
    assert_eq!(orders.len(), 40);
    for order in orders {
        let child = run(&order).child;
        assert!(child.ended || child.read >= 1, "{order:?}: the child neither read the interrupt nor ended");
        let unseen = at(&order, Step::Listens) < at(&order, Step::Look);
        assert!(child.read <= 1 || (child.read == 2 && unseen), "{order:?}: the child read {} interrupts", child.read);
    }
}

/// At its prompt an interactive shell takes an interrupt as nothing and ends
/// at once on a hang-up; while it waits, a hang-up ends it once the line has,
/// and an interrupt ends a `-c` shell alone.
#[test]
fn a_reason_ends_the_shell_by_whether_it_waits_and_runs_one_command() {
    let reasons = |reasons: &[Reason]| {
        let mut left = reasons.to_vec();
        move || (!left.is_empty()).then(|| left.remove(0))
    };
    let mut interactive = Asked::new();
    assert_eq!(interactive.take(reasons(&[Reason::Interrupt])), None);
    assert_eq!(interactive.take(reasons(&[Reason::HangUp])), Some(129));
    interactive.waiting();
    assert_eq!(interactive.take(reasons(&[Reason::Interrupt, Reason::Terminate])), None);
    assert_eq!(interactive.waited(0, reasons(&[])), Some(143));
    let mut prompt = Asked::new();
    prompt.waiting();
    assert_eq!(prompt.waited(7, reasons(&[Reason::Interrupt])), None, "an interactive shell runs on past an interrupt");
    assert!(!prompt.landed(reasons(&[Reason::Interrupt])), "an interactive shell's line is quit child by child");

    let mut one = Asked::new();
    one.one_command();
    assert_eq!(one.waited(3, reasons(&[])), None);
    assert_eq!(one.waited(3, reasons(&[Reason::Interrupt])), Some(3), "a -c shell ends with its line's status");
}
