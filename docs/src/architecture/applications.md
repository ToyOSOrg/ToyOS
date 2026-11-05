# Applications and portability

## Existing Rust just works

Rust programs build and run on ToyOS unchanged. ToyOS is a real Rust target with
a real standard library, and the ecosystem crates that need platform support,
such as windowing, presentation, audio and asynchronous I/O, get a proper ToyOS
backend in their own upstream projects. ToyOS never asks a program to use a
replacement crate written only for it. ToyOS is a first-class platform and never
pretends to be Linux: a crate that assumes every other OS is Linux gets a native
ToyOS branch.

ToyOS is a normal target for third-party software. Its SDK crates are published
on crates.io, and its toolchain is a release a developer can install and link
with. Its system-call ABI is unstable: a program built against an old SDK may
break, and is rebuilt.

## The same app everywhere

Apps use the same source on ToyOS, Linux under Wayland, macOS and Windows. A
program that runs only on ToyOS must be doing a job only ToyOS has, such as
driving a ToyOS device or serving a ToyOS protocol. X11 is legacy.

## C and C++

C and C++ programs build with the toolchain's clang against ToyOS's libc. libc is
built on ToyOS's native interface, never the other way round, and POSIX
compatibility lives there and nowhere in the kernel. Where POSIX conflicts with
ToyOS's model, libc implements the focused part programs need and refuses the
rest visibly:

- children start by spawn; there is no fork or exec;
- a child receives its standard streams and exactly the descriptors its spawn
  names;
- signals are limited to native events: a child's end, an alarm, and interrupt,
  hang-up and terminate requests. A signal nothing raises is refused, and no
  unsupported operation appears to succeed.

## Loading programs

The kernel constructs a protected process and maps executable segments. Dynamic
linking and library loading are userland work shared by the Rust and C runtimes.
A program's image is read by userland through its own view and handed to the
kernel as memory, never named by a path the kernel resolves.

## Desktop apps

ToyOS's own desktop software uses iced, and for now iced is the one toolkit the
platform supports. Drawing is on the CPU, through the toolkits' own 2D
renderers; a window's pixels reach the compositor through shared memory. GPU
rendering comes later through display and Vulkan drivers, not through a CPU
implementation of Vulkan.

## Packages

An installed app is one directory under `/apps`, immutable once committed,
holding everything the app needs. Its view holds its own directory read-only,
its private folder in the user's home read-write, `/system` read-only, its own
`/tmp`, and whatever the user grants. See
[Installation and updates](../product/installation.md).
