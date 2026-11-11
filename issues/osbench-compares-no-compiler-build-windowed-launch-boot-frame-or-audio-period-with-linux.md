---
status: open
kind: tooling
opened: 2026-11-11
---

# osbench compares no compiler build, windowed launch, boot, frame or audio period with Linux

Two performance requirements set ToyOS against Linux on matched work: build
throughput, and an application's launch to a usable state, cold and warm.
`tests/guest/osbench` measures neither as the work itself, nor a boot to a
usable state, a presented frame or a mixed audio period. Each row below is
designed and not built, with what it waits on. A row is the same work on both
systems from the one `osbench` binary; a figure only one system can produce
is no row.

What is measured meanwhile: `work.build_shaped.*`, which compresses files and
is a proxy; `proc.launch.first_output`, a console program from its spawn to
its first line, warm; and the `boot.*` conditions, which are of two different
boots.

## A compiler build: `work.cc.j1`, `work.cc.j<cpus>`

Build throughput is a stated requirement, so this row is owed first.

**The work.** doomgeneric at the commit `apps/doom/build.rs` pins, the C
program the build already compiles for ToyOS with this clang. Each
translation unit is compiled to an object by a `clang -c -O2` process of its
own, one at a time and one per CPU, a new one started as the oldest ends, and
the objects are linked by `ld.lld`: ns for the set, five rounds after one, as
`work.build_shaped` is shaped. The pinned TinyCC corpus (`tests/corpus/tinycc`,
170 C files, 11,018 lines) is the committed alternative if a fetched source
may not be a fixture.

**What makes it one work on both.** One LLVM commit's clang and LLD, one
command line, `--target=x86_64-unknown-toyos` and `--sysroot` naming the
ToyOS C sysroot on both systems, so Linux cross-compiles the same headers to
the same objects. Both must then write the same bytes. The row prints the
SHA-256 of what it linked, and the compare refuses the row where the two
systems' differ.

**What a ToyOS image needs.**

- clang running in a guest at all: `hosted_clang_hello` is red at the link
  until item 5 of `issues/toyos-builds-itself.md` lands, and that track's M2
  exit, its item 6, is open.
- The package `cargo run -- --hosted-clang` makes (`build/hostedclang.rs`),
  83.8 MB, 279 MB installed (`bin/clang` 148 MB, `bin/ld.lld` 86 MB). No CI
  job builds it and no image carries it.
- A way onto a metal image's ROOT. Only a QEMU boot takes extra ROOT files
  (`BootOptions::extra_root_files`, as `hosted_clang_hello` stages the
  package); `metal::Arm` has no such field. ROOT is served from memory, so
  the package costs its 84 MB of RAM for the boot.
- The install: the job list runs `pkg install` first, into `/apps` on the
  bench's DATA, as `tests/images/hostedclangcase.toml` declares it
  (`starts = ["pkg"]`, `installs = true`). The sources ride on ROOT beside
  the package.

**What Linux needs.** That commit's clang and LLD for x86-64 Linux, carried
to the bench beside `osbench` with the same sysroot and sources. The
distribution's clang is another compiler and so other work. The build
already makes this LLVM for whichever machine builds (`build/llvm.rs`,
`llvm/<key>/bin/clang` and `lld`), so a build on an x86-64 Linux host holds
them, linked against that host's C and C++ libraries. Whether they run on a
bench's Ubuntu as built is unverified; if not, they are built on the bench,
or statically for musl, which needs a C++ runtime for that target the build
does not make.

**In order:** item 5 of the self-hosting track, then `hosted_clang_hello`
green through the guest runner; staged ROOT files for `metal::Arm` and the
row's boot (the benchcase image with the package, the sources and `pkg`);
Linux's clang and LLD from an x86-64 Linux build's store; the row in
`work.rs` and the hash in the compare.

## A windowed launch: `launch.window.first_frame`

The work: a child of `osbench` that opens a window with winit and softbuffer,
as every shipped app does, draws it once and ends at the compositor's first
frame callback after that present: ns from the spawn to that callback, warm.
A shipped app cannot be the child: nothing tells another process when an
app's first frame was shown, on either system.

It waits on:

- **A Wayland session on the bench's Linux that an ssh run reaches**, and a
  ruling on which compositor is Linux as installed. Its name joins the
  compare's setup.
- **winit's Wayland backend in a static binary.** The apps build it with
  `wayland-dlopen`, which a static musl binary cannot use; without it the
  protocol is Rust, but winit still loads xkbcommon at run time. Whether a
  window opens without it is unverified.
- **A ToyOS image with the compositor.** `tests/images/benchcase.toml` runs
  none, so that its power rows read a quiet machine: a metal row of its own.
- **Cold**, the requirement's other half, needs each system's file cache
  dropped: root's `drop_caches` on Linux, and no call on ToyOS.

## A boot to a usable state: `boot.kernel_to_session`

The work: from the kernel's clock zero to the session accepting its first
client. Linux records it per unit (`graphical.target`'s
`ActiveEnterTimestampMonotonic`), read by a later run. The TSC runs from
reset on both, so firmware and loader are one reading on each: the counter
at the kernel's zero.

It waits on **ToyOS saying when a session is up**: the kernel's `Boot:
complete` precedes every program, and the supervisor records no moment at
which every `[boot]` program serves. It waits too on a boot of
`images/system.toml`'s programs with a job in it. The benchcase boot starts
six programs before its job runner and reached its first job 0.71 to 0.90 s
after the kernel's zero on the T14 and 0.77 to 0.80 s on the AMD bench, where
Ubuntu's kernel and userspace took 8.6 to 13.0 s and 6.6 to 7.1 s
(2026-11-11): two different boots, which compare nothing.

## A frame: `gfx.frame.interval`, `gfx.frame.present_to_callback`

The work: the windowed child above animating a fixed pattern for 600 frames,
each drawn on the frame callback: the interval between callbacks, and the
span from a present to its callback, ns. Both draw in software through
softbuffer into the compositor's shared buffer, each system's native path
for these apps. It waits on everything the windowed launch does. The panel's
refresh rate is a condition both must state; Wayland gives it
(`wl_output.mode`), and ToyOS's compositor learns it from the display driver
but its window protocol tells no client.

## An audio period: `audio.period.late`, `audio.underruns`

The work: a cpal output stream of a fixed format and buffer size, as doom's
is, filled with silence for 30 s: how late each callback came against the
stream's period, and how many periods the device played without the
client's data.

It waits on:

- **An underrun count the client can read.** soundserver counts underruns
  for `inspect` and tells no client; ALSA reports an xrun to its caller.
- **cpal in a static Linux binary.** cpal's Linux host is ALSA through
  `libasound`, a shared library. The kernel's PCM device is reachable
  without it but bypasses the sound server Linux as installed plays
  through.
- **A sound device and soundserver in the measured image**, and the bench's
  Linux playing to the same device.
- Latency to the speaker needs a loopback cable and a capture path, which
  ToyOS has none of.

**Exit**: each row above in `osbench` on both systems, or declined here with
its reason.
