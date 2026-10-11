---
status: open
kind: defect
opened: 2026-10-11
---

# `consent_prompt`'s skip lost its prompt and the hostile window at host load 100

A whole guest suite (`cargo test`) on the development host, on pull
request #859's tree less this file, went red on `consent_prompt` alone, 61 of
62 green, while the host's load average read 102.81, 105.74 and 93.05 a
minute after it ended, on 14 cores. The suite had scaled its liveness
ceilings by 1.00x, off a fastest boot of 849 ms taken before the load rose.
The steps `always`, `again`, `deny` and `denied` passed; at `skip` the
supervisor logged `consentee: skipped`, the compositor then logged
`dropping client 32790 — its connection is gone` for the prompt, not the
`window closed … by the client itself` the other steps logged, and the
hostile window's output ended. The host's verdict was
`` `esc` changed nothing on the prompt in 20 s ``, `once: the launcher did
not open in 20 s`, and the test exited 101 on `skip`, `once` and `ssh`:
"the hostile window ended". No kernel panic ended the boot: the `ssh` step
ran after it.

`cargo test --test toyos-build -- consent_prompt` alone on the same tree a
minute later, load average 64.47, scaled its ceilings 5.84x and was green.
The log is on https://github.com/ToyOSOrg/ToyOS/pull/859.

Owner: the desktop track, `issues/toyos-has-a-desktop.md`.

**Exit**: why the prompt's connection and the hostile window ended under
load is named, and fixed or bounded; or the suite's ceilings are shown to
be what ran out, and the harness scales them by the load the test met.
