---
status: open
kind: defect
opened: 2026-08-24
---

# `fat_backing_revoked` panics on a budget-refused create instead of deciding

Seen once, dev host, 2026-08-24, in a full Fast suite running beside two other
suites (three-suite verification chain on a 14-core host). The guest's USB
storage took a real transport break under that load and the test died on the
create that opens its scenario:

```
[kernel 2.797 cpu0] usb-storage: 00:02.0 slot 1 transport broke on SCSI 0x2a: no answer in the status phase in 2000 ms
[kernel 2.801 cpu0] usb-storage: write of 1 blocks at 129159 ran out of its operation budget on disk 0
[kernel 2.801 cpu0] log-volume: create of fat-revoke-victim.bin: the device would not answer in the caller's own budget
thread 'main' (1) panicked at src/bin/fat_backing_revoked.rs:45:63:
create /log/fat-revoke-victim.bin: operation would block
```

Green alone the same session: `PASS (4.2s)`. The kernel behaved exactly as the
slow-disk doctrine says — the refusal line itself states "the caller's own
give-up policy decides whether to ask again" — and this caller's give-up policy
is a panic on the first `WouldBlock`. The test is about revocation after
unlink, not about device patience; its setup create could retry the way
`SYS_FSYNC`'s caller does and lose nothing it is trying to prove.

Two honest shapes: the setup retries `WouldBlock` a bounded number of times
(the doctrine's shape), or a redlist row prices this as a known
load-coincident red on the dev-host instrument. Whoever owns the test decides;
what is not honest is re-running it away without either.

**2026-08-25: promoted.** Verified unchanged: `write_file` in
`tests/toyos-rust-tests/src/bin/fat_backing_revoked.rs` still panics on the
first `WouldBlock` from `fs::File::create`, and no `src/redlist.rs` row prices
this. Neither of the two honest shapes has been taken; whoever owns the test
should pick one.
