---
status: open
kind: defect
opened: 2026-10-02
---

# Comments still name syscalls the ABI deleted

A deleted syscall is simply deleted, and a past implementation lives in a commit
message, never in source. Six comments still tell the history of a syscall
`toyos-abi` no longer declares:

- `userland/libc/src/misc.rs`, `waitpid`'s doc: "`SYS_WAITPID` is retired";
- `kernel/src/pipe.rs`, `Pipe::backing`'s doc: a pending `SYS_CONNECT`;
- `userland/soundd/src/virtio.rs`, `submit`'s doc: "the deleted
  `SYS_AUDIO_SUBMIT`";
- `tests/toyos-rust-tests/src/bin/abuse_pipe_owner.rs`, the module doc:
  `SYS_PIPE_OPEN` and `SYS_SOCKET_CREATE`, "that family is retired";
- `tests/toyos-rust-tests/src/bin/abuse_connect_flood.rs`, the module doc:
  "it used to `SYS_LISTEN`";
- `tests/toyos-rust-tests/src/bin/shm_release_reclaims.rs`, the module doc:
  what `SYS_RELEASE_SHARED` did.

None holds a number or refuses anything, so none is a retirement entry. Each is
deleted down to what is true of its item now.

Owner: the item each comment documents — `waitpid`, `Pipe::backing`,
`Virtio::submit`, and the three test binaries' module docs.

**Exit**: `git grep -w -E
'SYS_WAITPID|SYS_CONNECT|SYS_AUDIO_SUBMIT|SYS_PIPE_OPEN|SYS_SOCKET_CREATE|SYS_LISTEN|SYS_RELEASE_SHARED'
-- kernel userland tests` finds nothing.
