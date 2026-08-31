//! What one process reaches another through: pipes, ports and the namespaces
//! that name them, connections, shared memory, and inboxes.
//! No peer is addressed by name or pid: ports, namespaces and connections are
//! built only from connectors and handles the caller already holds.
//! Every all-or-nothing claim here is structural: names and connectors are
//! resolved and checked before anything is installed or removed.

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

/// Map a pipe's ring page into the caller, tracked against the pipe so
/// closing its last descriptor unmaps it.
pub(super) fn sys_pipe_map(h: RawHandle) -> u64 {
    let mapped = process::with_process_data(|data| {
        let pipe_id = match data.handles.get_ref(h, Rights::MAP) {
            Ok(object) => ops::pipe_id_read(object).or_else(|| ops::pipe_id_write(object)),
            Err(e) => return Err(e),
        };
        let Some(pipe_id) = pipe_id else {
            return Ok(SyscallError::InvalidArgument.to_u64());
        };
        // A second call returns the existing window, keeping pipe_maps bounded by the handle table.
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

/// Join a pipe read end and a pipe write end into one duplex connection; the caller keeps both handles and gains no more than it already held.
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

/// Make a port and install both ends; needs no right and grants none, since a port with no clients is not authority. The two handles come back packed into one word, which cannot be read as an error.
pub(super) fn sys_port_create() -> u64 {
    let (acceptor, connector) = port::create();
    process::with_process_data(|data| {
        let Ok(a) = ops::install(&mut data.handles, KObjectRef::Acceptor(acceptor)) else {
            return SyscallError::ResourceExhausted.to_u64();
        };
        let install_c =
            ops::install(&mut data.handles, KObjectRef::Connector(connector));
        let Ok(c) = install_c else {
            // The acceptor goes back so a refused pair leaves no orphaned port half.
            drop(data.handles.remove(a));
            return SyscallError::ResourceExhausted.to_u64();
        };
        ((a.0 as u64) << 32) | c.0 as u64
    })
}

/// Build a namespace from a base's kept names plus new bindings; a refusal leaves the caller's table unchanged (every name resolved and connector checked first).
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
                // WrongType returns InvalidArgument here instead of ending the caller: an added connector is often one a peer transferred, not proof of a caller bug.
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

/// Open a connection to `name` in the namespace `ns_h` holds; `NotFound` means the namespace lacks the name, `Gone` means its port has closed.
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
    // Cross-wired here only: the server's end is built from the same two queues when it accepts.
    let (to_server, to_client) = crate::object::service::ConnectionEnd::pair_queues();

    // Client's end installed first: queuing before that would let a server accept a connection whose client has no handle.
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
    // Both halves: the server's blocked `Acceptor` (the port's own watch) and
    // every ring polling it — `Source::wake` cannot do one without the other.
    crate::inbox::Source::Port(connector.port()).wake();
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
            // PipeReader/PipeWriter move from queue into connection: ownership transfers, no refcount change.
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
        // A closed acceptor will never queue again, so returning here is the only alternative to parking forever.
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

/// Make a shared region and hand back the one handle to it, carrying MAP, DUP and TRANSFER — naming it is what authorizes mapping it.
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

/// Move handles to the peer of a connection, all or nothing; rights, including `TRANSFER`, travel unchanged.
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
        // The peer's queue can still refuse; transfer restores every entry at its own number so a refusal is invisible.
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
pub(super) fn sys_handle_recv(
    conn_h: RawHandle,
    out: &mut crate::user_ptr::UserBytesMut,
    cap: usize,
) -> u64 {
    let taken = process::with_process_data(|data| {
        let conn = data
            .handles
            .get::<crate::object::service::ConnectionEnd>(conn_h, Rights::READ)?;
        // The front measured here is the front recv_bounded takes: only the peer pushes, and only to the far end.
        // Width is measured before the batch is taken: refusing after popping would drop already-taken handles, leaking capabilities nobody could ask for again.
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
    // Refused outside this hold: ending the caller takes the same non-reentrant lock.
    match taken {
        Ok(n) => n,
        Err(e) => e.refuse(),
    }
}

/// Make an inbox and tell the caller where it is; the inbox owns its page and only this mapping may name it.
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
    // NotFound and PermissionDenied are the table's own words for gone/wrong-type, not collapsed into InvalidArgument.
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
