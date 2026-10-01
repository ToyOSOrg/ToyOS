---
status: open
kind: defect
opened: 2026-09-30
---

# The C++ runtime's configure names ToyOS to CMake as `UNIX`

CMake has no platform module for ToyOS, so `src/libcxx.rs` configures the
runtimes with `CMAKE_SYSTEM_NAME=ToyOS` and sets `UNIX=ON` beside it: the one
fact a platform module would give that the runtimes' build branches on. CMake
says so on every configure, once per check it runs: `System is unknown to
cmake, create: Platform/ToyOS to use this system`. Everything else a platform
module sets (library prefixes and suffixes, search paths, the shared-library
flags) CMake leaves at its defaults, which the runtimes' static-only build
happens not to read.

**Exit**: CMake knows ToyOS, a `Modules/Platform/ToyOS.cmake` that sets what
its Unix-like neighbours set, and `UNIX=ON` goes from `src/libcxx.rs` with the
line every configure prints.
