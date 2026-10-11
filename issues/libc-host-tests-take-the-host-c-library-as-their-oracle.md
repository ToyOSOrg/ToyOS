---
status: open
kind: defect
opened: 2026-10-10
---

# libc host tests take the host C library as their oracle

`reviewer.md`'s **Tests**: another OS's output, recorded or live, is an oracle only when it is
committed with its provenance and read hermetically. These `sdk/libc/arch` tests call the
host's C library at run time and hold libc's answer to it, so the oracle is whichever C library
the host has, and the verdict differs between a Linux host and a macOS host:

- `strtonum_differential.rs`: `strtod`, `strtof`, `strtol`, `strtoul` and their `errno`.
- `internet_addresses.rs`: `inet_pton`, `inet_addr`, `inet_ntop`, `htonl`, `htons` and
  `getsockname`, with `darwin_reads` for where macOS parts from Linux.
- `socket_options.rs`: `setsockopt` and `getsockopt` on real host sockets, with `darwin` for where
  macOS parts from Linux.
- `fparts_differential.rs`: `modf` and `logb`.
- `text_differential.rs`: `strnlen` (its `strsignal` half is held to a committed glibc table and
  conforms).

`long_double.rs` is not one: its oracle is compiler-builtins' `__extenddftf2`, pinned by the
lockfile.

Owner: the orchestrator, which dispatches the reshape.

**Exit condition**: each test above holds libc to a committed table recorded from a named C
library with its provenance, or to the specification's text, and calls no host C library function
as its oracle; the `darwin` exception tables go with it.
