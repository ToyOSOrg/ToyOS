---
status: open
kind: defect
opened: 2026-09-23
---

# `toyos_ssh fire reboot` can end the reboot it asked for

`fire` (`tests/ssh-client-host`) asks for a program, reports the guest's
answer to the exec request, and drops the connection without waiting — by
design, because `reboot` never exits. sshd's rule is that nothing it starts
outlives the connection that asked for it (`userland/sshd/src/main.rs`), so a
`reboot` that has not reached its syscall by the time sshd notices the client
left is killed. Seen once, `tests/swapcase`, three guests at once on the dev
host, in a test that ended its boot with `fire`:

```
[kernel 6.822 cpu0] spawn: /system/bin/reboot pid=11 ...
[kernel 6.842 cpu0] exit: reboot pid=11 code=137 cpu=14ms
sshd: 10.0.2.2:56450: the connection is gone; ended /system/bin/reboot
```

The guest then ran on until the harness's stall guard. `lan_talk` and the T14's
talking loop hand the machine back with the same `fire`; on the T14 the loop's
fallback is the boot's own hold, so a lost race costs the rest of the hold, not
the machine. The swap tests end their boots with `exec` instead, which holds the
connection until the machine goes.

**Owed:** a way to ask for a program that does not return and is not ended by
the asker leaving — or `fire` holding its connection until the machine drops it.
