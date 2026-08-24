//! What one process reaches another through: pipes, ports and the namespaces
//! that name them, connections, shared memory, and inboxes.
//!
//! **Nothing here is addressed by name or by pid.** A port is created and its
//! two ends installed in the creator's own table; a namespace is built out of
//! connectors the caller already holds; a connection is opened by presenting one
//! of those connectors. There is no registry to look a peer up in, which is what
//! lets [`sys_connection_join`] be as simple as it is where its id-addressed
//! predecessor needed a rule about who was entitled to a number.
//!
//! **Every all-or-nothing claim here is structural.** [`sys_namespace_build`]
//! resolves every name and checks every connector before it installs anything,
//! [`sys_handle_send`] verifies the whole batch before it removes one entry, and
//! [`sys_handle_recv`] measures the batch before it takes it — so a refusal
//! leaves the caller's table exactly as it was, and `Gone` and
//! `ResourceExhausted` stay honest answers about the peer.

use alloc::vec::Vec;

use crate::completion;
use crate::mm::paging::Prot;
use crate::object::{ops, port, KObjectRef};
use crate::time::Deadline;
use crate::user_ptr::SyscallContext;
use crate::UserAddr;
use crate::{pipe, process};

use toyos_abi::handle::{RawHandle, Rights};
use toyos_abi::syscall::*;
use toyos_sched::task::WaitClass;

use super::{cancelled, HANDLE_LEN};
use super::handles::handle_result;

pub(super) fn sys_pipe() -> u64 {
    let (reader, writer) = pipe::create();
    let read_end = KObjectRef::PipeRead(crate::object::pipe::PipeReadEnd::new(reader));
    let write_end = KObjectRef::PipeWrite(crate::object::pipe::PipeWriteEnd::new(writer));
    process::with_process_data(|data| {
        let Ok(read_h) = ops::install(&mut data.handles, read_end) else {
            return SyscallError::ResourceExhausted.to_u64();
        };
        let Ok(write_h) = ops::install(&mut data.handles, write_end) else {
            ops::close(&mut data.handles, read_h, &mut data.pipe_maps)
                .expect("the read end this call installed a moment ago");
            return SyscallError::ResourceExhausted.to_u64();
        };
        ((read_h.0 as u64) << 32) | write_h.0 as u64
    })
}

/// Map a pipe's ring page into the caller.
///
/// The window is recorded against the pipe (`process::PipeMap`) so that
/// closing the last descriptor for it takes the mapping away. It used to be
/// recorded nowhere: `SYS_PIPE`, `SYS_PIPE_MAP`, close both ends freed the ring
/// page back to the PMM with the caller's writable mapping of it still live,
/// and whatever the PMM handed that page to next — another process's pipe, a
/// kernel heap region, a DMA buffer — was readable and writable by a process
/// that owned nothing.
///
/// A second call for the same pipe returns the window the first one made,
/// rather than a second window onto the same page. That is what keeps
/// `pipe_maps` bounded by the descriptor table.
pub(super) fn sys_pipe_map(h: RawHandle) -> u64 {
    let mapped = process::with_process_data(|data| {
        let pipe_id = match data.handles.get_ref(h, Rights::MAP) {
            Ok(object) => ops::pipe_id_read(object).or_else(|| ops::pipe_id_write(object)),
            Err(e) => return Err(e),
        };
        let Some(pipe_id) = pipe_id else {
            return Ok(SyscallError::InvalidArgument.to_u64());
        };
        if let Some(existing) = data.pipe_maps.iter().find(|m| m.pipe == pipe_id) {
            return Ok(existing.addr.raw());
        }
        let Some(phys) = pipe::map_page(pipe_id) else {
            return Ok(SyscallError::ResourceExhausted.to_u64());
        };
        let pt = crate::scheduler::current_address_space()
            .expect("sys_pipe_map: no address space");
        let Some((vaddr, _aligned)) = process::vma_map(&pt, phys.phys(), pipe::PIPE_SIZE as u64, Prot::ReadWrite) else {
            return Ok(SyscallError::ResourceExhausted.to_u64());
        };
        data.pipe_maps.push(process::PipeMap { pipe: pipe_id, addr: vaddr });

        Ok(vaddr.raw())
    });
    match mapped {
        Ok(word) => word,
        Err(e) => e.refuse(),
    }
}

/// Join a pipe read end and a pipe write end into one duplex connection.
///
/// The caller must already hold both, in the right direction, and keeps them:
/// this takes references of its own. It grants nothing — everything it can
/// reach is something the caller could already read or write — which is what
/// lets it be this simple where its id-addressed predecessor needed a rule
/// about who was entitled to a number.
///
/// `std`'s `TcpStream` is one handle and netd's data path is two pipes, and
/// that is the whole of why this exists.
pub(super) fn sys_connection_join(rx_h: RawHandle, tx_h: RawHandle) -> u64 {
    let ends = process::with_process_data(|data| {
        let rx = data.handles.get::<crate::object::pipe::PipeReadEnd>(rx_h, Rights::READ)?;
        let tx = data.handles.get::<crate::object::pipe::PipeWriteEnd>(tx_h, Rights::WRITE)?;
        Ok::<_, crate::object::HandleError>((rx.reference(), tx.reference()))
    });
    let (rx, tx) = match ends {
        Ok(ends) => ends,
        Err(e) => return e.refuse(),
    };
    let object = KObjectRef::Connection(crate::object::service::ConnectionEnd::joined(rx, tx));
    process::with_process_data(|data| handle_result(ops::install(&mut data.handles, object)))
}

/// Make a port and install both ends.
///
/// Needs no right and grants none: a port with no clients is not authority.
/// The two handles come back packed, which cannot be read as an error — see
/// [`SYS_PORT_CREATE`].
pub(super) fn sys_port_create() -> u64 {
    let (acceptor, connector) = port::create();
    process::with_process_data(|data| {
        let Ok(a) = ops::install(&mut data.handles, KObjectRef::Acceptor(acceptor)) else {
            return SyscallError::ResourceExhausted.to_u64();
        };
        let install_c =
            ops::install(&mut data.handles, KObjectRef::Connector(connector));
        let Ok(c) = install_c else {
            // The acceptor goes back, so a refused pair leaves no port half in
            // a table with nothing on the other side of it.
            drop(data.handles.remove(a));
            return SyscallError::ResourceExhausted.to_u64();
        };
        ((a.0 as u64) << 32) | c.0 as u64
    })
}

/// A namespace built from a base's kept names plus new bindings.
///
/// Every name is resolved against the base *before* anything is installed, and
/// every added connector is checked for `TRANSFER` first, so a refusal leaves
/// the caller's table exactly as it was.
pub(super) fn sys_namespace_build(ctx: &SyscallContext, args: &NamespaceBuild) -> u64 {
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

    if args.keep_n > 0 {
        let Some(keep) = (args.keep_n as usize)
            .checked_mul(core::mem::size_of::<NameRef>())
            .and_then(|len| ctx.user_bytes(UserAddr::new(args.keep_ptr), len as u64))
        else {
            return SyscallError::BadAddress.to_u64();
        };
        let base = match process::with_process_data(|data| {
            data.handles.get::<crate::object::namespace::Namespace>(args.base, Rights::READ)
        }) {
            Ok(base) => base,
            Err(e) => return e.refuse(),
        };
        for i in 0..args.keep_n as usize {
            let mut raw = [0u8; 8];
            keep.read_at(i * 8, &mut raw);
            let off = u32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]);
            let len = u32::from_ne_bytes([raw[4], raw[5], raw[6], raw[7]]);
            let Some(name) = name_at(off, len) else {
                return SyscallError::InvalidArgument.to_u64();
            };
            // A name the base does not carry is silently absent from the
            // child's: a parent narrowing a namespace is asking for an
            // intersection, and asking for a name it does not itself hold
            // grants nothing either way.
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
            add.read_at(i * 16, &mut raw);
            let off = u32::from_ne_bytes([raw[0], raw[1], raw[2], raw[3]]);
            let len = u32::from_ne_bytes([raw[4], raw[5], raw[6], raw[7]]);
            let handle = RawHandle(u32::from_ne_bytes([raw[8], raw[9], raw[10], raw[11]]));
            let Some(name) = name_at(off, len) else {
                return SyscallError::InvalidArgument.to_u64();
            };
            let connector = match process::with_process_data(|data| {
                data.handles.get::<port::Connector>(handle, Rights::TRANSFER)
            }) {
                Ok(c) => c,
                // **The one place a wrong type is not provably the caller's
                // bug.** An added connector is routinely one a *peer*
                // transferred — that is what a `provides` name is, and
                // `/bin/init`'s launcher builds a namespace out of handles a
                // client sent it. Faulting here let any process holding the
                // `launcher` connector end init by sending it a pipe.
                Err(crate::object::HandleError::WrongType { .. }) => {
                    return SyscallError::InvalidArgument.to_u64()
                }
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
    };
    process::with_process_data(|data| {
        handle_result(ops::install(&mut data.handles, KObjectRef::Namespace(namespace)))
    })
}

/// Open a connection to `name` in the namespace `ns_h` holds.
///
/// **Two facts, two words.** A name this namespace does not carry is
/// `NotFound` — a statement about this process. A name whose port has closed is
/// `Gone` — a statement about the machine. Only the kernel can tell them apart,
/// so only the kernel may collapse them, and it does not.
pub(super) fn sys_namespace_open(ns_h: RawHandle, name: &str) -> u64 {
    let connector = match process::with_process_data(|data| {
        let ns = data
            .handles
            .get::<crate::object::namespace::Namespace>(ns_h, Rights::READ)?;
        Ok::<_, crate::object::HandleError>(ns.lookup(name).cloned())
    }) {
        Ok(Some(c)) => c,
        Ok(None) => return SyscallError::NotFound.to_u64(),
        Err(e) => return e.refuse(),
    };
    connect_through(&connector)
}

/// The client half of a connection, and the server half queued on the port.
fn connect_through(connector: &port::Connector) -> u64 {
    if connector.closed() {
        return SyscallError::Gone.to_u64();
    }
    let (cs_reader, cs_writer) = pipe::create(); // client → server
    let (sc_reader, sc_writer) = pipe::create(); // server → client
    // Cross-wired here and nowhere else: what the client sends is what the
    // server receives, and the server's end is built out of the same two
    // queues when it accepts.
    let (to_server, to_client) = crate::object::service::ConnectionEnd::pair_queues();

    // The client's own end first. Installing it can fail on a full handle
    // table, and a connection queued for a server whose client never got a
    // handle is one the server accepts and finds already dead.
    let object = KObjectRef::Connection(crate::object::service::ConnectionEnd::new(
        sc_reader,          // client reads from server→client
        cs_writer,          // client writes to client→server
        to_client.clone(),  // and receives what the server sent
        to_server.clone(),
    ));
    let h = match process::with_process_data(|data| ops::install(&mut data.handles, object)) {
        Ok(h) => h,
        Err(e) => return e.to_u64(),
    };

    let queued = connector.push(port::PendingConnection {
        rx: cs_reader, // server reads from client→server
        tx: sc_writer, // server writes to server→client
        inbox: to_server,
        outbox: to_client,
    });
    if let Err(e) = queued {
        process::with_process_data(|data| {
            ops::close(&mut data.handles, h, &mut data.pipe_maps)
                .expect("the connection this call installed a moment ago");
        });
        return match e {
            port::PushError::Closed => SyscallError::Gone.to_u64(),
            port::PushError::QueueFull => SyscallError::ResourceExhausted.to_u64(),
        };
    }
    let port = connector.port();
    completion::post(
        completion::Subject::of(port.watch()),
        completion::Outcome::Ready,
    );
    let watchers = port.watchers();
    if !watchers.is_empty() {
        crate::inbox::complete_pending_for_event(
            &watchers,
            crate::inbox::Source::Port(port),
        );
    }
    h.0 as u64
}

pub(super) fn sys_accept(h: RawHandle) -> u64 {
    let acceptor = match process::with_process_data(|data| {
        data.handles.get::<port::Acceptor>(h, Rights::READ)
    }) {
        Ok(a) => a,
        Err(e) => return e.refuse(),
    };

    loop {
        if let Some(conn) = acceptor.pop() {
            // PipeReader/PipeWriter move from the queue into the connection. No
            // refcount change — ownership transfers.
            let object = KObjectRef::Connection(
                crate::object::service::ConnectionEnd::new(
                    conn.rx,
                    conn.tx,
                    conn.inbox,
                    conn.outbox,
                ),
            );
            let installed = process::with_process_data(|data| {
                ops::install(&mut data.handles, object)
            });
            return handle_result(installed);
        }
        // The last handle to this acceptor has gone — another thread of this
        // process closed it — so nothing will ever be queued again and the
        // condition below has become permanently false. Answering is the only
        // alternative to parking forever.
        if acceptor.closed() {
            return SyscallError::Gone.to_u64();
        }
        let parkable = crate::scheduler::Parkable::at_entry();
        if completion::wait_until(
            &parkable,
            completion::Subject::of(acceptor.watch()),
            completion::Token::new(0),
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

/// Make a shared region and hand back the one handle to it.
///
/// The creator's handle carries `MAP`, `DUP` and `TRANSFER`: mapping is what a
/// region is for, and giving one away is the whole point of the object. There
/// is no grant list — being able to name it *is* being allowed to map it.
pub(super) fn sys_shm_create(size: u64) -> u64 {
    let object = match crate::object::shm::SharedMemObject::create(size) {
        Ok(shm) => KObjectRef::SharedMem(shm),
        Err(e) => return e.to_u64(),
    };
    process::with_process_data(|data| handle_result(ops::install(&mut data.handles, object)))
}

pub(super) fn sys_shm_map(h: RawHandle) -> u64 {
    let shm = match process::with_process_data(|data| {
        data.handles.get::<crate::object::shm::SharedMemObject>(h, Rights::MAP)
    }) {
        Ok(shm) => shm,
        Err(e) => return e.refuse(),
    };
    let pt = process::current_address_space();
    match shm.map_into(process::current_process(), &pt) {
        Ok(vaddr) => vaddr,
        Err(e) => e.to_u64(),
    }
}

/// Move handles to the peer of a connection, all or nothing.
///
/// Every handle is verified — it resolves, it carries `TRANSFER`, it is named
/// once, and it is not the connection itself — before any of them is removed,
/// and a peer's queue that refuses the batch afterwards hands it back. **So
/// every refusal leaves the caller's table exactly as it was**, which is what
/// makes `Gone` and `ResourceExhausted` honest: they are answers about the
/// peer, and a caller retrying or closing what it still holds is right rather
/// than fatal. Refusing to send the connection over itself is what keeps a
/// cross-pair reference cycle to two objects rather than one.
///
/// **Rights travel unchanged, `TRANSFER` included.** A move requires it and
/// carries it, so everything that can be moved can be moved on: the
/// non-transitive grant the pid ACL had is not expressible, and making it so
/// is a rights word on *both* move paths rather than on this one
/// (`issues/isolation/a-moved-handle-is-always-re-movable.md`).
pub(super) fn sys_handle_send(conn_h: RawHandle, handles: &crate::user_ptr::UserBytes, count: usize) -> u64 {
    let mut wanted = [RawHandle(0); MAX_TRANSFER_HANDLES];
    for (i, slot) in wanted.iter_mut().enumerate().take(count) {
        let mut raw = [0u8; HANDLE_LEN];
        handles.read_at(i * HANDLE_LEN, &mut raw);
        *slot = RawHandle(u32::from_ne_bytes(raw));
    }
    let wanted = &wanted[..count];

    let sent = process::with_process_data(|data| {
        let conn = data
            .handles
            .get::<crate::object::service::ConnectionEnd>(conn_h, Rights::TRANSFER)?;
        for (i, h) in wanted.iter().enumerate() {
            if *h == conn_h || wanted[..i].contains(h) {
                return Err(SyscallError::InvalidArgument.into());
            }
            let rights = data.handles.rights_of(*h)?;
            if !rights.contains(Rights::TRANSFER) {
                return Err(SyscallError::PermissionDenied.into());
            }
        }
        // The peer's queue can still refuse, and both of its refusals are ones
        // a caller reads as "the handles did not go" — so they must not have
        // gone. `transfer` puts every entry back at its own number, under this
        // same hold, where nothing can observe the gap.
        data.handles
            .transfer(wanted, |batch| conn.send(batch))
            .map_err(crate::object::Refusal::Error)
    });
    match sent {
        Ok(()) => 0,
        Err(e) => e.refuse(),
    }
}

/// Take the oldest batch the peer sent. Never blocks; zero means none queued.
///
/// The whole thing runs under one hold of this process's table, so the batch
/// whose size was checked is the batch that is installed — the peer can only
/// add to the far end of the queue, and a sibling thread of this process is
/// serialised by the same lock.
///
/// **Every refusal is answered outside that hold**, per [`HandleError::refuse`]:
/// three of the five kinds end the caller, and ending it takes the same
/// non-reentrant lock this closure is running under.
///
/// [`HandleError::refuse`]: crate::object::HandleError::refuse
pub(super) fn sys_handle_recv(
    conn_h: RawHandle,
    out: &mut crate::user_ptr::UserBytesMut,
    cap: usize,
) -> u64 {
    let taken = process::with_process_data(|data| {
        let conn = data
            .handles
            .get::<crate::object::service::ConnectionEnd>(conn_h, Rights::READ)?;
        // **Measured before it is taken, and both refusals leave it queued.**
        // A batch popped and then dropped is capabilities nobody can ask for
        // again, reported as an error a caller reads as "they did not arrive".
        // Only the peer pushes, and only to the far end, so the front this saw
        // is the front the pop takes — or the queue closed under it, which is
        // the same answer as an empty one.
        let Some(width) = conn.peek_width() else { return Ok(0) };
        if width > cap {
            return Err(SyscallError::InvalidArgument.into());
        }
        if !data.handles.has_room(width) {
            return Err(SyscallError::ResourceExhausted.into());
        }
        let Some(batch) = conn.recv_bounded(cap)? else { return Ok(0) };
        let count = batch.len();
        for (i, entry) in batch.into_iter().enumerate() {
            let h = data.handles.install(entry).expect("room was asked for first");
            out.write_at(i * HANDLE_LEN, &h.0.to_ne_bytes());
        }
        Ok::<_, crate::object::Refusal>(count as u64)
    });
    match taken {
        Ok(n) => n,
        Err(e) => e.refuse(),
    }
}

/// Make an inbox and tell the caller where it is.
///
/// The inbox owns its page and this maps it. An inbox is not something two
/// processes share, so nothing else may name that page.
pub(super) fn sys_inbox_setup(ctx: &SyscallContext, depth: u32, out: u64) -> u64 {
    let out = match UserAddr::checked(out) {
        Some(addr) => addr,
        None => return SyscallError::InvalidArgument.to_u64(),
    };
    let (inbox, vaddr) = match crate::inbox::create(depth) {
        Ok(v) => v,
        Err(e) => return e.to_u64(),
    };
    // A refused install drops the reference, which tears the inbox down again.
    let object = KObjectRef::Inbox(crate::object::inbox::InboxObject::new(inbox));
    let installed = process::with_process_data(|data| ops::install(&mut data.handles, object));
    let handle = match installed {
        Ok(h) => h,
        Err(e) => return e.to_u64(),
    };
    let answer = toyos_abi::syscall::InboxSetup { handle, _pad: 0, vaddr };
    match ctx.copy_out(out, &answer) {
        Ok(()) => 0,
        Err(e) => {
            process::with_process_data(|data| {
                ops::close(&mut data.handles, handle, &mut data.pipe_maps)
                    .expect("the inbox this call installed a moment ago");
            });
            e.to_u64()
        }
    }
}

pub(super) fn sys_inbox_submit(
    inbox_h: RawHandle,
    to_submit: u32,
    min_complete: u32,
    timeout_nanos: u64,
) -> u64 {
    // The table's own words, not one invented here: a handle that is gone is
    // `NotFound` and one of the wrong type is `PermissionDenied`, the same as
    // every other call. Collapsing both into `InvalidArgument` made "this
    // inbox was closed" indistinguishable from "this argument is nonsense".
    let inbox_id = process::with_process_data(|data| {
        data.handles
            .get::<crate::object::inbox::InboxObject>(
                inbox_h,
                Rights::READ.union(Rights::WRITE),
            )
            .map(|r| r.id())
    });
    let inbox_id = match inbox_id {
        Ok(id) => id,
        Err(e) => return e.refuse(),
    };
    match crate::inbox::submit(inbox_id, to_submit, min_complete, timeout_nanos) {
        Ok(n) => n as u64,
        Err(e) => e.to_u64(),
    }
}
