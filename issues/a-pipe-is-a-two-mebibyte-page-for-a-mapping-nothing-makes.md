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

Owner: the orchestrator, who stages
`issues/netstack-moves-every-byte-through-two-kernel-pipes.md`, whose streams
ring their doorbells through pipes and pay a page for each until this closes.

Exit: `rg -n -i 'pipe_?map|publish_ends|RING_(WRITER|READER)_CLOSED' kernel
sdk tests` is empty, and a pipe's ring is no `pmm::alloc_page` page:
`rg -n 'alloc_page|PAGE_2M' kernel/src/pipe.rs` is empty.
