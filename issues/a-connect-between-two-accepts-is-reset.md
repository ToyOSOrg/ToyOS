---
status: open
kind: defect
opened: 2026-09-24
---

# A connect that lands between two of sshd's accepts is reset

In one `lan_swap` of about seventy on the dev host, with six guests running and
eight CPU hogs beside them, the host's `reboot` over ssh was refused at connect.
The guest had just answered an `echo` session on the same forward:

```
[swap] `reboot` Err("error connecting to 127.0.0.1:57102: Connection reset by peer (os error 54)")
```

The guest's console shows no `sshd: connection from` for it. It only shows the
`echo` session's threads ending. So the SYN was answered with a reset rather
than queued. A smoltcp listening socket becomes the connection it accepts, and
the port has no listener until another is bound. A host that connects inside
that window is told nothing is there.

What is not known is whether the window is netd's re-arm or sshd's accept loop,
and how wide it gets under load. A backlog of one is not a listener. Retrying at
the client hides this and does not fix it.

**`lan_swap` is deleted**, as a red test is: `bb68c186c` took it out, its
QEMU and T14 rows both, with `lan_swap_hold`, the T14 row's judge and the
metal harness's swapping boots, which that row was the one user of, and
`29733dba3` then deleted the swap file and `--hand-back` only they used.
`toyos-metal --swap`, the host's stream and its ssh client went after them;
`issues/the-host-cannot-reach-the-t14-while-it-runs-toyos.md` records the commit
that restores those.

netstack's listener is the node's now, whose queue is [tcp]'s: a connect
that arrives while another waits to be accepted is queued
(`a_connect_between_two_accepts_is_queued_not_reset`,
`userland/netstack/node/tests/listeners.rs`), and in a guest two host peers
that dial before any accept are both accepted (`netstack_streams`, which on
smoltcp ended `the listener was woken for 1 of two peers`). What is left of the
exit is `lan_swap`.

**Exit**: a listener that queues a connect arriving between two accepts, and
`lan_swap` restored and green.
