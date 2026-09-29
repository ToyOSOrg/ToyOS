---
status: open
kind: tooling
opened: 2026-09-28
---

# A T14 measurement has no way back when the T14 stops answering

A measurement under the T14's Ubuntu, such as the LLVM bar in
`issues/build/toyos-builds-itself.md`, is driven and read over ssh to
`t14@192.168.1.46`. At about 19:20Z on 2026-09-28, during the bar's `s3-2`
span, the T14 stopped answering ping and ssh, so the run's state, and whether
its driver and root sampler still run, cannot be read. Which of the T14's
links that address is on is unread; `ip route get` to the development Mac,
run on the T14, reads it.

Owner: the orchestrator, which runs the T14
(`issues/hardware/the-t14-answers-only-through-a-usb-stick.md`). Exit: a second
path to the T14 under Ubuntu, independent of the link that address is on, that
the orchestrator reaches when that link is down.
