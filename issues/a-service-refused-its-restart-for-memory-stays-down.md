---
status: open
kind: defect
opened: 2026-10-23
---

# A service refused its restart for want of memory stays down

When a service ends, the supervisor starts it again once
(`system/supervisor/src/main.rs`, `AfterEnd::Restart`). If that spawn is
refused, it closes the service's ports and does not try again. Since memory is
refused at the call rather than panicking the kernel, out of memory is such a
refusal, and it is temporary.

**Evidence.** In #1045 round 1's `dirs-uncharged` control, `kernel_objects_refused`
held every frame a process may in a shared boot, and the file server's data
instance died of an allocation it was refused. Its restart was refused too, and
it never came back after the test gave its memory back:

`supervisor: fileserver data ended and would not start again (out of memory); its ports are closed`

Every later member that needed `/data` would have failed for a cause outside it.

**Exit:** a restart refused `ResourceExhausted` is retried once memory comes back,
on an event rather than a timer, such as a memory-returned notice the kernel
posts. The retry is reported, and its stall is visible. The guest test is
`kernel_objects_refused`'s own boot: a service it starves while it holds every
frame is serving again once it gives the memory back.

Part of `issues/no-physical-memory-fairness.md`.
