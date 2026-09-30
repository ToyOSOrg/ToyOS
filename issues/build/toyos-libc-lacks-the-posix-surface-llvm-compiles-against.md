---
status: open
kind: defect
opened: 2026-09-30
---

# ToyOS's libc lacks the POSIX surface LLVM compiles against

LLVM does not build for a ToyOS host. The 71 libraries rustc links
(`llvm-config --libnames` of `compiler/rustc_llvm/build.rs`'s required
components, with `x86` and `aarch64`) stop in `LLVMSupport`,
`LLVMTargetParser` and `LLVMObjectYAML`, on names the C library does not
declare or define. Measured by configuring `src/llvm-project/llvm` at
`849da7d62` for `x86_64-unknown-toyos`, with CMake's system `ToyOS` and
`UNIX=ON` as `src/libcxx.rs` names it, against #637's C sysroot
(`de0de8ee7862147a`), compiling with `ninja -k 0`, and declaring each missing
name in a scratch header until all 71 archives built, exit 0.

**Headers the C sysroot does not have:** `dirent.h` (its functions are in
`libtoyos_c.a`), `sys/resource.h`, `sys/utsname.h`, `sys/statvfs.h`,
`sys/un.h`, `pwd.h`, `sysexits.h` and `endian.h`.

**Names its headers do not declare:**
- `signal.h`: `sigemptyset`, `sigfillset`, `sigaddset`, `pthread_sigmask`,
  `SIGUSR1`, `SIGUSR2`, `SA_ONSTACK`, `SA_RESETHAND`, `SA_NODEFER`.
- `unistd.h`: `alarm`, `gethostname`, `getsid`, `setsid`, `execv`, `execve`,
  `readlink`, `symlink`, `link`, `ftruncate`, `fchown`, `_SC_ARG_MAX`,
  `_SC_PAGE_SIZE`, `_SC_GETPW_R_SIZE_MAX`.
- `string.h` and `stdlib.h`: `strnlen`, `strsignal`, `realpath`.
- `sys/stat.h`: `fchmod`. `sys/wait.h`: `wait4`. `limits.h`: `_POSIX_ARG_MAX`.
- `sys/mman.h`: `msync`, `MS_SYNC`, `madvise`, `MADV_WILLNEED`,
  `MADV_DONTNEED`.
- `fcntl.h`: `struct flock`, `F_SETLK`, `F_SETLKW`, `F_RDLCK`, `F_WRLCK`,
  `F_UNLCK`.
- `sys/socket.h`: `AF_UNIX`. `dlfcn.h`: `Dl_info`, `dladdr`.

**Functions no archive defines**, once those compile. rustc's LLVM wrapper
(`compiler/rustc_llvm/llvm-wrapper`, whole), the 71 archives, `libc++.a` and
the Rust sysroot's archives, `libtoyos_c.a` among them, linked by `ld.lld
-shared -z defs`, leave 29 C functions undefined: `alarm aligned_alloc dladdr
execv execve fchown fstatvfs getpwnam_r getpwuid_r getrlimit link logb madvise
modf msync pthread_sigmask readlink realpath setrlimit setsid sigaddset
sigemptyset sigfillset statvfs strsignal symlink uname wait wait4`.
`aligned_alloc` is the C allocator's, which in a std program is std's: it
defines `malloc`, `calloc`, `realloc` and `free`, and not `aligned_alloc`.

**Exit**: those 71 libraries compile for `x86_64-unknown-toyos` against the C
sysroot with nothing declared beside it, and that link leaves no C function
undefined.
