---
status: open
kind: defect
opened: 2026-09-27
---

# A file server maps a lent window at the size it expects, not the size it is

fsd adopts the region a client lends at its hello with
`SharedMemory::adopt(lent, WINDOW_BYTES)` (`userland/fsd/src/main.rs`), and
`adopt` checks nothing about the region: its doc says a peer that promises a
size and sends a smaller region is the reader's to bound. A region
`SYS_SHM_CREATE` made is whole 2 MiB pages, so it is never short of the window.
A region a device claim answered is not made that way — a BAR aperture or a
scanout is whatever the device is — and a program holding one can lend it. The
server then copies a read's bytes into device memory, or past the end of a
short mapping into whatever follows, and a fault there ends DATA's server,
whose restarts are budgeted for the whole machine.

**Exit**: the kernel answers a region's size and whether it is memory it
allocated to the holder of a handle to it, fsd refuses a hello whose window is
not `WINDOW_BYTES` of that memory by name, and a guest test lends a region a
device claim answered and is refused while the server answers the next client.
