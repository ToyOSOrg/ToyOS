//! What one process reaches another through: pipes, ports and the namespaces
//! that name them, connections, shared memory, and inboxes.
//! No peer is addressed by name or pid: ports, namespaces and connections are
//! built only from connectors and handles the caller already holds.
//! Every all-or-nothing claim here is structural: whatever can refuse is
//! resolved and checked before anything is installed or removed, or under the
//! hold that installs it. No handle is taken back across a released hold: a
//! sibling thread can close or move one the moment the table's lock is given up.

use alloc::vec::Vec;

use crate::watch;
use crate::object::{ops, port, KObjectRef};
use crate::message::{MessageBounce, QueueRef, SendRefused, Unclaimed, Unsent};
use crate::object::handle::{HandleEntry, HandleTable};
use crate::object::service::ConnectionEnd;
use crate::time::Deadline;
use crate::user_ptr::SyscallContext;
use crate::UserAddr;
use crate::{message, pipe, process};

use toyos_abi::handle::{NotRights, RawHandle, Rights, TransferEntry};
use toyos_abi::syscall::*;
use kernel::sched::task::WaitClass;

use super::{cancelled, HANDLE_LEN, TRANSFER_ENTRY_LEN};
use super::handles::handle_result;

pub(super) fn sys_pipe() -> u64 {
    let ends = pipe::create().and_then(|(reader, writer)| {
        let read_end = KObjectRef::PipeRead(crate::object::pipe::PipeReadEnd::new(reader)?);
        Ok((read_end, KObjectRef::PipeWrite(crate::object::pipe::PipeWriteEnd::new(writer)?)))
    });
    match ends {
        Ok((read_end, write_end)) => install_pair(read_end, write_end),
        Err(e) => SyscallError::from(e).to_u64(),
    }
}

/// Install two objects or neither; the two handles come back packed into one word, which cannot be read as an error.
pub(super) fn install_pair(first: KObjectRef, second: KObjectRef) -> u64 {
    let installed = process::with_handles(|handles| {
        // Room for both before either: a refused pair installs nothing, so nothing is taken back.
        if !handles.make_room(2) {
            return Err((first, second));
        }
        let mut install =
            |object| ops::install(handles, object).expect("room was asked for first");
        let first = install(first);
        let second = install(second);
        Ok(((first.0 as u64) << 32) | second.0 as u64)
    });
    match installed {
        Ok(word) => word,
        // A refused pair drops here, with the hold given up.
        Err(_pair) => SyscallError::ResourceExhausted.to_u64(),
    }
}

/// Make a port and install both ends; needs no right and grants none, since a port with no clients is not authority.
pub(super) fn sys_port_create() -> u64 {
    match port::create() {
        Ok((acceptor, connector)) => install_pair(KObjectRef::Acceptor(acceptor), KObjectRef::Connector(connector)),
        Err(e) => SyscallError::from(e).to_u64(),
    }
}

/// Build a namespace from a base's kept names plus new bindings; a refusal leaves the caller's table unchanged (every name resolved and connector checked first).
pub(super) fn sys_namespace_build(ctx: &SyscallContext, args: &NamespaceBuild) -> u64 {
    // A bit this kernel does not define is a caller asking for something it will not get, so it is refused rather than skipped.
    if args.flags & !NAMESPACE_FLAGS_KNOWN != 0 {
        return SyscallError::InvalidArgument.to_u64();
    }
    let keep_all = args.flags & NAMESPACE_KEEP_ALL != 0;
    if keep_all && args.keep_n > 0 {
        return SyscallError::InvalidArgument.to_u64();
    }
    let total = args.keep_n.saturating_add(args.add_n);
    if total > MAX_NAMESPACE_ENTRIES as u64 {
        return SyscallError::InvalidArgument.to_u64();
    }
    if args.names_len > (MAX_NAMESPACE_ENTRIES * MAX_SERVICE_NAME) as u64 {
        return SyscallError::InvalidArgument.to_u64();
    }
    let names = match ctx.user_vec(UserAddr::new(args.names_ptr), args.names_len) {
        Ok(bytes) => bytes,
        Err(e) => return e.to_u64(),
    };
    let name_at = |off: u32, len: u32| -> Option<alloc::boxed::Box<str>> {
        let end = (off as usize).checked_add(len as usize)?;
        let bytes = names.get(off as usize..end)?;
        Some(alloc::string::String::from(core::str::from_utf8(bytes).ok()?).into_boxed_str())
    };

    let mut entries: Vec<(alloc::boxed::Box<str>, alloc::sync::Arc<port::Connector>)> =
        Vec::new();

    if keep_all {
        let base = match process::with_handles(|handles| {
            handles.get::<crate::object::namespace::Namespace>(args.base, Rights::READ)
        }) {
            Ok(base) => base,
            Err(e) => return e.refuse(),
        };
        for (name, connector) in base.entries() {
            entries.push((name.clone(), connector.clone()));
        }
    }

    if args.keep_n > 0 {
        let Some(keep) = (args.keep_n as usize)
            .checked_mul(core::mem::size_of::<NameRef>())
            .and_then(|len| ctx.user_bytes(UserAddr::new(args.keep_ptr), len as u64))
        else {
            return SyscallError::BadAddress.to_u64();
        };
        let base = match process::with_handles(|handles| {
            handles.get::<crate::object::namespace::Namespace>(args.base, Rights::READ)
        }) {
            Ok(base) => base,
            Err(e) => return e.refuse(),
        };
        for i in 0..args.keep_n as usize {
            let mut raw = [0u8; 8];
            if keep.read_at(i * 8, &mut raw).is_err() {
                return SyscallError::BadAddress.to_u64();
            }
            let off = u32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]);
            let len = u32::from_ne_bytes([raw[4], raw[5], raw[6], raw[7]]);
            let Some(name) = name_at(off, len) else {
                return SyscallError::InvalidArgument.to_u64();
            };
            // A name absent from the base is silently absent from the child: narrowing asks for an intersection, not an error.
            if let Some(connector) = base.lookup(&name) {
                entries.push((name, connector.clone()));
            }
        }
    }

    if args.add_n > 0 {
        let Some(add) = (args.add_n as usize)
            .checked_mul(core::mem::size_of::<NamespaceEntry>())
            .and_then(|len| ctx.user_bytes(UserAddr::new(args.add_ptr), len as u64))
        else {
            return SyscallError::BadAddress.to_u64();
        };
        for i in 0..args.add_n as usize {
            let mut raw = [0u8; 16];
            if add.read_at(i * 16, &mut raw).is_err() {
                return SyscallError::BadAddress.to_u64();
            }
            let off = u32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]);
            let len = u32::from_ne_bytes([raw[4], raw[5], raw[6], raw[7]]);
            let handle = RawHandle(u32::from_ne_bytes([raw[8], raw[9], raw[10], raw[11]]));
            let Some(name) = name_at(off, len) else {
                return SyscallError::InvalidArgument.to_u64();
            };
            let connector = match process::with_handles(|handles| {
                handles.get_sent::<port::Connector>(handle, Rights::TRANSFER)
            }) {
                Ok(c) => c,
                Err(e) => return e.refuse(),
            };
            entries.push((name, connector));
        }
    }

    let namespace = match crate::object::namespace::Namespace::build(entries) {
        Ok(ns) => ns,
        Err(crate::object::namespace::BuildError::TooMany) => {
            return SyscallError::InvalidArgument.to_u64()
        }
        Err(crate::object::namespace::BuildError::Duplicate) => {
            return SyscallError::AlreadyExists.to_u64()
        }
        Err(crate::object::namespace::BuildError::Exhausted) => {
            return SyscallError::ResourceExhausted.to_u64()
        }
    };
    process::with_handles(|handles| {
        handle_result(ops::install(handles, KObjectRef::Namespace(namespace)))
    })
}

/// Open a connection to `name` in the namespace `ns_h` holds; `NotFound` means the namespace lacks the name, `Gone` means its port has closed.
pub(super) fn sys_namespace_open(ns_h: RawHandle, name: &str) -> u64 {
    let connector = match process::with_handles(|handles| {
        let ns = handles.get::<crate::object::namespace::Namespace>(ns_h, Rights::READ)?;
        Ok::<_, crate::object::HandleError>(ns.lookup(name).cloned())
    }) {
        Ok(Some(c)) => c,
        Ok(None) => return SyscallError::NotFound.to_u64(),
        Err(e) => return e.refuse(),
    };
    connect_through(&connector)
}

/// Inspect only the names this namespace already holds. The Arc keeps the
/// immutable entries alive after the handle-table lock is released; copying
/// into user memory therefore holds no object or handle-table lock.
pub(super) fn sys_namespace_names(ns_h: RawHandle, out: &mut crate::user_ptr::UserBytesMut) -> u64 {
    let namespace = match process::with_handles(|handles| {
        handles.get::<crate::object::namespace::Namespace>(ns_h, Rights::READ)
    }) {
        Ok(namespace) => namespace,
        Err(e) => return e.refuse(),
    };
    let required: usize = namespace.entries().iter().map(|(name, _)| 2 + name.len()).sum();
    if out.len() >= required {
        let mut at = 0;
        for (name, _) in namespace.entries() {
            let wrote = out.write_at(at, &(name.len() as u16).to_le_bytes()).and_then(|()| out.write_at(at + 2, name.as_bytes()));
            if let Err(faulted) = wrote {
                return SyscallError::from(faulted).to_u64();
            }
            at += 2 + name.len();
        }
    }
    required as u64
}

/// The client half of a connection, and the server half queued on the port.
fn connect_through(connector: &port::Connector) -> u64 {
    if connector.closed() {
        return SyscallError::Gone.to_u64();
    }
    let made = (|| {
        let (to_server_rx, to_server_tx) = message::create()?;
        let (to_client_rx, to_client_tx) = message::create()?;
        // Cross-wired here only: the server's end is built from the pending half when it accepts.
        let object = KObjectRef::Connection(ConnectionEnd::new(to_client_rx, to_server_tx)?);
        let pending = port::PendingConnection { rx: to_server_rx, tx: to_client_tx, stamp: connector.stamp() };
        Ok::<_, crate::mm::charge::Exhausted>((object, pending))
    })();
    let (object, pending) = match made {
        Ok(made) => made,
        Err(e) => return SyscallError::from(e).to_u64(),
    };
    // One hold from the room to the install: the port can still refuse, and the client's end is in the table only once it has not.
    let installed = process::with_handles(|handles| {
        if !handles.make_room(1) {
            return Err((SyscallError::ResourceExhausted, object, pending));
        }
        match connector.push(pending) {
            Ok(()) => Ok(ops::install(handles, object).expect("room was asked for first")),
            Err((pending, port::PushError::Closed)) => Err((SyscallError::Gone, object, pending)),
            Err((pending, port::PushError::QueueFull)) => {
                Err((SyscallError::ResourceExhausted, object, pending))
            }
        }
    });
    let h = match installed {
        Ok(h) => h,
        // Both halves of a refused connection drop here, with the hold given up: their queues go with them.
        Err((e, _client, _server)) => return e.to_u64(),
    };
    // The port's one watch carries both: the server blocked in `accept` and every ring polling it.
    connector.port().watch().post();
    h.0 as u64
}

pub(super) fn sys_accept(h: RawHandle, flags: u64) -> u64 {
    let waits = match flags {
        0 => true,
        ACCEPT_NONBLOCK => false,
        _ => return SyscallError::InvalidArgument.to_u64(),
    };
    let acceptor = match process::with_handles(|handles| {
        handles.get::<port::Acceptor>(h, Rights::READ)
    }) {
        Ok(a) => a,
        Err(e) => return e.refuse(),
    };

    loop {
        // Charged before a connection is taken, so a refusal leaves it queued.
        let core = match ConnectionEnd::accept_core() {
            Ok(core) => core,
            Err(e) => return SyscallError::from(e).to_u64(),
        };
        if let Some(conn) = acceptor.pop() {
            // The ends move from the port's queue into the connection: ownership transfers, no count changes.
            let object = KObjectRef::Connection(ConnectionEnd::accepted(core, conn));
            let installed = process::with_handles(|handles| {
                ops::install(handles, object)
            });
            return handle_result(installed);
        }
        // A closed acceptor will never queue again, so returning here is the only alternative to parking forever.
        if acceptor.closed() {
            return SyscallError::Gone.to_u64();
        }
        if !waits {
            return SyscallError::WouldBlock.to_u64();
        }
        let parkable = crate::scheduler::Parkable::at_entry();
        if watch::wait_until(
            &parkable,
            acceptor.watch(),
            0,
            WaitClass::Ipc,
            Deadline::never(),
            || acceptor.has_pending() || acceptor.closed(),
        )
        .is_err()
        {
            return cancelled();
        }
    }
}

/// A connector to the caller's own port stamping every connection with `badge`, which the dispatch has copied in and bounded. `READ` on the acceptor, the right accepting takes, is the authority: whoever answers every connection on a port grants nothing by labelling them.
pub(super) fn sys_port_mint(h: RawHandle, badge: &[u8]) -> u64 {
    let acceptor = match process::with_handles(|handles| {
        handles.get::<port::Acceptor>(h, Rights::READ)
    }) {
        Ok(a) => a,
        Err(e) => return e.refuse(),
    };
    let connector = match acceptor.mint(alloc::sync::Arc::from(badge)) {
        Ok(connector) => KObjectRef::Connector(connector),
        Err(e) => return SyscallError::from(e).to_u64(),
    };
    process::with_handles(|handles| handle_result(ops::install(handles, connector)))
}

/// The badge a connection accepted from `acceptor_h`'s port was stamped with, written to `out`; answers its length.
///
/// **The port check is the whole boundary**: any process can make a port and mint any bytes on it, so a stamp is answered only to the acceptor of the port it was made on. A connection with no stamp (a client's end) is `InvalidArgument`, one from another port `PermissionDenied`, and one made through an unbadged connector `NotFound`, never an empty badge.
pub(super) fn sys_port_badge(
    acceptor_h: RawHandle,
    conn_h: RawHandle,
    out: &mut crate::user_ptr::UserBytesMut,
) -> u64 {
    let found = process::with_handles(|handles| {
        let acceptor = handles.get::<port::Acceptor>(acceptor_h, Rights::READ)?;
        let conn = handles.get::<ConnectionEnd>(conn_h, Rights::READ)?;
        Ok::<_, crate::object::HandleError>((acceptor.port_id(), conn.stamp().cloned()))
    });
    let (port, stamp) = match found {
        Ok(found) => found,
        Err(e) => return e.refuse(),
    };
    let Some(stamp) = stamp else { return SyscallError::InvalidArgument.to_u64() };
    if stamp.port != port {
        return SyscallError::PermissionDenied.to_u64();
    }
    let Some(badge) = stamp.badge else { return SyscallError::NotFound.to_u64() };
    out.write_at(0, &badge).map_or_else(|faulted| SyscallError::from(faulted).to_u64(), |()| badge.len() as u64)
}

/// Make a shared region and hand back the one handle to it, carrying MAP, WRITE, DUP and TRANSFER — naming it is what authorizes mapping it.
/// `SHM_DEVICE` asks for memory a device can be lent, and `SHM_FOR_JOB`
/// counts it to the job `job` names rather than the caller's; any other
/// bit is refused.
pub(super) fn sys_shm_create(size: u64, flags: u64, job: RawHandle) -> u64 {
    let charged = match flags & SHM_FOR_JOB {
        0 => crate::mm::charge::current(),
        _ => match process::with_handles(|handles| handles.get::<crate::object::job::JobObject>(job, Rights::WRITE)) {
            Ok(object) => alloc::sync::Arc::clone(object.job()),
            Err(e) => return e.refuse(),
        },
    };
    let created = match flags & !SHM_FOR_JOB {
        0 => crate::object::shm::SharedMemObject::create(&charged, size),
        SHM_DEVICE => crate::object::shm::SharedMemObject::create_run(&charged, size),
        _ => return SyscallError::InvalidArgument.to_u64(),
    };
    let object = match created {
        Ok(shm) => KObjectRef::SharedMem(shm),
        Err(e) => return e.to_u64(),
    };
    process::with_handles(|handles| handle_result(ops::install(handles, object)))
}

/// `toyos_abi::syscall::shm_sub`: the new object's handle carries `h`'s rights.
pub(super) fn sys_shm_sub(h: RawHandle, offset: u64, len: u64) -> u64 {
    // A refusal is made with nothing held: a bad handle's ends the process.
    let held = process::with_handles(|handles| {
        let whole = handles.get::<crate::object::shm::SharedMemObject>(h, Rights::MAP)?;
        Ok::<_, crate::object::handle::HandleError>((whole, handles.rights_of(h)?))
    });
    let (whole, rights) = match held {
        Ok(held) => held,
        Err(e) => return e.refuse(),
    };
    let sub = match crate::object::shm::SharedMemObject::sub(&whole, offset, len) {
        Ok(sub) => sub,
        Err(e) => return e.to_u64(),
    };
    let entry = crate::object::handle::HandleEntry::new(KObjectRef::SharedMem(sub), rights);
    process::with_handles(|handles| {
        handle_result(handles.install(entry).map_err(|_| SyscallError::ResourceExhausted))
    })
}

/// A send's arguments as the dispatch bounded them.
pub(super) struct Outgoing<'a> {
    pub bytes: crate::user_ptr::UserBytes<'a>,
    pub handles: crate::user_ptr::UserBytes<'a>,
    pub count: usize,
}

/// A receive's buffers as the dispatch bounded them.
pub(super) struct Incoming<'a> {
    /// Where the message's first bytes go, before [`Self::bytes`].
    pub head: crate::user_ptr::UserBytesMut<'a>,
    pub bytes: crate::user_ptr::UserBytesMut<'a>,
    pub handles: crate::user_ptr::UserBytesMut<'a>,
    pub cap: usize,
}

/// Queue a whole message on a connection and move its handles to the peer,
/// together or not at all ([`toyos_abi::syscall::message_send`]), each with
/// the rights its [`TransferEntry`] names. Waits for room unless `nonblock`.
pub(super) fn sys_message_send(conn_h: RawHandle, out: &Outgoing<'_>, nonblock: bool) -> u64 {
    send(conn_h, out, nonblock).map_or_else(|word| word, |()| 0)
}

/// Take a connection's next whole message ([`toyos_abi::syscall::message_recv`]).
pub(super) fn sys_message_recv(conn_h: RawHandle, into: &mut Incoming<'_>, nonblock: bool) -> u64 {
    recv(conn_h, into, nonblock)
}

/// [`sys_message_send`]'s work, or the word its refusal answers.
fn send(conn_h: RawHandle, out: &Outgoing<'_>, nonblock: bool) -> Result<(), u64> {
    let mut wanted = [(RawHandle(0), None); MAX_TRANSFER_HANDLES];
    for (i, slot) in wanted.iter_mut().enumerate().take(out.count) {
        let mut entry: TransferEntry = toyos_abi::usersafe::zeroed();
        out.handles
            .read_at(i * TRANSFER_ENTRY_LEN, toyos_abi::usersafe::bytes_mut(&mut entry))
            .map_err(|faulted| SyscallError::from(faulted).to_u64())?;
        *slot = (entry.handle, entry.rights().map_err(|NotRights| SyscallError::InvalidArgument.to_u64())?);
    }
    let wanted = &wanted[..out.count];
    // `TRANSFER` on the connection only when it carries handles: bytes alone
    // are a write, which `WRITE` already allows.
    let need = if out.count == 0 { Rights::WRITE } else { Rights::WRITE.union(Rights::TRANSFER) };
    // Every handle is checked before anything is queued or moved, and again
    // under the hold that moves them: the answer is what each arrives with.
    let checked = |handles: &HandleTable| {
        let conn = handles.get::<ConnectionEnd>(conn_h, need)?;
        let mut moving = [(RawHandle(0), Rights::NONE); MAX_TRANSFER_HANDLES];
        for (i, (h, want)) in wanted.iter().enumerate() {
            if *h == conn_h || wanted[..i].iter().any(|(seen, _)| seen == h) {
                return Err(SyscallError::InvalidArgument.into());
            }
            moving[i] = (*h, handles.rights_to_move(*h, *want)?);
        }
        Ok::<_, crate::object::Refusal>((conn.tx().clone(), moving))
    };

    // The message's bytes, copied in once before any lock.
    let mut bounce = MessageBounce::new();
    let src = bounce.read(&out.bytes).map_err(|faulted| SyscallError::from(faulted).to_u64())?.bytes();
    let parkable = crate::scheduler::Parkable::at_entry();
    loop {
        let moved = process::with_handles(|handles| {
            let (tx, moving) = checked(handles)?;
            let (moved, handed) = move_handles(handles, &moving[..out.count], |batch| tx.send_now(src, batch));
            Ok::<_, crate::object::Refusal>((tx, moved, handed))
        });
        let (tx, refused) = match moved {
            Ok((tx, Ok(()), handed)) => {
                sent(&tx, handed);
                return Ok(());
            }
            Ok((tx, Err(Unsent::Refused(refused)), _)) => (tx, Some(refused)),
            // Its bytes want frames, or another send's claim is taking them.
            Ok((tx, Err(Unsent::Claim), _)) => {
                let refused = match tx.claim_send(&parkable, src, out.count) {
                    Ok(mut claim) => {
                        let moved = process::with_handles(|handles| {
                            let (_, moving) = checked(handles)?;
                            Ok::<_, crate::object::Refusal>(move_handles(handles, &moving[..out.count], |batch| claim.commit(batch)))
                        });
                        drop(claim);
                        match moved {
                            Ok((Ok(()), handed)) => {
                                sent(&tx, handed);
                                None
                            }
                            Ok((Err(refused), _)) => Some(refused),
                            Err(e) => return Err(e.refuse()),
                        }
                    }
                    Err(refused) => Some(refused),
                };
                (tx, refused)
            }
            Err(e) => return Err(e.refuse()),
        };
        let Some(refused) = refused else { return Ok(()) };
        match refused {
            SendRefused::NoRoom if !nonblock => {}
            SendRefused::NoRoom => return Err(SyscallError::WouldBlock.to_u64()),
            SendRefused::Gone => return Err(SyscallError::Gone.to_u64()),
            SendRefused::NoMemory => return Err(SyscallError::ResourceExhausted.to_u64()),
            SendRefused::Cancelled => return Err(cancelled()),
        }
        // Outside every lock: room appears as the receiver takes messages, and each receive posts this watch.
        let (len, count) = (out.bytes.len(), out.count);
        if watch::wait_until(&parkable, tx.write_watch(), 0, WaitClass::Ipc, Deadline::never(), || tx.fits(len, count)).is_err() {
            return Err(cancelled());
        }
    }
}

/// Move `wanted`'s handles, checked under this same hold and each arriving
/// with the rights beside it, through `sink`,
/// which queues them with their message or refuses and hands them back. A
/// device claim that moves takes back every mapping made through it: what it
/// handed out is taken off it here, before the send shows it to a receiver
/// that may hand out its own, and is answered to revoke once the lock is let
/// go. A refusal restores every handle at its own number, and every mapping,
/// so it is invisible.
fn move_handles<E>(
    handles: &mut HandleTable,
    wanted: &[(RawHandle, Rights)],
    sink: impl FnOnce(Vec<HandleEntry>) -> Result<(), (Vec<HandleEntry>, E)>,
) -> (Result<(), E>, Vec<crate::pcidev::Handed>) {
    let claims: Vec<_> = wanted
        .iter()
        .filter_map(|(h, _)| handles.get::<crate::object::device::DeviceClaim>(*h, Rights::TRANSFER).ok())
        .filter_map(|claim| claim.take_mappings().map(|handed| (claim, handed)))
        .collect();
    let moved = handles.transfer(wanted, sink);
    let handed = match moved {
        Ok(()) => claims.into_iter().map(|(_, handed)| handed).collect(),
        Err(_) => {
            for (claim, handed) in claims {
                claim.restore_mappings(handed);
            }
            Vec::new()
        }
    };
    (moved, handed)
}

/// A message is queued: with every lock let go, revoke what its device
/// claims handed out and wake the receiver.
fn sent(tx: &QueueRef, handed: Vec<crate::pcidev::Handed>) {
    for handed in handed {
        handed.revoke();
    }
    crate::scheduler::post_lending(tx.read_watch(), || tx.set_rt_boost_pending());
}

fn recv(conn_h: RawHandle, into: &mut Incoming<'_>, nonblock: bool) -> u64 {
    let parkable = crate::scheduler::Parkable::at_entry();
    let (buf, cap) = (into.head.len() + into.bytes.len(), into.cap);
    let mut bounce = MessageBounce::new();
    loop {
        // Taken, and its handles installed, in the hold that looks the
        // connection up; its bytes are copied out after.
        let looked = process::with_handles(|handles| {
            let rx = handles.get::<ConnectionEnd>(conn_h, Rights::READ)?.rx().clone();
            let taken = loop {
                match rx.recv_now(handles, buf, cap, &mut bounce) {
                    // Grown with the queue's lock let go, then asked again.
                    Err(Unclaimed::Room(n)) if handles.make_room(n) => {}
                    Err(Unclaimed::Room(_)) => break Err(Unclaimed::Untaken(kernel::message::Untaken::NoRoom)),
                    taken => break taken,
                }
            };
            Ok::<_, crate::object::HandleError>((rx, taken))
        });
        let rx = match looked {
            // Refused outside this hold: ending the caller takes the same non-reentrant lock.
            Err(e) => return e.refuse(),
            Ok((rx, Ok(received))) => {
                let bytes = bounce.bytes();
                let first = bytes.len().min(into.head.len());
                let copied = into.head.write_at(0, &bytes[..first]).and_then(|()| into.bytes.write_at(0, &bytes[first..]));
                let (head, installed) = (received.head, received.installed);
                drop(received);
                return match copied {
                    Ok(()) => delivered(&rx, into, head, &installed),
                    Err(faulted) => undelivered(&rx, &installed[..head.handles], faulted),
                };
            }
            Ok((_, Err(Unclaimed::Gone))) => return SyscallError::Gone.to_u64(),
            Ok((_, Err(Unclaimed::Untaken(kernel::message::Untaken::TooSmall)))) => return SyscallError::InvalidArgument.to_u64(),
            Ok((_, Err(Unclaimed::Untaken(kernel::message::Untaken::NoRoom)))) => return SyscallError::ResourceExhausted.to_u64(),
            Ok((_, Err(Unclaimed::Room(_)))) => unreachable!("a table that would not grow answers NoRoom"),
            Ok((_, Err(Unclaimed::Empty))) if nonblock => return SyscallError::WouldBlock.to_u64(),
            Ok((rx, Err(Unclaimed::Empty))) => rx,
        };
        if watch::wait_until(&parkable, rx.read_watch(), 0, WaitClass::Ipc, Deadline::never(), || rx.readable()).is_err() {
            return cancelled();
        }
    }
}

/// A message is taken, its bytes in the reader's buffers: answer its
/// handles' numbers and wake the sender, for its room.
fn delivered(rx: &QueueRef, into: &mut Incoming<'_>, head: kernel::message::Head, installed: &[RawHandle]) -> u64 {
    let installed = &installed[..head.handles];
    let told = installed.iter().enumerate().try_for_each(|(i, h)| into.handles.write_at(i * HANDLE_LEN, &h.0.to_ne_bytes()));
    if let Err(faulted) = told {
        return undelivered(rx, installed, faulted);
    }
    rx.write_watch().post();
    toyos_abi::syscall::Received::encode(head.len, head.handles)
}

/// A message taken whose reader's buffers faulted: its bytes are lost, and
/// its handles, installed at numbers the reader was never told, are closed.
fn undelivered(rx: &QueueRef, installed: &[RawHandle], faulted: crate::user_ptr::Faulted) -> u64 {
    process::with_handles(|handles| {
        for &h in installed {
            // A sibling may have closed the number since, or reused it for a
            // handle of its own, which this then closes: the race is the
            // misbehaving process's own, and stays inside it.
            let _ = ops::close(handles, h);
        }
    });
    rx.write_watch().post();
    SyscallError::from(faulted).to_u64()
}

/// Make an inbox and tell the caller where it is; the inbox owns its page and only this mapping may name it. `out` is the window the dispatch took for the answer.
pub(super) fn sys_inbox_setup(depth: u32, out: &mut crate::user_ptr::UserBytesMut) -> u64 {
    let (inbox, vaddr) = match crate::inbox::create(depth) {
        Ok(v) => v,
        Err(e) => return e.to_u64(),
    };
    // A refused install drops the reference, which tears the inbox down again.
    let object = match crate::object::inbox::InboxObject::new(inbox) {
        Ok(object) => KObjectRef::Inbox(object),
        Err(e) => return SyscallError::from(e).to_u64(),
    };
    let installed = process::with_handles(|handles| ops::install(handles, object));
    let handle = match installed {
        Ok(h) => h,
        Err(e) => return e.to_u64(),
    };
    // A caller that cannot be told its handle does not keep it.
    if let Err(faulted) = out.write_at(0, toyos_abi::usersafe::bytes(&InboxSetup { handle, _pad: 0, vaddr })) {
        let _ = process::with_handles(|handles| ops::close(handles, handle));
        return SyscallError::from(faulted).to_u64();
    }
    0
}

pub(super) fn sys_inbox_submit(
    inbox_h: RawHandle,
    to_submit: u32,
    min_complete: u32,
    timeout_nanos: u64,
) -> u64 {
    // NotFound and PermissionDenied are the table's own words for gone/wrong-type, not collapsed into InvalidArgument.
    let object = process::with_handles(|handles| {
        handles.get::<crate::object::inbox::InboxObject>(
            inbox_h,
            Rights::READ.union(Rights::WRITE),
        )
    });
    let object = match object {
        Ok(object) => object,
        Err(e) => return e.refuse(),
    };
    // Held across the park: the ring outlives this call even if its last handle closes meanwhile.
    let Some(inbox) = object.inbox() else {
        return SyscallError::NotFound.to_u64();
    };
    match crate::inbox::submit(&inbox, to_submit, min_complete, timeout_nanos) {
        Ok(n) => n as u64,
        Err(e) => e.to_u64(),
    }
}
