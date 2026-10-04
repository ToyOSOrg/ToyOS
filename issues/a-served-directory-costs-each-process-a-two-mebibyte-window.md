---
status: open
kind: tooling
opened: 2026-09-27
---

# A served directory costs each process a 2 MiB window, and nobody measured it

Every process that names a file under a served directory connects to that
directory's server once and lends it a `toyos::fs::WINDOW_BYTES` region
(`toyos/src/fs.rs`): 2 MiB of the process's memory per directory it has
touched, held for the process's life, and mapped into the server too. A
desktop process that touches `/home`, `/config`, `/state` and `/apps` holds
8 MiB of windows; fsd maps up to `MAX_SERVED` of them. Whether the window's
size is paid for by the reads that fill it, and what the desktop's processes
hold in windows together, has not been measured.

**Exit**: the resident memory of a desktop boot's processes with and without
their windows, measured, and the window's size justified from it or cut.
