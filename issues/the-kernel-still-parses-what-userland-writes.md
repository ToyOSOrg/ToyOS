---
status: open
kind: track
opened: 2026-08-14
---

# The kernel still parses what userland writes, and the loader move has a deadline

Parked 2026-08-14 by the owner after an external architecture review, to be
taken up when a pipeline slot frees. The direction is his standing one: a
smaller kernel is a more secure kernel, and the answer to a critique is a
deletion, not a defence.

**Move 1 — the userland loader.** Relocations, symbol resolution, TLS layout and
the dlopen family run on attacker-supplied bytes in privileged code. It is not
the largest such parser in Ring 0 — filesystem and USB are both larger — but it
is the only large one whose input an unprivileged process writes byte by byte,
and it holds the most privileged `unsafe`. No production kernel dynamic-links
userland. The end state: the kernel maps the `PT_LOAD`s of a static-PIE image
and jumps; everything else runs in a Ring 3 loader inside the target's own
address space, holding only the file handles it was endowed, where a crafted
binary can only corrupt itself. Five syscalls retire, and
the crafted-ELF corpus becomes the negative gate against a kernel that no longer
parses most of it.

**Stage 1 is built: an executable relocates itself** (`toyos::relocate`,
called first by std's and libc's `_start`), and the kernel reads no
executable's dynamic section, symbol tables or relocations.

**Stage 2 is built: the loader is the program's.** A program lays out its own
thread-local storage (`toyos::tls`): its module list, every thread's area and
DTV, which grows in the process's own memory, and `__tls_get_addr`'s blocks.
The kernel's only part is the thread pointer a thread names
(`SYS_SET_THREAD_POINTER`). The SDK's loader (`toyos::dl`) lays a
library out in private memory with every library it names, relocates them,
protects their pages, their RELRO read-only, and publishes them in the module
list together or not at all; libc's `dlopen`, `dlsym`, `dladdr` and
`dl_iterate_phdr` read that list, and so do libloading, std's unwinder and the
backtrace crate. `SYS_DLOPEN`, `SYS_DLSYM`, `SYS_DLCLOSE`, `SYS_TLS_ALLOC_BLOCK` and
`SYS_QUERY_MODULES` are deleted, and a program may protect its own memory read
and execute, never write and execute at once. An executable's start, std's or libc's, loads the
libraries it names (`DT_NEEDED`) through the same loader before `main`
(`toyos::dl::start`), and `std_tls` holds it. AArch64 shared objects reach
their thread-locals through TLS descriptors, which the loader refuses.

**Stage 3 is built: nothing in the kernel parses an ELF** (owner, 2026-11-05).
A parent lays out its child's program (`toyos::elf::program`) and hands the
kernel ranges to map, each checked as an ordinary mapping is
(`SpawnArgs::maps_ptr`, `abuse_spawn_maps`), the entry, the image base and the
build-id a crash record names. The build lays out the kernel and the
supervisor ahead of the boot (`toyos_kimage`): the loader copies the kernel and
applies its `RELATIVE`s from that table, and the kernel takes its own ranges,
its symbols and the supervisor's program from it. The ELF decoder is a module
of `toyos` (`toyos::elf`), the one parser, which the build, `symbolize` and
the SDK's loader read with; `toyos-elf` is no crate, and the kernel and the
loader link none of it.

The evidence that it is worth doing: nine dated commits over eleven days fixed
userland-reachable kernel defects in this code, six of them machine-wide panics;
seven of the loader's twelve bounds are over quantities a workload sets and two
have no bound at all; and two of those ceilings are *already* exceeded by
artifacts this tree builds.

Its deadline, the day an image ships a hosted rustc whose shared objects the
kernel would have loaded, is met: those bounds are the process's own now.

**Move 2 — filesystem daemons**, sequenced after the completion architecture. A
crafted image attacks the kernel rather than a sandboxed daemon. The FS daemon
needs the blocking story to be efficient, and the boot path needs its story told
first. Told (owner ruling 2026-10-21): the kernel parses no filesystem of
ROOT; the supervisor's file server serves it out of the memory the loader
verified, and every other role is a file server too.

**Move 3 — the panic-time symbol resolver**, small and independent. Ruled
(owner, 2026-10-03, `issues/toyos-explains-itself.md`): he chose
"Adopt it: Built after the folder work, as part of the observability plan.
Frees at least 2 MiB per running program, the kernel stops parsing program
files' name tables, library frames get names." and wrote "adopt, make sure the
code can never panic or fail is that possible? i dont want to allow a kernel
crash because of that and it seems brittle by nature do you agree? can we
share a lot of code there? should we?". The kernel reads no ELF for it: its
own names are the build's table (`toyos_kimage::Symbols`), which resolves as
the ELF symbol table does (`build::kimage`'s test). The kernel names only
itself: a killed program's frame is recorded as the file it ran, the offset in
that file and the build-id its parent handed the spawn
(`lib/symbols/src/frame.rs`), and `/system/bin/symbolize` names it out of the
file with `toyos::elf` and the kernel's demangling code. What is left: a library's frames print bare, because the kernel records
only the executable's image for a process; and `rustc-demangle`'s standing. It
is an unforked crates.io dependency, ~2k lines of third-party string parsing in
Ring 0 on the panic path, now for the kernel's own names alone: fork it into
the estate like every other third-party source, or record the exemption
deliberately. **Exit**: `disk_backtrace`, the T14's shared-boot row,
reads its killed child's report as the child's file, offset and build-id with
no user symbol in it, and `symbolize` names the faulting function from them;
no kernel source builds a backtrace table from a program's file; a library's
frame is recorded as its file and offset; and `rustc-demangle` is forked or its
exemption recorded.

**Small trim to evaluate:** main-thread exit killing the process is kernel policy
the review called unnecessary. A process could end when its last thread does, or
only by explicit exit; the main thread stops being special either way.

**Non-moves, recorded so nobody re-proposes them.**

- **2 MiB pages stay.** The fragmentation is a memory cost, not attack surface,
  and the smaller mm it buys is itself security. Revisit only on measured memory
  pressure, never for this.
- **Invalid handles keep killing the caller.** A process naming a handle it does
  not hold is buggy or probing, and the kill stops handle-guessing cold.
  Compatibility lives in `sdk/libc`, which tracks its own fds.
- **The lifetime models are not unified in place.** They shrink as their
  subjects leave the kernel.
