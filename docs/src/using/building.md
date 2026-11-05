# Building and running

## One command

- From a clean clone, `cargo run` builds everything and boots the system in
  QEMU, and `cargo test` boots whole systems to run the integration suite.
- The build system is an ordinary Rust program; it names anything missing
  before it starts.
- Linux and macOS are supported hosts.

## The toolchain is ToyOS's

- ToyOS carries its own forks of the Rust compiler and of LLVM, where the
  ToyOS targets live.
- One LLVM is both rustc's code generator and the clang that builds C programs,
  and its linker links the UEFI loader, the kernel and every program.
- Nothing installed on the host goes into anything that boots.
- The first build bootstraps the toolchain once per machine; later builds
  rebuild only what changed.

## Images

Every build produces GPT-partitioned, UEFI-only disk images, ready to write to
a USB stick. One kernel and loader serve three boot configurations:

| Image | Purpose |
|---|---|
| The system | The whole desktop and its services; what QEMU boots |
| Diagnostic | Nothing may claim the display, so the kernel's log and panics stay readable on a machine with no serial port |
| Console | A shell on the raw framebuffer, without the compositor |

Development images are signed with a key generated for the checkout; an image
for an owner's machine is signed with the owner's key.
