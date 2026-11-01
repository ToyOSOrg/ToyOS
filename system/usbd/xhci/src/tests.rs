//! The driver against the stub xHC, seeded where the order of events is the
//! question.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::rc::Rc;

use toyos_input::wire::Record;

use crate::exec::{Dead, Host, Nanos, Tasks};
use crate::hc::{self, Reset};
use crate::input::{Opened, Owner, Sent};
use crate::msc::{Event, Work};
use crate::stub::devices::{gpt_disk, Keyboard, Pipes, Stick};
use crate::stub::port::{FakePort, ResetBehaviour};
use crate::stub::{Answers, Dma, Function, Regs, Xhc, LENT_BASE};
use crate::usb::{Config, Usb};

const MS: Nanos = 1_000_000;
const QUICK: ResetBehaviour = ResetBehaviour::Completes { after: MS };
const GRANT: usize = 2 * 1024 * 1024;
/// The partition every test stick carries.
const UNIQUE: [u8; 16] = [0x33; 16];

#[derive(Clone)]
struct TestHost {
    now: Rc<Cell<Nanos>>,
    lines: Rc<RefCell<Vec<String>>>,
}

impl Host for TestHost {
    fn now(&self) -> Nanos {
        self.now.get()
    }
    fn say(&self, line: core::fmt::Arguments<'_>) {
        let line = line.to_string();
        if std::env::var_os("USBD_SAY").is_some() {
            eprintln!("[{:>9} us] {line}", self.now.get() / 1000);
        }
        self.lines.borrow_mut().push(line);
    }
}

/// The span of [`UNIQUE`] the table names, which is what a test's reader
/// reads of a disk.
type Table = Option<(u64, u64)>;

fn table(sectors: &mut dyn toyos_gpt::Sectors) -> Table {
    let located = toyos_gpt::locate(sectors, toyos_gpt::Guid(UNIQUE)).ok()?;
    let p = located.partition();
    Some((p.first_lba(), p.lba_count().get()))
}

/// The owner of input, as a test plays it.
#[derive(Default)]
struct Mock {
    connected: bool,
    /// Records it takes before it is full; `None` is no bound.
    room: Option<usize>,
    /// Whether it accepts a connection.
    refuses: bool,
    opens: u32,
    got: Vec<Record>,
    /// Records per connection.
    connections: Vec<Vec<Record>>,
}

impl Owner for Mock {
    fn open(&mut self) -> Opened {
        self.opens += 1;
        if self.refuses {
            return Opened::Refused;
        }
        self.connected = true;
        self.connections.push(Vec::new());
        Opened::Opened
    }
    fn send(&mut self, record: &Record) -> Sent {
        if !self.connected {
            return Sent::Ended;
        }
        if self.room == Some(0) {
            return Sent::Full;
        }
        if let Some(room) = &mut self.room {
            *room -= 1;
        }
        self.got.push(*record);
        self.connections.last_mut().unwrap().push(*record);
        Sent::Taken
    }
    fn close(&mut self) {
        self.connected = false;
    }
}

struct World {
    regs: Regs,
    dma: Dma,
    host: TestHost,
}

type Driver = Usb<Regs, Dma, TestHost, Table>;

/// Ends a test's driver with what it holds: declared after its tasks, it
/// drops first.
struct End<'a>(&'a Driver);

impl Drop for End<'_> {
    fn drop(&mut self) {
        self.0.hc.ending.set(true);
    }
}

impl World {
    fn new(ports: Vec<(FakePort, Option<Box<dyn Function>>)>, usb2: usize) -> Self {
        let (ports, devices) = ports.into_iter().unzip();
        let (regs, dma, _) = Xhc::build(ports, devices, usb2, GRANT);
        let host = TestHost { now: Rc::new(Cell::new(0)), lines: Rc::default() };
        Self { regs, dma, host }
    }

    fn xhc(&self) -> std::cell::RefMut<'_, Xhc> {
        self.regs.0.borrow_mut()
    }

    fn start(&self, config: Config) -> Driver {
        let reset = Reset::new(self.regs.clone(), &self.host).expect("the stub resets");
        assert!(reset.grant_bytes() <= GRANT, "{} bytes", reset.grant_bytes());
        let hc = reset.start(self.dma.clone(), &self.host).expect("the stub runs");
        Usb::new(hc, self.host.clone(), config, table)
    }

    fn said(&self, what: &str) -> bool {
        self.host.lines.borrow().iter().any(|l| l.contains(what))
    }

    /// Run the driver and the stub together until the clock reaches `until`.
    fn run(&self, usb: &Driver, tasks: &mut Tasks<'_>, owner: &mut Mock, until: Nanos) -> Result<(), Dead> {
        loop {
            let now = self.host.now.get();
            let mut quiet = 0;
            let mut at = None;
            let mut turns = 0;
            // The stub and the driver in turn at this instant, until neither
            // has anything to do.
            while quiet < 2 {
                turns += 1;
                assert!(turns < 100_000, "the driver and the stub never went quiet at {now} ns");
                let moved = self.xhc().run(now);
                // As usbd's binary does: the event ring is read only when an
                // interrupt record says to.
                let raised = self.xhc().take_interrupt();
                let mut took = false;
                if raised {
                    while usb.hc.take_events() && usb.hc.event_waiting() {}
                    took = true;
                }
                at = tasks.run(&usb.wake)?;
                usb.input.borrow_mut().flush(owner, now);
                quiet = if moved || took { 0 } else { quiet + 1 };
            }
            if now >= until {
                return Ok(());
            }
            let due = self.xhc().due().filter(|d| *d > now);
            let next = [at, due, Some(until)].into_iter().flatten().min().unwrap().max(now + 1);
            self.host.now.set(next.min(until));
        }
    }
}

/// What a test pushes a keyboard's reports into.
type Reports = Rc<RefCell<VecDeque<Vec<u8>>>>;
/// What a test reads a stick's sectors from, and sets its pipes by.
type Sectors = Rc<RefCell<Vec<u8>>>;
/// One piece of work's token and whether it was done.
type Answer = ((u64, u32), bool);

fn keyboard() -> (Box<dyn Function>, Reports) {
    let (kb, reports) = Keyboard::new();
    (Box::new(kb), reports)
}

fn stick(serial: &str) -> (Stick, Sectors, Rc<Cell<Pipes>>) {
    let stick = Stick::new(gpt_disk(16384, UNIQUE, 2048, 8192), serial);
    let (disk, pipes) = (stick.disk.clone(), stick.pipes.clone());
    (stick, disk, pipes)
}

const BOTH: Config = Config { input: true, disks: true };

fn keys(got: &[Record]) -> Vec<(u8, bool)> {
    got.iter()
        .filter_map(|r| match r {
            Record::Key { key, .. } => Some((key.keycode, key.pressed())),
            _ => None,
        })
        .collect()
}

/// A keyboard present at bring-up is reset, enumerated, bound by its report
/// descriptor in report protocol, and its reports reach the owner as key
/// transitions.
#[test]
fn a_keyboard_present_at_bring_up_delivers_its_reports() {
    let (kb, reports) = keyboard();
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(kb)), (FakePort::empty(QUICK), None)], 2);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 500 * MS).unwrap();
    assert!(usb.settled());
    assert!(w.said("collection 0: a Keyboard"), "{:#?}", w.host.lines.borrow());
    reports.borrow_mut().push_back(vec![0x02, 0, 0x04, 0, 0, 0, 0, 0]);
    w.run(&usb, &mut tasks, &mut owner, 510 * MS).unwrap();
    reports.borrow_mut().push_back(vec![0; 8]);
    w.run(&usb, &mut tasks, &mut owner, 520 * MS).unwrap();
    assert_eq!(keys(&owner.got), [(0xE1, true), (0x04, true), (0xE1, false), (0x04, false)]);
    assert_eq!(owner.opens, 1);
    assert_eq!(w.xhc().violations, []);
    assert_eq!(w.xhc().mem.borrow().faults, 0);
}

/// A device present at bring-up and one plugged in later run the same path:
/// the controller is asked the same commands in the same order.
#[test]
fn a_device_plugged_in_later_runs_the_same_path_as_one_present_at_bring_up() {
    let trace = |present: bool| {
        let (kb, _) = keyboard();
        let port = if present { FakePort::occupied(QUICK) } else { FakePort::empty(QUICK) };
        let w = World::new(vec![(port, Some(kb))], 1);
        let usb = w.start(BOTH);
        let mut tasks = usb.tasks();
        let _end = End(&usb);
        let mut owner = Mock::default();
        w.run(&usb, &mut tasks, &mut owner, 300 * MS).unwrap();
        if !present {
            w.xhc().ports[0].attach();
        }
        w.run(&usb, &mut tasks, &mut owner, 900 * MS).unwrap();
        assert!(usb.named.borrow().contains_key(&0), "present: {present}");
        let commands: Vec<u32> = w.xhc().commands.iter().map(|(_, c)| *c).collect();
        commands
    };
    assert_eq!(trace(true), trace(false));
}

/// A USB 2 port is debounced 100 ms (USB 2.0 §7.1.7.3) and given 10 ms of
/// reset recovery before its device is addressed; a USB 3 port trains its
/// own link and its device is addressed at once.
#[test]
fn a_usb2_port_waits_its_debounce_and_reset_recovery_and_a_usb3_port_waits_neither() {
    let addressed = |port: FakePort, usb2: usize| {
        let (kb, _) = keyboard();
        let w = World::new(vec![(port, Some(kb))], usb2);
        let usb = w.start(BOTH);
        let mut tasks = usb.tasks();
        let _end = End(&usb);
        let mut owner = Mock::default();
        w.run(&usb, &mut tasks, &mut owner, 50 * MS).unwrap();
        w.xhc().ports[0].attach();
        let attached = w.host.now.get();
        w.run(&usb, &mut tasks, &mut owner, 800 * MS).unwrap();
        let at = w.xhc().commands.iter().find(|(_, c)| *c == 11).map(|(at, _)| *at).expect("Address Device");
        at - attached
    };
    let usb2 = addressed(FakePort::empty(QUICK), 1);
    assert!((100 * MS + MS + 10 * MS..=100 * MS + MS + 12 * MS).contains(&usb2), "{} us", usb2 / 1000);
    let usb3 = addressed(FakePort::superspeed(QUICK), 0);
    assert!((100 * MS..100 * MS + MS).contains(&usb3), "{} us", usb3 / 1000);
}

/// The owed test: a stick whose bulk endpoint never answers leaves a
/// keyboard on the same controller delivering every report. Nothing here
/// reads the clock: every report is counted.
#[test]
fn a_stick_whose_bulk_endpoint_never_answers_leaves_a_keyboard_on_the_same_controller_delivering_every_report() {
    let (kb, reports) = keyboard();
    let (stick, _, pipes) = stick("SILENT1");
    pipes.set(Pipes::SilentIn);
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(Box::new(stick))), (FakePort::occupied(QUICK), Some(kb))], 2);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 400 * MS).unwrap();
    assert!(w.said("collection 0: a Keyboard"));
    // Across a minute: past the stick's every deadline, through each rung of
    // its ladder and its enumerations again.
    let mut want = Vec::new();
    for n in 0..40u8 {
        let key = 0x04 + (n % 20);
        reports.borrow_mut().push_back(vec![0, 0, key, 0, 0, 0, 0, 0]);
        reports.borrow_mut().push_back(vec![0; 8]);
        let now = w.host.now.get();
        w.run(&usb, &mut tasks, &mut owner, now + 1_500 * MS).unwrap();
        want.extend([(key, true), (key, false)]);
        assert_eq!(keys(&owner.got), want, "report {n}");
    }
    assert!(reports.borrow().is_empty());
    assert!(w.said("Reset Recovery") || w.said("the ladder climbs"), "{:#?}", w.host.lines.borrow());
    assert!(w.said("the ladder climbs to its port reset"), "{:#?}", w.host.lines.borrow());
}

/// A bulk endpoint that halted with its event lost, and whose device stalls
/// CLEAR_FEATURE(HALT), costs only its stick: the transfer the ring was moved
/// past is taken back rather than dropped, and the keyboard beside it goes on.
#[test]
fn a_halt_whose_clear_feature_stalls_costs_only_its_stick() {
    let (kb, reports) = keyboard();
    let (stick, _, pipes) = stick("HALTED1");
    pipes.set(Pipes::HaltUnannounced);
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(Box::new(stick))), (FakePort::occupied(QUICK), Some(kb))], 2);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 400 * MS).unwrap();
    assert!(w.said("collection 0: a Keyboard"));
    let mut want = Vec::new();
    for n in 0..20u8 {
        let key = 0x04 + n;
        reports.borrow_mut().push_back(vec![0, 0, key, 0, 0, 0, 0, 0]);
        reports.borrow_mut().push_back(vec![0; 8]);
        let now = w.host.now.get();
        w.run(&usb, &mut tasks, &mut owner, now + 1_500 * MS).unwrap();
        want.extend([(key, true), (key, false)]);
        assert_eq!(keys(&owner.got), want, "report {n}");
    }
    assert!(w.said("its device is reset (Offline)"), "{:#?}", w.host.lines.borrow());
}

/// The stop before a machine reset: every connected port reset while the
/// controller still runs, then the controller halted, reset and its ports
/// unpowered, with a stick's command still open, which is let go of and not
/// a panic.
#[test]
fn the_stop_resets_every_connected_port_then_halts_resets_and_unpowers_the_controller() {
    let (kb, _) = keyboard();
    let (stick, _, pipes) = stick("STOP1");
    let w = World::new(
        vec![
            (FakePort::occupied(QUICK), Some(Box::new(stick))),
            (FakePort::occupied(QUICK), Some(kb)),
            (FakePort::empty(QUICK), None),
        ],
        3,
    );
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 400 * MS).unwrap();
    assert!(w.said("collection 0: a Keyboard"), "{:#?}", w.host.lines.borrow());
    // The stick's next command goes unanswered, so the stop finds it open.
    pipes.set(Pipes::SilentIn);
    usb.disks.submit(0, Work::Io { write: false, block: 0, blocks: 1, at: LENT_BASE, token: (0, 1) });
    let now = w.host.now.get();
    w.run(&usb, &mut tasks, &mut owner, now + 50 * MS).unwrap();

    let stopped = Rc::new(Cell::new(None));
    let mut stop = Tasks::default();
    {
        let (usb, stopped) = (&usb, stopped.clone());
        stop.spawn(async move {
            stopped.set(Some(usb.hc.stop(&usb.wake).await));
            Ok(())
        });
    }
    // As usbd does: the driver ends, so what its tasks hold may be let go.
    usb.hc.ending.set(true);
    drop(tasks);
    let now = w.host.now.get();
    w.run(&usb, &mut stop, &mut owner, now + hc::STOP_NS).unwrap();
    assert_eq!(
        stopped.get(),
        Some(hc::Stopped { connected: 2, reset: 2, halted: true, cleared: true, ports: 3, unpowered: 3 })
    );
    for port in 0..3 {
        assert!(!usb.hc.portsc(port).in_reset(), "port {port} is still in reset");
    }
}

/// A stick whose connect is only seen after the driver started, as the T14's
/// is once its ports are powered again: the bus is not settled until the
/// stick is read and offered, and an empty port settles once its attach
/// window (USB 2's TSIGATT) has passed with nothing on it.
#[test]
fn a_stick_that_connects_after_the_start_is_offered_before_the_bus_settles() {
    let (stick, _, _) = stick("LATE1");
    let w = World::new(vec![(FakePort::empty(QUICK), Some(Box::new(stick))), (FakePort::empty(QUICK), None)], 2);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 20 * MS).unwrap();
    assert!(!usb.settled(), "the bus settled before its ports' attach window was over");
    w.xhc().ports[0].attach();
    let mut offered = false;
    for step in 1..=80 {
        w.run(&usb, &mut tasks, &mut owner, 20 * MS + step * 10 * MS).unwrap();
        offered |= usb.disks.take_events().iter().any(|e| matches!(e, Event::Arrived { .. }));
        if usb.settled() {
            assert!(offered, "the bus settled at {} ms with the stick not yet offered", 20 + step * 10);
            return;
        }
    }
    panic!("the bus never settled: {:#?}", w.host.lines.borrow());
}

/// A bus with nothing on it settles once the attach window has passed, and
/// not before.
#[test]
fn an_empty_bus_settles_when_its_attach_window_ends() {
    let w = World::new(vec![(FakePort::empty(QUICK), None)], 1);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 90 * MS).unwrap();
    assert!(!usb.settled());
    w.run(&usb, &mut tasks, &mut owner, 110 * MS).unwrap();
    assert!(usb.settled());
}

/// Firmware that never clears HC BIOS Owned: the controller is refused by
/// name after the handoff's bound and left as found, its SMIs and its run
/// state untouched (§4.22.1).
#[test]
fn a_controller_firmware_keeps_is_refused_and_left_as_found() {
    struct Ticking(Cell<Nanos>, RefCell<Vec<String>>);
    impl Host for &Ticking {
        fn now(&self) -> Nanos {
            self.0.set(self.0.get() + MS);
            self.0.get()
        }
        fn say(&self, line: core::fmt::Arguments<'_>) {
            self.1.borrow_mut().push(line.to_string());
        }
    }
    let w = World::new(vec![(FakePort::empty(QUICK), None)], 1);
    w.xhc().firmware_keeps = true;
    // Running, as firmware left it.
    w.xhc().usbcmd = 1;
    let host = Ticking(Cell::new(0), RefCell::new(Vec::new()));
    match Reset::new(w.regs.clone(), &&host) {
        Err(hc::Refusal::FirmwareKeepsIt(legsup)) => assert_ne!(legsup & 1 << 16, 0),
        Err(other) => panic!("refused for {other}"),
        Ok(_) => panic!("a controller firmware keeps was taken"),
    }
    assert_eq!(w.xhc().usbcmd, 1, "the controller was halted or reset under firmware");
    assert!(!host.1.borrow().iter().any(|l| l.contains("SMI generation off")), "{:#?}", host.1.borrow());
}

/// A controller that issues only 32-bit addresses (HCCPARAMS1.AC64 clear) is
/// driven, and states that width for every grant and lend (§5.3.6).
#[test]
fn a_controller_without_ac64_is_driven_at_32_bits() {
    let w = World::new(vec![(FakePort::empty(QUICK), None)], 1);
    w.xhc().ac64 = false;
    let reset = Reset::new(w.regs.clone(), &w.host).unwrap_or_else(|why| panic!("refused: {why}"));
    assert_eq!(reset.address_bits, 32);
    let w = World::new(vec![(FakePort::empty(QUICK), None)], 1);
    assert_eq!(Reset::new(w.regs.clone(), &w.host).expect("the stub resets").address_bits, 64);
}

/// The stick present at bring-up is read and offered: its table through the
/// reader the caller gives, and the bus is settled only once it has been.
#[test]
fn a_stick_is_offered_once_its_table_is_read_and_the_bus_settles_only_then() {
    let (stick, _, _) = stick("SERIAL1");
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(Box::new(stick)))], 1);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    let mut settled_before = false;
    let mut arrived = None;
    for step in 1..=60 {
        w.run(&usb, &mut tasks, &mut owner, step * 10 * MS).unwrap();
        for event in usb.disks.take_events() {
            if let Event::Arrived { disk, table, .. } = event {
                arrived = Some((disk, table));
            }
        }
        if arrived.is_none() && usb.settled() {
            settled_before = true;
        }
    }
    assert!(!settled_before, "the bus said it was settled before its disk was read");
    assert!(usb.settled());
    assert_eq!(arrived, Some((0, Some((2048, 8192)))));
}

/// A stick whose sector count is not whole 4 KiB blocks keeps its backup
/// table on its true last sector (UEFI 2.10 §5.3.1), past the last whole
/// block: with the primary header torn, the table is still read, from there.
#[test]
fn a_stick_s_backup_past_its_last_whole_block_is_read() {
    let sectors = 16384 + 4;
    let mut bytes = gpt_disk(sectors, UNIQUE, 2048, 8192);
    bytes[512 + 40] ^= 1;
    let stick = Stick::new(bytes, "SERIAL1");
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(Box::new(stick)))], 1);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    let mut arrived = None;
    for step in 1..=60 {
        w.run(&usb, &mut tasks, &mut owner, step * 10 * MS).unwrap();
        for event in usb.disks.take_events() {
            if let Event::Arrived { disk, table, .. } = event {
                arrived = Some((disk, table));
            }
        }
    }
    assert_eq!(arrived, Some((0, Some((2048, 8192)))), "the backup on sector {} was not read", sectors - 1);
}

/// A session's read and write move straight between the stick and the
/// client's lent region, and a flush is SYNCHRONIZE CACHE.
#[test]
fn a_disks_work_moves_between_the_stick_and_the_lent_region() {
    let (stick, disk, _) = stick("SERIAL1");
    let commands = stick.commands.clone();
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(Box::new(stick)))], 1);
    w.xhc().mem.borrow_mut().lend(LENT_BASE, 128 * 1024);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 600 * MS).unwrap();
    assert!(matches!(usb.disks.take_events()[..], [Event::Arrived { disk: 0, .. }]));
    let pattern: Vec<u8> = (0..8192u32).map(|i| (i * 7) as u8).collect();
    w.xhc().mem.borrow_mut().write(LENT_BASE + 0xF000, &pattern);
    // Two blocks from 60 KiB into the region: the data crosses 64 KiB.
    usb.disks.submit(0, Work::Io { write: true, block: 300, blocks: 2, at: LENT_BASE + 0xF000, token: (1, 1) });
    usb.disks.submit(0, Work::Flush { token: (1, 2) });
    usb.disks.submit(0, Work::Io { write: false, block: 300, blocks: 2, at: LENT_BASE + 0x1_8000, token: (1, 3) });
    w.run(&usb, &mut tasks, &mut owner, 700 * MS).unwrap();
    let answered: Vec<((u64, u32), bool)> = usb
        .disks
        .take_events()
        .into_iter()
        .filter_map(|e| match e {
            Event::Answered { token, ok, .. } => Some((token, ok)),
            _ => None,
        })
        .collect();
    assert_eq!(answered, [((1, 1), true), ((1, 2), true), ((1, 3), true)]);
    assert_eq!(&disk.borrow()[300 * 4096..302 * 4096], &pattern[..]);
    let mut back = vec![0; 8192];
    w.xhc().mem.borrow_mut().read(LENT_BASE + 0x1_8000, &mut back);
    assert_eq!(back, pattern);
    assert!(commands.borrow().contains(&0x35));
    assert_eq!(w.xhc().mem.borrow().faults, 0);
}

/// Writes that each cross 64 KiB, so their data phase is a TD of two TRBs,
/// interleaved with reads, whose bulk-out share is their CBW alone: the
/// bulk-out ring laps with TDs at every offset from its Link, and the stub
/// refuses any TD that crosses it.
#[test]
fn a_td_that_would_cross_the_rings_link_starts_behind_it() {
    let (stick, disk, _) = stick("SERIAL1");
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(Box::new(stick)))], 1);
    w.xhc().mem.borrow_mut().lend(LENT_BASE, 128 * 1024);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 600 * MS).unwrap();
    assert!(matches!(usb.disks.take_events()[..], [Event::Arrived { disk: 0, .. }]));
    let pattern: Vec<u8> = (0..8192u32).map(|i| (i * 13) as u8).collect();
    w.xhc().mem.borrow_mut().write(LENT_BASE + 0xF000, &pattern);
    let works = 400u32;
    let (mut queued, mut ok, mut until) = (0u32, 0usize, 600 * MS);
    while ok < works as usize {
        while queued < works && usb.disks.has_room(0) {
            let write = queued % 2 == 0;
            let (block, blocks, at) = if write { (300, 2, LENT_BASE + 0xF000) } else { (310, 1, LENT_BASE + 0x1_8000) };
            usb.disks.submit(0, Work::Io { write, block, blocks, at, token: (1, queued) });
            queued += 1;
        }
        until += 50 * MS;
        assert!(until < 60_000 * MS, "{ok} of {works} answered");
        w.run(&usb, &mut tasks, &mut owner, until).unwrap();
        ok += usb.disks.take_events().iter().filter(|e| matches!(e, Event::Answered { ok: true, .. })).count();
    }
    assert_eq!(&disk.borrow()[300 * 4096..302 * 4096], &pattern[..]);
    assert_eq!(w.xhc().mem.borrow().faults, 0);
}

/// TD Size (§4.11.2.4): the packets left after each TRB, 0 in the last.
#[test]
fn a_tds_size_counts_the_packets_left_after_each_trb() {
    let mps = |n| core::num::NonZeroU16::new(n).unwrap();
    let sizes = |pieces: &[(u64, u32)], max_packet| -> Vec<u32> {
        hc::normal_td(pieces, mps(max_packet)).iter().map(|t| t.status >> 17).collect()
    };
    // 8 KiB from 60 KiB at 512 bytes a packet: 16 packets, 8 after the first.
    assert_eq!(sizes(&[(0xF000, 0x1000), (0x1_0000, 0x1000)], 512), [8, 0]);
    // 64 KiB from 512 bytes in at 1024: 64 packets, and the first TRB's 63
    // whole ones leave 1 (its residue begins the next).
    assert_eq!(sizes(&[(0x200, 0xFE00), (0x1_0000, 0x200)], 1024), [1, 0]);
    // More than 31 left is 31.
    assert_eq!(sizes(&[(0, 0x1000), (0x1000, 0x1_0000), (0x1_1000, 0x1000)], 512), [31, 8, 0]);
    // Every TRB but the last chains; the last interrupts.
    let trbs = hc::normal_td(&[(0, 0x1000), (0x1000, 0x1000)], mps(512));
    assert_eq!(trbs.iter().map(|t| (t.control & hc::TRB_CHAIN != 0, t.control & hc::TRB_IOC != 0)).collect::<Vec<_>>(), [(true, false), (false, true)]);
}

/// A controller that answers no command once a device has left: the
/// teardown's Disable Slot is aborted on its deadline (§4.6.1.2), the port is
/// released and can enumerate again, and the slot's pages stay the
/// controller's.
#[test]
fn a_controller_that_never_answers_still_releases_the_port() {
    let (kb, _) = keyboard();
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(kb))], 1);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 400 * MS).unwrap();
    let free = usb.hc.pages_free();
    w.xhc().answers = Answers::Never;
    w.xhc().ports[0].detach();
    let left = w.host.now.get();
    w.run(&usb, &mut tasks, &mut owner, left + 100 * MS + hc::COMMAND_NS - 10 * MS).unwrap();
    assert!(usb.named.borrow().contains_key(&0), "released before the abort");
    w.run(&usb, &mut tasks, &mut owner, left + 100 * MS + hc::COMMAND_NS + 100 * MS).unwrap();
    assert!(!usb.named.borrow().contains_key(&0), "the port was never released");
    assert_eq!(usb.hc.counts.get().aborted, 1);
    assert_eq!(usb.hc.pages_free(), free, "a slot the controller never disabled gave its pages back");
    assert!(owner.got.iter().any(|r| matches!(r, Record::Gone { .. })), "the keyboard's gone was never sent");
}

/// An abort the controller does not answer either: the driver ends by name.
#[test]
fn an_unanswered_abort_ends_the_driver_by_name() {
    let (kb, _) = keyboard();
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(kb))], 1);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 400 * MS).unwrap();
    w.xhc().answers = Answers::NotEvenAnAbort;
    w.xhc().ports[0].detach();
    let ended = w.run(&usb, &mut tasks, &mut owner, 400 * MS + 100 * MS + 2 * hc::COMMAND_NS + 100 * MS);
    assert_eq!(ended, Err(Dead("a command and then its abort went unanswered")));
}

/// A keyboard whose endpoint stalls, on a controller that refuses the
/// recovery's command: the keyboard is let go of and its `gone` sent, and
/// the port keeps it, so it is not enumerated again every debounce.
#[test]
fn a_recovery_the_controller_refuses_lets_the_device_go() {
    struct Stalls(Keyboard);
    impl Function for Stalls {
        fn control(&mut self, setup: [u8; 8], out: &[u8]) -> crate::stub::Reply {
            self.0.control(setup, out)
        }
        fn transfer(&mut self, _: u8, _: Option<&[u8]>, _: usize) -> crate::stub::Reply {
            crate::stub::Reply::Stall
        }
    }
    let (kb, _) = Keyboard::new();
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(Box::new(Stalls(kb))))], 1);
    w.xhc().answers = Answers::Refuses(19);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 2_000 * MS).unwrap();
    assert!(w.said("would not run again; it is let go of"), "{:#?}", w.host.lines.borrow());
    assert_eq!(w.xhc().commands.iter().filter(|(_, c)| *c == 11).count(), 1, "enumerated again");
    assert!(usb.named.borrow().contains_key(&0), "the port let its device go");
    assert!(!owner.connected, "the connection outlived its last device");
}

/// What a marginal cable does: attach, detach and replug at instants no test
/// author would pick, with every port step and every PORTSC write checked
/// against the invariants, and at the end the driver's belief agreeing with
/// the port.
#[test]
fn no_sequence_breaks_an_invariant() {
    for seed in std::env::var("SEED").ok().map_or(0..200u64, |s| { let s: u64 = s.parse().unwrap(); s..s + 1 }) {
        let mut state = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        let behaviour = if next() % 8 == 0 { ResetBehaviour::Never } else { ResetBehaviour::Completes { after: 1 + next() % (4 * MS) } };
        let (kb, _) = keyboard();
        let w = World::new(vec![(FakePort::empty(behaviour), Some(kb))], 1);
        let usb = w.start(BOTH);
        let mut tasks = usb.tasks();
        let _end = End(&usb);
        let mut owner = Mock::default();
        for _ in 0..16 {
            match next() % 4 {
                0 => w.xhc().ports[0].attach(),
                1 => w.xhc().ports[0].detach(),
                2 => w.xhc().ports[0].replug(),
                _ => {}
            }
            let now = w.host.now.get();
            w.run(&usb, &mut tasks, &mut owner, now + 1 + next() % (400 * MS)).unwrap();
        }
        let now = w.host.now.get();
        w.run(&usb, &mut tasks, &mut owner, now + 2_000 * MS + crate::port::RESET_DEADLINE_NS).unwrap();
        assert_eq!(w.xhc().violations, [], "seed {seed}");
        assert_eq!(w.xhc().mem.borrow().faults, 0, "seed {seed}");
        let present = w.xhc().ports[0].present();
        if behaviour != ResetBehaviour::Never {
            assert_eq!(usb.named.borrow().contains_key(&0), present, "seed {seed}: {:#?}", w.host.lines.borrow());
        }
    }
}

/// Every TRB the driver hands the controller is published body first: no
/// control word, which carries the cycle bit, is stored before a `publish`
/// that follows the TRB's body (§4.9.2). The ARM track's ordering check.
#[test]
fn the_cycle_bit_is_published_after_the_trbs_body() {
    let (kb, reports) = keyboard();
    let (stick, _, _) = stick("SERIAL1");
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(kb)), (FakePort::occupied(QUICK), Some(Box::new(stick)))], 2);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 500 * MS).unwrap();
    reports.borrow_mut().push_back(vec![0, 0, 4, 0, 0, 0, 0, 0]);
    w.run(&usb, &mut tasks, &mut owner, 600 * MS).unwrap();
    // A ring is a page whose last TRB is its Link back (§4.11.5.1).
    let ring = |at: usize| {
        let page = at & !0xFFF;
        let mut control = [0u8; 4];
        w.xhc().mem.borrow_mut().read(crate::stub::GRANT_BASE + (page + 0xFFC) as u64, &mut control);
        (u32::from_le_bytes(control) >> 10) & 0x3F == 6
    };
    let log = w.dma.log.borrow();
    let mut body_since_publish = std::collections::BTreeSet::<usize>::new();
    let mut checked = 0;
    for entry in log.iter() {
        match entry {
            None => body_since_publish.clear(),
            Some(at) if at % 16 == 12 && ring(*at) => {
                let trb = at - 12;
                assert!(!body_since_publish.contains(&trb) && !body_since_publish.contains(&(trb + 8)), "the TRB at {trb:#x}'s control word was stored before its body was published");
                checked += 1;
            }
            Some(at) => {
                body_since_publish.insert(*at & !7);
            }
        }
    }
    assert!(checked > 20, "{checked} control words");
}

/// A write whose CBW goes unanswered leaves the stick owed its Data-Out, so
/// the ladder enters at its port reset (BOT 1.0 §6.7). Answers the driver,
/// the stick's pipes and when the write was queued.
fn a_write_breaks_into_a_port_reset() -> (World, Rc<Cell<Pipes>>) {
    let (stick, _, pipes) = stick("SERIAL1");
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(Box::new(stick)))], 1);
    w.xhc().mem.borrow_mut().lend(LENT_BASE, 64 * 1024);
    (w, pipes)
}

fn answered(usb: &Driver) -> (Vec<Answer>, Vec<Event<Table>>) {
    let (mut answers, mut rest) = (Vec::new(), Vec::new());
    for event in usb.disks.take_events() {
        match event {
            Event::Answered { token, ok, .. } => answers.push((token, ok)),
            other => rest.push(other),
        }
    }
    (answers, rest)
}

/// The same stick back from the port reset its ladder asked for keeps its
/// disk: no `Gone`, and the write it was given is issued again and answered.
#[test]
fn a_stick_back_from_its_port_reset_keeps_its_disk_and_its_work() {
    let (w, pipes) = a_write_breaks_into_a_port_reset();
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 600 * MS).unwrap();
    assert!(matches!(answered(&usb).1[..], [Event::Arrived { disk: 0, .. }]));
    pipes.set(Pipes::SilentOut);
    let queued = w.host.now.get();
    usb.disks.submit(0, Work::Io { write: true, block: 10, blocks: 1, at: LENT_BASE, token: (1, 1) });
    w.run(&usb, &mut tasks, &mut owner, queued + crate::msc::COMMAND_NS + MS / 2).unwrap();
    assert!(w.said("the ladder climbs to its port reset"), "{:#?}", w.host.lines.borrow());
    pipes.set(Pipes::Answer);
    w.run(&usb, &mut tasks, &mut owner, queued + crate::msc::COMMAND_NS + 600 * MS).unwrap();
    let (answers, rest) = answered(&usb);
    assert!(w.said("disk 0's device came back"), "{:#?}", w.host.lines.borrow());
    assert_eq!(answers, [((1, 1), true)]);
    assert!(rest.is_empty(), "{rest:?}");
}

/// A different stick on the port after that reset is a new disk, and the
/// one that left is held for its return window and then ends, its write
/// answered not done and never issued to the stranger.
#[test]
fn a_different_stick_after_the_port_reset_ends_the_disk_that_left() {
    let (w, pipes) = a_write_breaks_into_a_port_reset();
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 600 * MS).unwrap();
    answered(&usb);
    pipes.set(Pipes::SilentOut);
    let queued = w.host.now.get();
    usb.disks.submit(0, Work::Io { write: true, block: 10, blocks: 1, at: LENT_BASE, token: (1, 1) });
    w.run(&usb, &mut tasks, &mut owner, queued + crate::msc::COMMAND_NS + MS / 2).unwrap();
    let (other, other_disk, _) = stick("OTHER");
    w.xhc().devices[0] = Some(Box::new(other));
    w.run(&usb, &mut tasks, &mut owner, queued + crate::msc::COMMAND_NS + crate::identity::RETURN_WINDOW + 100 * MS).unwrap();
    let (answers, rest) = answered(&usb);
    assert_eq!(answers, [((1, 1), false)]);
    assert!(matches!(rest[..], [Event::Arrived { disk: 1, .. }, Event::Gone { disk: 0 }]), "{rest:?}");
    assert_eq!(&other_disk.borrow()[10 * 4096..11 * 4096], &[0u8; 4096][..], "the write reached the stranger");
}

/// A SuperSpeed link whose port is reset the way it cannot take — a hot
/// reset that kills it, or one that completes failed (§4.19.5) — is given
/// the warm reset §4.19.1.2.4 has for it, and its device is enumerated.
#[test]
fn a_superspeed_link_a_hot_reset_breaks_is_recovered_warm() {
    for behaviour in [
        ResetBehaviour::HotResetKillsTheLink { warm_works: true },
        ResetBehaviour::FailsTheBusReset { warm_works: true },
    ] {
        let (kb, _) = keyboard();
        let w = World::new(vec![(FakePort::untrained(behaviour), Some(kb))], 0);
        let usb = w.start(BOTH);
        let mut tasks = usb.tasks();
        let _end = End(&usb);
        let mut owner = Mock::default();
        w.run(&usb, &mut tasks, &mut owner, 4 * crate::port::RESET_DEADLINE_NS).unwrap();
        assert!(w.said("port 1 Warm reset"), "{behaviour:?}: {:#?}", w.host.lines.borrow());
        assert!(usb.named.borrow().contains_key(&0), "{behaviour:?}: {:#?}", w.host.lines.borrow());
        assert_eq!(w.xhc().violations, [], "{behaviour:?}");
    }
}

/// A link a warm reset re-detects and still cannot bring up is given up on
/// by its own name, and not reset again while its device stays in.
#[test]
fn a_link_that_never_trains_is_given_up_on_and_left_alone() {
    let (kb, _) = keyboard();
    let w = World::new(vec![(FakePort::untrained(ResetBehaviour::RetrainsDisabled), Some(kb))], 0);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 4 * crate::port::RESET_DEADLINE_NS).unwrap();
    assert!(w.said("given up on: LinkNeverTrained"), "{:#?}", w.host.lines.borrow());
    let resets = w.host.lines.borrow().iter().filter(|l| l.contains("reset")).count();
    w.run(&usb, &mut tasks, &mut owner, 20 * crate::port::RESET_DEADLINE_NS).unwrap();
    assert_eq!(w.host.lines.borrow().iter().filter(|l| l.contains("reset")).count(), resets, "reset again");
    assert!(usb.settled(), "a port given up on keeps the bus from settling");
}

/// An interrupt whose event an earlier drain already took — a controller's
/// assertion that landed inside a drain — finds the ring empty, and the next
/// event is still announced: every drain ends Event Handler Busy, or no event
/// after it raises an interrupt and the driver waits for ever.
#[test]
fn an_interrupt_that_finds_no_event_leaves_the_next_one_announced() {
    let (kb, reports) = keyboard();
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(kb))], 1);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 400 * MS).unwrap();
    w.xhc().raise_for_nothing();
    w.run(&usb, &mut tasks, &mut owner, 410 * MS).unwrap();
    reports.borrow_mut().push_back(vec![0, 0, 0x04, 0, 0, 0, 0, 0]);
    w.run(&usb, &mut tasks, &mut owner, 420 * MS).unwrap();
    assert_eq!(keys(&owner.got), [(0x04, true)], "the report's event raised no interrupt");
}

/// A stick that answers late, past the transfer's bound, and then answers
/// again: the ladder takes the transport back, the work it was given is
/// answered, and work queued after it is served.
#[test]
fn a_stick_that_answers_late_once_is_served_again() {
    let (stick, _, pipes) = stick("SERIAL1");
    let w = World::new(vec![(FakePort::occupied(QUICK), Some(Box::new(stick)))], 1);
    w.xhc().mem.borrow_mut().lend(LENT_BASE, 64 * 1024);
    let usb = w.start(BOTH);
    let mut tasks = usb.tasks();
    let _end = End(&usb);
    let mut owner = Mock::default();
    w.run(&usb, &mut tasks, &mut owner, 600 * MS).unwrap();
    answered(&usb);
    pipes.set(Pipes::SilentIn);
    let queued = w.host.now.get();
    usb.disks.submit(0, Work::Io { write: true, block: 10, blocks: 1, at: LENT_BASE, token: (1, 1) });
    usb.disks.submit(0, Work::Io { write: false, block: 10, blocks: 1, at: LENT_BASE + 4096, token: (1, 2) });
    w.run(&usb, &mut tasks, &mut owner, queued + crate::msc::COMMAND_NS + 2_000 * MS).unwrap();
    pipes.set(Pipes::Answer);
    w.run(&usb, &mut tasks, &mut owner, queued + 6 * crate::msc::COMMAND_NS).unwrap();
    let (answers, rest) = answered(&usb);
    assert_eq!(answers, [((1, 1), true), ((1, 2), true)], "{rest:?}\n{:#?}", w.host.lines.borrow());
    usb.disks.submit(0, Work::Flush { token: (1, 3) });
    let now = w.host.now.get();
    w.run(&usb, &mut tasks, &mut owner, now + 1_000 * MS).unwrap();
    assert_eq!(answered(&usb).0, [((1, 3), true)]);
}
