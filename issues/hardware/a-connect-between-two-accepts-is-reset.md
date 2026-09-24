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
