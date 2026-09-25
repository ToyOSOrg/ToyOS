---
status: open
kind: finding
opened: 2026-09-25
---

# `swap_refused_device_fails` reds "unanswered nothing in 60s" before it reaches `accepted`

Seen once, in PR #492's round-3 CI (`abi-split` pass, `host` skipped by
design, run 36151956317), as a red beside this PR's own metaltalk/metalswap
work. `swap_once_sshd_answers`'s ssh client
(`tests/ssh-client-host/src/main.rs:408`) printed `unanswered nothing in 60s
after ""`: `SWAP_ANSWER` (60 s) passed with no byte on the channel and
`said` still empty, so `init_accepted` failed on `the swap was answered
Ok("unanswered nothing in 60s after \"\""), where init's `accepted` is owed`
— the swap never reached `accepted` at all.

This is not this round's redial or ceiling: `Stream`/`metaltalk` play no part
in `refused_device_fails` before `accepted`, and this red carries none of
`lan_swap`/`swap_refusals`' 2,080,768-byte upload signature. It is neither
reproduced nor attributed to a cause here — it may be sshd, the ssh client,
or the harness's own timing, and it may not recur.

## Exit condition

Reproduced (or shown gone) and attributed to an owning module, at which point
this is promoted to a `defect` there or closed as noise with the run that
showed it gone.
