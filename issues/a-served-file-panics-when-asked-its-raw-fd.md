---
status: open
kind: defect
opened: 2026-09-27
---

# A served file panics when asked its raw fd

`std::os::toyos::io::AsRawFd` for `std::fs::File` calls the fork's
`File::as_raw_fd` (`sdk/std/sys/fs.rs`), which panics with
"a file on a file server has no kernel handle" for every file under `/apps`,
`/config`, `/home`, `/state`, `/log` and `/boot`. A crate that takes a file's
fd — to lock it, map it or hand it to a C library — builds for ToyOS and
panics there, which is what "existing Rust just works" rules out.

Owner: the std fork's ToyOS file layer.

**Exit**: `as_raw_fd` on a served file answers without a panic — a kernel
handle that reaches the file's server, as `as_child_stdio`'s pipe does for a
child's writes — with a guest test that asks it of a file under `/home`.
