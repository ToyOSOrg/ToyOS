---
status: open
kind: defect
opened: 2026-09-28
---

# std's `set_times` answers `Ok` and sets nothing

`File::set_times`, `fs::set_times` and `set_times_nofollow`
(`sdk/std/sys/fs.rs`) return `Ok(())` having set nothing, so a tool that stamps
an output (`touch`, a build system's restat) is told it did.

**Exit condition.** A time-setting call sets the stamp through the kernel or
the file server that holds the file, or is refused as `Unsupported`.
