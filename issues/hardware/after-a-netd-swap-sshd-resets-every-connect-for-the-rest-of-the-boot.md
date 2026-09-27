---
status: open
kind: defect
opened: 2026-09-27
---

# After a netd swap, sshd's port resets every connect for the rest of the boot

`bench_loop_drives_a_toyos_machine` on the dev host (QEMU 11.1.1, TCG,
`--nightly --jobs 1`, alone, at `be959b5d`) went red on its first attempt and
green on the harness's alone re-run:

```
FAIL bench_loop_drives_a_toyos_machine: the loop refused: the swap did not put the new binary in service:
  `echo the T14 answers over its own cable` afterwards answered Err("error connecting to 127.0.0.1:55861: Connection reset by peer (os error 54)")
```

The guest's console on the red attempt:

```
9.026 init: swap netd: started: …/netd as pid 10; in service if it runs 5000 ms
9.047 sshd: the listener on port 22 failed (netd error); binding it again
9.065 sshd: listening on port 22
9.277 sshd: connection from 10.0.2.2:55934
10.314 sshd: session error: … "early eof"
12.331 sshd: connection from 10.0.2.2:55935
13.589 sshd: session error: … "early eof"
14.043 init: swap netd: in service: …/netd as pid 10
```

After 13.589 no connection reaches sshd until the test's 60 s bound, and
sshd logs no listener failure. The host's every dial to the forward in those
46 s is reset. The green attempt's sshd goes on accepting: its session at
18.804 authenticates and runs `echo`.

So after sshd's second accept on the new netd, port 22 has no listener, and
nothing says so. `issues/hardware/a-connect-between-two-accepts-is-reset.md`
is the one-connect form of this window, and here it never closed. Not known:
whether sshd never re-armed its listener after the accept, or netd dropped it.

`cargo run -- --known-red bench_loop_drives_a_toyos_machine` answers NO, and
`lan_swap` too.

**Exit**: a listener that sshd holds across its accepts, or a failed re-arm
that sshd logs and retries, and the bench loop's swap green without the
harness's re-run.
