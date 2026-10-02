---
status: open
kind: defect
opened: 2026-10-01
---

# libc lacks names LLVM for a ToyOS host calls

LLVM, clang and lld built for `x86_64-unknown-toyos`
(`issues/build/bootstrap-cannot-build-llvm-clang-and-lld-for-a-toyos-host.md`)
call five names libc neither declares nor defines:

- `shm_open` and `shm_unlink`, with which ORC maps memory on every Unix but
  Android (`llvm/lib/ExecutionEngine/Orc/MemoryMapper.cpp`,
  `TargetProcess/ExecutorSharedMemoryMapperService.cpp`).
- `scanf`, which LLVM's interpreter hands an interpreted program
  (`llvm/lib/ExecutionEngine/Interpreter/ExternalFunctions.cpp`).
- `ctime`, with which `llvm-objdump` prints a file's time stamp.
- `lround`, which libc++'s `std::lround` calls, from clang's `Basic` and its
  static analyzer: clang does not link without it.

**Exit**: libc declares and defines each, doing what POSIX says or refusing in
POSIX's form, asserted by a guest C case that reads its effect back.
