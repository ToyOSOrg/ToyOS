//! A move delivers the rights its mover names, never more than the mover
//! holds, and a handle that arrives without `TRANSFER` moves no further. Both
//! moves are the kernel's, a message's handles and a spawn's endowments, and
//! what a handle carries is only observable by what its holder's own calls
//! are refused.
//!
//! - A pipe end sent as `WRITE` alone still writes, and its receiver can
//!   neither duplicate it nor send it on; the same end sent unchanged moves
//!   again, which is what says the probe can pass.
//! - A send asking for a right the handle lacks, or with a word that is no
//!   rights set, is refused and the handle stays.
//! - A narrowed send its connection refuses gives the handle back with the
//!   rights it had.
//! - An endowment is narrowed and refused the same way, seen from the child.
//! - A ring carries no `TRANSFER`: neither move takes it, so its watches are
//!   only ever resolved in the table of the process that made it.

#[path = "../image.rs"]
mod image;

use toyos::ipc::Connection;
use toyos::{namespace, port, AsHandle};
use toyos_abi::end::End;
use toyos_abi::handle::{Rights, TransferEntry};
use toyos_abi::syscall::{self, SpawnArgs, SpawnEndow, SyscallError, MAX_TRANSFER_HANDLES};
use toyos_abi::RawHandle;

const SELF: &str = "/system/bin/test_rs_moved_rights";
/// The name the child goes by.
const NAME: &str = "moved_rights";
/// The label a child finds its endowment under.
const LABEL: &str = "moved";
/// The role a spawned copy takes: its exit code is what it found its
/// endowment to carry ([`code`]).
const CHILD: &str = "endowed";

/// A word with a bit no right uses.
const NO_RIGHTS: u32 = 1 << 30;

fn main() {
    if std::env::args().nth(1).as_deref() == Some(CHILD) {
        let endowed = toyos::endow::Endowments::get().take::<toyos::OwnedHandle>(LABEL).expect("the endowment");
        std::process::exit(code(carries(&Pair::new(), endowed.into_raw())));
    }
    // Each arm is judged by itself, so one red names every claim that broke.
    let arms: [(&str, fn(&Pair)); 5] = [
        ("a handle sent as WRITE alone writes and moves no further", a_narrowed_send_arrives_with_what_it_named),
        ("a send past the handle's rights, or of no rights set, moves nothing", a_send_cannot_widen),
        ("a refused narrowed send leaves the handle its rights", a_refused_send_gives_back_the_rights_it_had),
        ("an endowment arrives with what it named, and a wider one is refused", an_endowment_is_narrowed_the_same),
        ("a ring is never moved", a_ring_is_never_moved),
    ];
    let mut red = Vec::new();
    for (claim, arm) in arms {
        match std::panic::catch_unwind(|| arm(&Pair::new())) {
            Ok(()) => println!("moved_rights: {claim}"),
            Err(_) => red.push(claim),
        }
    }
    assert!(red.is_empty(), "moved_rights: red: {red:#?}");
    println!("moved_rights: ok");
}

/// A connection to this process itself: what one end sends the other takes.
struct Pair {
    client: Connection,
    server: Connection,
}

impl Pair {
    fn new() -> Self {
        let (acceptor, connector) = port::create().expect("a port");
        let names = namespace::build().add("conn", &connector).finish().expect("a namespace");
        let client = names.open("conn").expect("connect");
        Self { client, server: acceptor.accept().expect("accept") }
    }

    fn send(&self, entry: TransferEntry) -> Result<(), SyscallError> {
        // SAFETY: no type holds the numbers these arms move.
        unsafe { syscall::message_send(self.client.as_handle().raw(), b"m", &[entry], true) }
    }

    /// The one handle of the next message.
    fn take(&self) -> RawHandle {
        let mut handles = [RawHandle(0); MAX_TRANSFER_HANDLES];
        let got = syscall::message_recv(self.server.as_handle().raw(), &mut [], &mut [0; 1], &mut handles, true)
            .expect("the message just sent");
        assert_eq!(got.handles, 1, "the message carried {} handles", got.handles);
        handles[0]
    }

    /// `h` moved through the connection as `entry` asks: its number after.
    fn moved(&self, entry: TransferEntry) -> Result<RawHandle, SyscallError> {
        self.send(entry).map(|()| self.take())
    }
}

/// A pipe's write end, which carries `WRITE|WAIT|DUP|TRANSFER`, and its read end.
fn pipe() -> (RawHandle, RawHandle) {
    let ends = syscall::pipe().expect("a pipe");
    (ends.write, ends.read)
}

fn close(handles: &[RawHandle]) {
    for &h in handles {
        // SAFETY: no type holds this number.
        unsafe { syscall::close(h) };
    }
}

/// What a holder of `h` finds it carries: whether it writes, duplicates and moves.
fn carries(pair: &Pair, h: RawHandle) -> (bool, bool, bool) {
    let writes = syscall::write(h, b"x") == Ok(1);
    // SAFETY: nothing relies on being the source's only holder.
    let dup = unsafe { syscall::dup(h) };
    let duplicates = dup.is_ok();
    assert!(duplicates || dup == Err(SyscallError::PermissionDenied), "a refused dup answered {dup:?}");
    close(&dup.into_iter().collect::<Vec<_>>());
    let moved = pair.send(TransferEntry::unchanged(h));
    let moves = moved.is_ok();
    assert!(moves || moved == Err(SyscallError::PermissionDenied), "a refused move answered {moved:?}");
    if moves {
        close(&[pair.take()]);
    }
    (writes, duplicates, moves)
}

fn a_narrowed_send_arrives_with_what_it_named(pair: &Pair) {
    let (write, read) = pipe();
    let arrived = pair.moved(TransferEntry::narrowed(write, Rights::WRITE)).expect("a send narrowed to WRITE");
    assert_eq!(carries(pair, arrived), (true, false, false), "what a handle sent as WRITE alone (writes, duplicates, moves)");
    // Asking for less of a handle that cannot move is still a move.
    assert_eq!(pair.send(TransferEntry::narrowed(arrived, Rights::NONE)), Err(SyscallError::PermissionDenied));
    close(&[arrived, read]);

    let (write, read) = pipe();
    let arrived = pair.moved(TransferEntry::unchanged(write)).expect("a send unchanged");
    assert_eq!(carries(pair, arrived), (true, true, true), "what a handle sent unchanged (writes, duplicates, moves)");
    close(&[read]);
}

fn a_send_cannot_widen(pair: &Pair) {
    let (write, read) = pipe();
    // A pipe's write end has no `READ`.
    let wider = Rights::WRITE.union(Rights::READ);
    assert_eq!(pair.send(TransferEntry::narrowed(write, wider)), Err(SyscallError::PermissionDenied), "a send asking for READ");
    assert_eq!(
        pair.send(TransferEntry { handle: write, rights: NO_RIGHTS }),
        Err(SyscallError::InvalidArgument),
        "a send with a word that is no rights set"
    );
    assert_eq!(carries(pair, write), (true, true, true), "what the refused sends left (writes, duplicates, moves)");
    close(&[read]);
}

fn a_refused_send_gives_back_the_rights_it_had(pair: &Pair) {
    let gone = Pair::new();
    let Pair { client, server } = gone;
    drop(server);
    let (write, read) = pipe();
    let entry = TransferEntry::narrowed(write, Rights::NONE);
    // SAFETY: no type holds this number.
    let sent = unsafe { syscall::message_send(client.as_handle().raw(), b"m", &[entry], true) };
    assert_eq!(sent, Err(SyscallError::Gone), "a send to a peer that has gone");
    assert_eq!(carries(pair, write), (true, true, true), "what a refused narrowed send left (writes, duplicates, moves)");
    close(&[read]);
}

/// `SYS_SPAWN` of this binary as [`CHILD`], endowed `moved` under [`LABEL`].
fn spawn(moved: TransferEntry) -> Result<RawHandle, SyscallError> {
    static OWN: std::sync::OnceLock<toyos::process::Image> = std::sync::OnceLock::new();
    let laid = image::laid(OWN.get_or_init(|| image::image_of(SELF)), &format!("{SELF}\0{CHILD}"));
    let entry = SpawnEndow { label_off: 0, label_len: LABEL.len() as u32, moved };
    // SAFETY: every pointer names a live local for the whole call.
    unsafe {
        syscall::spawn(&SpawnArgs {
            path_ptr: SELF.as_ptr() as u64,
            path_len: SELF.len() as u64,
            name_ptr: NAME.as_ptr() as u64,
            name_len: NAME.len() as u64,
            endow_ptr: &entry as *const SpawnEndow as u64,
            endow_count: 1,
            labels_ptr: LABEL.as_ptr() as u64,
            labels_len: LABEL.len() as u64,
            maps_ptr: laid.maps().as_ptr() as u64,
            maps_count: laid.maps().len() as u64,
            entry: laid.entry,
            image_base: laid.image_base,
            build_id_ptr: laid.build_id().as_ptr() as u64,
            build_id_len: laid.build_id().len() as u64,
            place: u64::from(toyos_abi::HANDLE_INVALID.0),
            pager: u64::from(toyos_abi::HANDLE_INVALID.0),
        })
    }
}

/// The child's exit code says what it found its endowment to carry.
fn code((writes, duplicates, moves): (bool, bool, bool)) -> i32 {
    10 + i32::from(writes) + 2 * i32::from(duplicates) + 4 * i32::from(moves)
}

fn an_endowment_is_narrowed_the_same(pair: &Pair) {
    let ended = |child: RawHandle| {
        let end = syscall::process_wait(child);
        close(&[child]);
        end
    };
    let (write, read) = pipe();
    let child = spawn(TransferEntry::narrowed(write, Rights::WRITE)).expect("a spawn endowing WRITE alone");
    assert_eq!(ended(child), Ok(End::Exited(code((true, false, false)))), "what a child endowed WRITE alone found");
    close(&[read]);

    let (write, read) = pipe();
    let child = spawn(TransferEntry::unchanged(write)).expect("a spawn endowing unchanged");
    assert_eq!(ended(child), Ok(End::Exited(code((true, true, true)))), "what a child endowed unchanged found");
    close(&[read]);

    let (write, read) = pipe();
    let wider = Rights::WRITE.union(Rights::READ);
    assert_eq!(spawn(TransferEntry::narrowed(write, wider)).err(), Some(SyscallError::PermissionDenied), "a spawn endowing READ");
    assert_eq!(
        spawn(TransferEntry { handle: write, rights: NO_RIGHTS }).err(),
        Some(SyscallError::InvalidArgument),
        "a spawn endowing a word that is no rights set"
    );
    assert_eq!(carries(pair, write), (true, true, true), "what the refused spawns left (writes, duplicates, moves)");
    close(&[read]);
}

fn a_ring_is_never_moved(pair: &Pair) {
    // SAFETY: the page stays mapped while `ring` is open, and is not read.
    let (ring, _) = unsafe { syscall::inbox_setup(8) }.expect("a ring");
    for entry in [TransferEntry::unchanged(ring), TransferEntry::narrowed(ring, Rights::NONE)] {
        assert_eq!(pair.send(entry), Err(SyscallError::PermissionDenied), "a ring sent to a peer");
        assert_eq!(spawn(entry).err(), Some(SyscallError::PermissionDenied), "a ring endowed to a child");
    }
    // SAFETY: nothing relies on being the source's only holder.
    let dup = unsafe { syscall::dup(ring) }.expect("a second handle to a ring in its own process");
    // SAFETY: nothing relies on being the source's only holder.
    assert_eq!(
        unsafe { syscall::dup_narrowed(ring, Rights::TRANSFER) },
        Err(SyscallError::PermissionDenied),
        "a duplicate of a ring carrying TRANSFER"
    );
    assert_eq!(pair.send(TransferEntry::unchanged(dup)), Err(SyscallError::PermissionDenied), "a ring's duplicate sent to a peer");
    syscall::inbox_submit(ring, 0, 0, 0).expect("the ring the refusals left");
    close(&[dup, ring]);
}
