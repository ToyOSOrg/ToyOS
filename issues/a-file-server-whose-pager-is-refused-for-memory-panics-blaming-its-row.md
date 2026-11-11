---
status: open
kind: defect
opened: 2026-11-11
---

# A file server whose pager is refused for memory panics, blaming its row

`pager` (`system/fileserver/src/main.rs`) panics whenever
`toyos::paged::Pager::create` is refused, saying "its row is no pager row".
Under memory exhaustion the refusal is `ResourceExhausted`. That is an
expected condition, not a manifest error, so the message is false, and the
server ends instead of reporting it.

Seen in guest run 43092179151 (#1252 at 041f1b81e), `kernel_objects_refused_alone`.
While the test held every frame, the Log file server (pid 9) ended. Each of
the supervisor's next three restarts went the same way:

- it said `fileserver: diskserver would not open the partition: Kernel(ResourceExhausted)`;
- it said `fileserver: Log serving /log — absent`;
- it panicked at `main.rs:290` with
  `fileserver: the Log server makes no pager (ResourceExhausted); its row is no pager row`.

The supervisor then closed the role for the rest of the boot:
`supervisor: fileserver log ended 4 times in 10 s; its ports are closed, its
clients answered Gone, and a program started from now on is given none of
its directories`. Main has the same code (e10b6fd49).

Owner: the file server, with the supervisor's restart budget.

Exit: a refused pager is no panic. The file server reports what the kernel
answered, and a role refused its pager for memory ends with a status that the
supervisor reads as want of memory, as it reads a start refused for memory.
A host test of the refusal holds this.
