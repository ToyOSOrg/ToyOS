---
status: open
kind: defect
opened: 2026-10-12
---

# `consent_prompt`'s click on an open launcher's row made no launch

A whole guest suite (`cargo test`) on the development host, on pull request
#889's tree at `c24bee1db`, went red on `consent_prompt` alone, 62 of 63
green, while the host's load average read 34.22, 38.33 and 47.31 a minute
after it ended, on 14 cores. The suite had scaled its liveness ceilings by
1.00x. Of what that test boots or runs, the tree differs from `main`'s at
`ab655a115` only in a doc comment of `toyos_manifest::launch::refused`.

The steps `always`, `again`, `deny` and `denied` passed. At `skip` the host
clicked `+`, saw the launcher's row change on the panel within its 20 s, and
clicked the row. Nothing launched: the supervisor said nothing more of
`consentee`, no window opened, and the compositor logged only idle frames
until the job's 600 s ceiling ended it. With no launch there was no prompt,
so `Consent::prompt_up`, which only the prompt's line starts, never ran. The
log:
https://github.com/ToyOSOrg/evidence/blob/c7a625ddef468380faaca0e94f0be7ffb52dfed3/build/guest.log#L1259

The host waited for the launcher before its second click because two clicks
in one read are one click
(`issues/two-clicks-the-compositor-reads-at-once-are-one-click.md`). That
wait ended when any pixel of the row's rectangle changed (`Consent::begin`,
`tests/toyos.rs` at `c24bee1db`), which is not the row being drawn.

Owner: the desktop track, `issues/toyos-has-a-desktop.md`.

**Its test is deleted**: `ff1a11dbd` cut `consent_prompt` with its image and
the harness's `Consent` steps, under the testing ruling that a test which
launches software and drives it through steps is cut. Nothing now clicks the
launcher's row.

**Exit**: why a click on the open launcher's row made no launch is named and
fixed.
