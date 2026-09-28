---
status: open
kind: tooling
opened: 2026-09-28
---

# A T14 measurement has no way back but its WiFi

A measurement under the T14's Ubuntu, such as the LLVM bar in
`issues/build/toyos-builds-itself.md`, is driven and read over ssh to
`t14@192.168.1.46`, which rides the T14's WiFi. At about 19:20Z on
2026-09-28, during the bar's `s3-2` span, the T14 stopped answering ping and
ssh; nobody could reach it physically, so the run's state, and whether its
driver and root sampler still run, cannot be read.

Owner: whoever holds the T14 runbook. Exit: a wired or out-of-band path to
the T14 under Ubuntu that the orchestrator can reach while its WiFi is down.
