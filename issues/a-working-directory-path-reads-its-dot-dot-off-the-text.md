---
status: open
kind: defect
opened: 2026-10-23
---

# A working-directory path reads its `..` off the text

`toyos::files` makes every absolute path, and every path from the working
directory, absolute and drops its `..` by text before it picks the server
(`sdk/toyos/src/files.rs`, `absolute`; `files/owner.rs`, `place`). So
`/home/link/../x`, where `link` is a symlink to `a/b` on `/home`, opens
`/home/x`, where POSIX resolves `..` from the directory `link` led to and
opens `/home/a/x`.

A path from a directory descriptor does not do this. Its `..` goes to the file
server, which resolves it from the directory itself
(`system/fileserver/src/resolve.rs`). So within one process, `openat(fd, "link/../x")`
and `open("/home/link/../x")` open two different files, and libc's `realpath`,
which resolves a served name POSIX's way (`sdk/libc/src/resolve.rs`), names the
first.

The text rule has one real job: a `..` that climbs out of a capability's
directory (`/home/x/../../system`) leads to another server, so the client
must settle that climb in its own table before it routes the path.

**Owner**: the SDK's file client, `sdk/toyos/src/files.rs`.

**Exit**: `toyos::files` hands each component after a path's capability
prefix, `..` included, to that capability's server. It resolves a `..` that
climbs above the capability's directory against the process's own table. A
guest case opens `/home/link/../x` with `link` → `a/b`, both through
`AT_FDCWD` and from a descriptor, and gets `/home/a/x` both times.
