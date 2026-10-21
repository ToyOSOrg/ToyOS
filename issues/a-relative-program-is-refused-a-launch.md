---
status: open
kind: defect
opened: 2026-10-20
---

# A relative program path is refused a launch

`toyos::spawn` asks the launcher with the program's path as the caller holds
it, and the supervisor refuses any path that is not canonical and absolute
(`lib/manifest/src/package.rs`'s `is_canonical`), answering `MSG_REFUSED`. So
under a parent that holds a launcher, `Command::new("./tool")` and
`posix_spawn` of `"./tool"` fail, where the same path spawned directly would
start. The shell works around it for what its user types
(`package::launch_path`); std and libc do not. Read from the code, not
reproduced: no guest test spawns a relative path under a launcher.

*Exit*: a relative program spawned under a parent that holds a launcher
starts, by std and by libc, made absolute against the caller's working
directory by one rule the supervisor's gate shares, and the shell's copy of
that rule goes.
