---
status: open
kind: defect
opened: 2026-10-12
---

# The socket2 fork's `From<Socket> for OwnedFd` holds a table key, not a handle

Read from the fork at the commit the lock names and the `toyos` branch's tip,
`1d90250`, and not run. socket2's ToyOS backend (`src/sys/toyos.rs`) names a
socket by a key into a table of its own, counted up from 1000 (`NEXT_FD`,
`sockets`, `alloc_fd`, lines 320-329), and keeps the socket's kernel handle
inside the entry (`SocketState`, 331-364). Its `From<crate::Socket> for
OwnedFd` (1237-1241) hands `OwnedFd::from_raw_fd` that key, taken by
`socket_into_raw` (435-439), which forgets the socket. So:

- The `OwnedFd` holds a number the kernel never gave the process. `RawHandle`
  (`sdk/abi/src/handle.rs`) reads a key K as slot K mod 4096 at generation
  K / 4096: the first socket's key, 1000, as slot 1000 at generation 0.
- What the socket held is never let go: its entry stays in the table, so its
  `Drop` (408-421), which sends `tcp_close` and closes its connection's or
  listener's handle, never runs while the process lives.
- The `OwnedFd`'s drop closes the key as a handle (`OwnedFd`'s `Drop`,
  `rust/library/std/src/os/fd/owned.rs:241-244` at the gitlink's `c1b8813`).
  The kernel looks it up in the process's table (`sys_close`,
  `kernel/src/syscall/handles.rs`; `HandleTable::remove`,
  `kernel/src/object/handle.rs`). A slot past the table, empty, or at another
  generation is `BadHandle` or `Stale`, and `process::handle_fault` ends the
  process with a handle fault. A process holding a live handle at that slot and
  generation loses it silently, whatever object it names, and the next use of
  that handle by its holder is `Stale` and ends the process there.

The other fd impls cross the same two numbers. `AsFd`, `AsRawFd` (1225-1235)
and `IntoRawFd` (1243-1247) hand out the key as a descriptor. `From<OwnedFd>`
and `FromRawFd` (1249-1259) take a kernel handle as a key: it names no entry,
so every call on the socket that looks the number up is refused, "invalid
socket fd" or, from `poll_connect` (538-545), `NotConnected`, and its drop
closes nothing and leaks the handle; or, where the handle's number is a live
key, it names another socket's entry, which its drop then closes. An unconnected socket
(`SocketState::Unconnected`) holds no kernel object at all, so it has no handle
to give.

Nothing calls the conversion yet. On ToyOS socket2 is built only for the tokio
fork (`902fb97`). There `TcpSocket::connect` and `listen` drop their socket2
socket instead of converting it (`tokio/src/net/tcp/socket.rs:838-843` and
`913-923`), and its ToyOS `IntoRawFd for TcpSocket` (1081-1086) passes the key
to whoever calls it. Nothing in the tree, `system/sshserver` included, or in
the russh fork (`389804d`) calls one.

**Exit**: on ToyOS, `From<socket2::Socket> for OwnedFd` and each other impl at
1225-1259 either names a kernel handle the process holds for that socket, or
does not compile, and the lock names the fork commit that does it. A root
`[patch.crates-io]` that no longer names `ToyOSOrg/socket2` (stage 4 of
`issues/the-ssh-server-is-toyos-own-and-russh-is-gone.md`) also meets this,
because upstream socket2 has no ToyOS backend.

**Owner**: whoever holds `issues/toyos-has-its-own-network-stack.md`; the
change is a commit on the socket2 fork's `toyos` branch.
