---
status: open
kind: defect
opened: 2026-11-05
---

# A concurrent dlopen can answer a library whose constructors are still running

`toyos::dl::open` publishes a set of libraries while it holds the module
list (`tls::loading`), then lets the list go and runs their constructors, so a
constructor may itself load a library. A second thread that opens the same
path meanwhile finds it published and returns it at once, before the first
thread's constructors for it have finished, or begun. glibc holds its loader
lock, which is recursive, across the constructors.

Nothing shows it: no guest case opens one library from two threads while its
constructor is slow.

**Exit**: an open that finds a library its constructors have not finished
waits for them, while a constructor's own load in the constructing thread
still proceeds. A host test of the waiting rule holds it, and a guest case
opens one library from two threads, the first's constructor slow, and the
second reads the constructor's effect.
