---
status: open
kind: track
opened: 2026-09-30
---

# There is no `/dev/null`

Three things a C build runs open it by path: Ninja as every command's stdin
(v1.13.1 `src/subprocess-posix.cc:107`), LLVM for a redirect to nowhere
(llvmorg-21.1.0 `llvm/lib/Support/Unix/Program.inc:97`, `:127`), and libuv for
each stdio of a child left unset (v1.53.0 `src/unix/process.c:679-685`).
ToyOS has no `/dev`: `/` holds exactly the names `kernel/src/vfs.rs`'s
`ROOT_ENTRIES` lists.

std's `Stdio::null()` is the same gap: it leaves the child's slot empty, and
a child that writes to a null stdout or reads a null stdin names a handle it
does not hold and is ended (`spawn_cost` pipes its child's line for this).
Rust's contract is that such a write is taken and such a read answers end of
file.

Owed: a name that opens to an object every read of which answers end of file
and every write to which is taken and dropped. Where the name lives, in `/` or
in each program's view, is
`issues/every-program-sees-only-the-files-it-was-given.md`'s.

**Exit**: a C program opens `/dev/null` for reading and writing, and a std
child spawned with `Stdio::null()` reads and writes its stdio; a read answers
0, and a write of n bytes answers n.
