---
status: open
kind: defect
opened: 2026-10-11
---

# A pipe is a 2 MiB page for a mapping nothing makes

A pipe's ring is one `pmm::alloc_page` page, `PIPE_SIZE = PAGE_2M`
(`kernel/src/pipe.rs`), allocated and zeroed on its first write, because
`SYS_PIPE_MAP` maps that page into a process and a 2 MiB page is the only one
this kernel maps. Nothing outside the guest tests calls `SYS_PIPE_MAP`: its
last caller, netd, mapped its clients' rings only to read the closed flags,
and stopped in a23734684 when they were found forgeable. Since then the map
costs every pipe and every IPC connection, which is two pipes, 2 MiB a
direction once a byte moves, whatever the traffic: a wake bell holds 2 MiB to
carry one byte, and so does each direction of a connection that carries one
request. It also keeps `Pipe::publish_ends` writing reader and writer flags
into the page for a reader that no longer exists, and the ring's header
contract over a page a process may write (`sdk/abi/src/ring.rs`).

Exit: `SYS_PIPE_MAP`, `publish_ends`, the ring's closed flags and the guest
tests that exercise only the map are gone, a pipe's ring is sized for a byte
stream rather than for a mapping, and a guest test that opens a connection,
sends one request and reads `memory_total` less free memory finds no 2 MiB
moved.
