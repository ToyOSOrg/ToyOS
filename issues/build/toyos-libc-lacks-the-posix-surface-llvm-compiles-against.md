---
status: open
kind: defect
opened: 2026-09-30
---

# ToyOS's libc lacks the POSIX surface LLVM compiles against

LLVM does not build for a ToyOS host, so neither do clang and lld (M2 of
`issues/build/toyos-builds-itself.md`) nor a rustc that carries it (M3). The 71
libraries rustc links (`llvm-config --libnames` of
`compiler/rustc_llvm/build.rs`'s required components, with `x86` and
`aarch64`) stop in `LLVMSupport`, `LLVMTargetParser` and `LLVMObjectYAML`, on
names the C library does not declare or define. Measured by configuring
`src/llvm-project/llvm` at `849da7d62` for `x86_64-unknown-toyos`, with CMake's
system `ToyOS` and `UNIX=ON` as `src/libcxx.rs` names it, against #637's C
sysroot (`de0de8ee7862147a`), compiling with `ninja -k 0`, and declaring each
missing name in a scratch header until all 71 archives built, exit 0. Eight
objects outside those 71 also failed (ORC, the interpreter, `llvm-objcopy`'s
Mach-O, `llvm-exegesis`) and were not chased, and clang's and lld's own
libraries were not built.

**Headers the C sysroot does not have:** `sys/resource.h`, `sys/utsname.h`,
`sys/statvfs.h`, `sys/un.h`, `pwd.h`, `sysexits.h`, and `endian.h`, which
`bit.h` includes once ToyOS joins its list
(`issues/build/bootstrap-cannot-build-llvm-clang-and-lld-for-a-toyos-host.md`). The
scratch headers carried `machine/endian.h` and `MNT_LOCAL`, BSD names LLVM
reads only on a system it does not list; that issue's two LLVM arms answer
them, not libc.

**Names its headers do not declare:**
- `signal.h`: `pthread_sigmask`, `SIGUSR1`, `SIGUSR2`, `SA_ONSTACK`,
  `SA_RESETHAND`, `SA_NODEFER`.
- `unistd.h`: `gethostname`, `getsid`, `setsid`, `execv`, `execve`,
  `readlink`, `symlink`, `link`, `fchown`, `_SC_ARG_MAX`, `_SC_PAGE_SIZE`,
  `_SC_GETPW_R_SIZE_MAX`.
- `string.h` and `stdlib.h`: `strnlen`, `strsignal`, `realpath`.
- `limits.h`: `_POSIX_ARG_MAX`.
- `sys/mman.h`: `msync`, `MS_SYNC`, `madvise`, `MADV_WILLNEED`,
  `MADV_DONTNEED`.
- `fcntl.h`: `struct flock`, `F_SETLK`, `F_SETLKW`, `F_RDLCK`, `F_WRLCK`,
  `F_UNLCK`, which `fcntl` answers as
  `issues/build/libc-fcntl-and-fchmod-answer-0-and-do-nothing.md` says.
- `sys/socket.h`: `AF_UNIX`. `dlfcn.h`: `Dl_info`, `dladdr`.

**Functions no archive defines**, once those compile. rustc's LLVM wrapper
(`compiler/rustc_llvm/llvm-wrapper`, whole), the 71 archives, `libc++.a` and
the Rust sysroot's archives, `libtoyos_c.a` among them, linked by `ld.lld
-shared -z defs`, leave 29 C functions undefined. 22 are this issue's:
`dladdr execv execve fchown fstatvfs getpwnam_r getpwuid_r getrlimit link logb
madvise modf msync pthread_sigmask readlink realpath setrlimit setsid statvfs
strsignal symlink uname`. LLVM reaches `execv`, `execve` and `setsid` only
after libc's `fork`, which answers `ENOSYS`
(`llvm/lib/Support/Unix/Program.inc`, `Execute`).

**A function it has and does not do.** `sigprocmask`
(`userland/libc/src/misc.rs`), which LLVM calls
(`llvm/lib/Support/Unix/Signals.inc`), answers 0 and neither sets nor reports
a mask.

**Left to other issues.** `wait`, `wait4`, `sigemptyset`, `sigfillset` and
`sigaddset` are stage 3 of
`issues/kernel/a-childs-end-is-an-event-and-a-parent-takes-its-children-down.md`,
with the `posix_spawn` LLVM starts a child through once libc has one.
`dirent.h`, `ftruncate` and `fchmod`, which libc defines and declares nowhere, are
`issues/build/libc-headers-are-written-by-hand-and-drift-from-its-definitions.md`'s.
`alarm` is `issues/build/libc-has-no-alarm.md`'s, and `aligned_alloc`
`issues/build/a-rust-std-program-defines-no-aligned-alloc.md`'s.

**Exit**: every name listed above is declared by the header POSIX puts it in,
and each function among them is defined and does what POSIX specifies — a name
POSIX does not specify, what its Linux manual page does — or refuses as POSIX
has it report an error, with `ENOSYS` where ToyOS has no such call: `setsid`
and `getsid`, the child-process track ruling out a POSIX session, and `execv`
and `execve`, no call replacing a process's image. A guest C case per function
asserts its answer, each refusal among them, and reads back the effect of each
that has one: the old set a second `pthread_sigmask` or `sigprocmask` answers,
`getrlimit` after `setrlimit`, `readlink` of what `symlink` made, and the file
read through the name `link` made. A host test compiles and links, with the
toolchain's clang against the C sysroot, one C probe per name.
