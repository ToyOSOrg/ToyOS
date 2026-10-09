---
status: open
kind: defect
opened: 2026-10-01
---

# libc lacks names LLVM for a ToyOS host calls

LLVM's tools built for `x86_64-unknown-toyos` call names libc neither
declares nor defines. None is in the clang and `ld.lld` the build makes for
ToyOS (`src/hostedclang.rs`), which compile and link without them; each
stops a build of LLVM's other tools:

- `shm_open` and `shm_unlink`, with which ORC maps memory on every Unix but
  Android (`llvm/lib/ExecutionEngine/Orc/MemoryMapper.cpp`,
  `TargetProcess/ExecutorSharedMemoryMapperService.cpp`).
- `scanf`, which LLVM's interpreter hands an interpreted program
  (`llvm/lib/ExecutionEngine/Interpreter/ExternalFunctions.cpp`).
- `ctime`, with which `llvm-objdump` prints a file's time stamp.

`lround`, which clang calls through libc++'s `std::lround`, is defined
(`userland/libc/src/math.rs`) and held to the host's on the host
(`tests/libc-arch/src/fparts_differential.rs`); no guest case reads it.

**Exit**: libc declares and defines each, doing what POSIX says or refusing in
POSIX's form, asserted by a guest C case that reads its effect back, `lround`
among them.
