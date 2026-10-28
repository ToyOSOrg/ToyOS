---
status: open
kind: defect
opened: 2026-10-28
---

# Two holders of one socket queue can wait for good

`netstack_holders_exported_to_children` (`tests/toyos.rs`), on QEMU x86-64's
`tests/images/netcase.toml`, sometimes stops in `test_rs_socket_export`'s
race. The job has said its line for the export ("a listener and a datagram
socket exported to a child: it accepted and spoke on them, and so did the
parent after it") and spawned the racer, so the parent and the racer are
blocked on one listener's queue and one datagram socket's. The guest then
says nothing until the harness's ceiling ends it, 690 s to 915 s later. No
kernel `exit:` line follows for either process, so this is lost progress,
not a slow guest.

The soak ran the seven-test batch
`kernel_objects_refused_alone memory_floor_holds_the_controls stream_holders
netstack_holders_exported_to_children session_lifecycle libc_children
udp_receive_times_out` repeatedly. Three detached worktrees ran it at once,
under the same host load, on 2026-10-28:

| revision | runs | this stall |
|---|---|---|
| `544f9cc3c` | 16 | 1 |
| `576bebf3a` (main) | 22 | 2 |
| `5e0cb52b0` (#1074) | 21 | 4 |

It stalls at every revision, so it predates #1074. These counts do not
separate the rates.

The two holders are woken by netstack ringing each holder's bell, a pipe
write it does not wait on. Where a wake is lost is not known.

Owner: the orchestrator, who assigns it.

Exit: the race's lost wake is found and fixed, shown by a host or loom
model of two holders on one queue. The same soak then runs at least 40
batches without this stall.
