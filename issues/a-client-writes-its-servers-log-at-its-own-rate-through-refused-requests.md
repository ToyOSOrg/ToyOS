---
status: open
kind: defect
opened: 2026-11-11
---

# A client writes its server's log at its own rate through refused requests

`toyos::admission` (`sdk/toyos/src/admission.rs`) has its server say at most
one line a holder a limit, and a table at most one more than its slots, of
what befalls a connection before its first request: a refusal, a frame no
protocol describes, a silence. What a server
says of a request it then refuses has no such bound. The supervisor says a
line for each launch it refuses (`serve_launch`,
`system/supervisor/src/main.rs`: a working directory that is not absolute, a
launch that names no parent, a descriptor no launch carries, a program no row
declares), the file server for each hello it lets go (`hello`,
`system/fileserver/src/main.rs`), and diskserver for each driver whose first
frame is not its name (`named`, `system/diskserver/src/main.rs`).

So a client that repeats a refused request writes a line into its server's log
on every one. logkeeper counts a program's records past its allowance in a
second rather than writing them (`system/logkeeper/src/origin.rs`), so the
server's own lines in that second are counted, not written.

Seen by reading at `4d1fad663`; not measured.

**Owner**: the orchestrator, which dispatches the fix.

**Exit condition**: every line a server says of a client's request, refused or
let go, is said at most once a holder in a bound period, with a count of the
rest, and a host test of each server's bound goes red when one holder's
repeated requests are each said.
