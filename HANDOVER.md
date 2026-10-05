# Handover: cloud batch A–E (session_015tYBoMwh9xUcBBLG35wTFr), 2026-10-05

The cloud orchestrator ran out of usage. Everything below is pushed. This branch
(`claude/bold-davinci-opii4t-handover`, an orphan branch that never lands) holds this file and the
cloud job directory `job/` (logs, PR bodies, reviews, mutation patches, the gate helper). Paths
like `B/host.log` below are under `job/`.

Brief: the CTO orchestrator's batch A–E (A rootless host suite, B one handle-send API, C irq
census/selftests/console, D per-session file shares, E AML next stage after #739 merges).

## State per task

| Task | PR / branch | Head | Gates | Review | Next |
|---|---|---|---|---|---|
| A | no PR yet; `claude/bold-davinci-opii4t-A` | `ec4f9645` | Root control at origin/main f260e0b9: EXIT=1 (`A/control/`, the negative control). **No gate at ec4f9645 yet** (root or non-root). | none | Gate ec4f9645 as root AND non-root (the issue's exit), write the PR body (`A/pr-title.txt`, `A/commit-msg.txt`, `A/draft/` are the start), open a draft PR, review. |
| B | [#740](https://github.com/ToyOSOrg/ToyOS/pull/740), ready (not draft), `claude/sleepy-cori-ejictl-B` | `b07c745b` | Non-root host EXIT=0 (75 steps). **CI green: host, toolchain/build, guest/suite.** 5 SDK controls each 101. | r1 LAND AFTER NAMED CHANGES; the named changes are in b07c745b (no further review per orchestrator.md). | Owed before land: the **whole-change guest negative control** (`netstack_gone_mid_bind` with its new assertion on the base SDK, must red). Metal rows owed: `process_tree`, `launch_toctou`, `launch_authority`, `fs_share`. Then merge queue — the owner decides. |
| C | [#741](https://github.com/ToyOSOrg/ToyOS/pull/741), draft, `claude/bold-davinci-opii4t-C` | `c5cc9cf5` | Host EXIT=0 at r1 head 71fb985c (`C/gate2/`). **Fix-round commits 0cfedce2, c5cc9cf5 are pushed but NOT gated**, and the PR body on GitHub is still round 1's (the implementer's draft rewrite is `C/pr-body.md`, possibly unfinished). | r1 LAND AFTER NAMED CHANGES (3 NOTEs). Fix round: NOTE 1 (selftest issue restored, stays open — orchestrator's decision) and NOTE 2 (`Census::raise` in `common::irqcensus`) committed; NOTE 3 (body names every SELFTESTS row as owed) is in the body draft. | Gate c5cc9cf5 non-root, finish and set the body (title now closes two issues), push body, mark ready for CI, land. T14 owed: `lapic_spurious_vector` three arms (patches on the PR), every other SELFTESTS row from arm 1's boot, `irq_census_conservation`. |
| D | [#742](https://github.com/ToyOSOrg/ToyOS/pull/742), draft, `claude/bold-davinci-opii4t-D` | `750af802` | Non-root host EXIT=0. 4 host controls each 101. | r1 **SEND BACK**: 3 BLOCKERs (a session reaches `sshserver`, a login row, and opens fresh shares → exit not met; every boot service now shares one machine share — regression from #729; `metal-2` patch malformed) + 5 NOTEs. | Fix round was interrupted mid-edit: uncommitted WIP pushed as `claude/bold-davinci-opii4t-D-wip` (`fe043dca`, never lands, unbuilt). Resume from it or restart from 750af802 against `D/review-r1.md`. T14 owed: `fs_share`, the metal control, metal-1/2/3. |
| E | — | — | — | — | Blocked: starts only once #739 merges (brief). |

## Outside the batch, still open from before

- [#713](https://github.com/ToyOSOrg/ToyOS/pull/713) (ACPI stage 1): round-8 SEND BACK at 47ac0ce1; the round-9 fix (delete both attended T14 rows, their judges, `presses.txt`, the S5 acceptance; rewrite the S5 issue, the press issue's exit and stage 1's exit; quote the owner's no-manual-steps and power-button rulings verbatim) was lost with an earlier container and never pushed. Also owes `--metal-readback` of `counters`, `acpi_server_events`, `acpi_server_death` on the `acpi1-r13` readbacks (Mac).
- [#739](https://github.com/ToyOSOrg/ToyOS/pull/739) (AML interpreter): round-2 SEND BACK at 0af9f964 (3 BLOCKERs: a LocalX reference outliving its frame; a field store allocating per character outside the Meter; the Meter's silent clamps). Round 3 was lost, never pushed. The T14-tables half of round-1 BLOCKER 2 needs the owner's local tables. E waits on this PR.

## Owner decisions pending

1. #740's whole-change guest control: the Mac, or a short-lived non-draft PR carrying the base SDK plus the new assertion so CI shows the red.
2. Landing: the orchestrator enqueues green LAND PRs, or the owner merges.

## Notes for whoever runs next

- Off-batch defects filed by the branches: `issues/the-irq-census-summary-takes-a-cpus-last-stamped-line-as-its-newest-read.md` (C), `issues/std-says-a-launch-moves-its-handles-to-the-launcher-even-when-the-move-is-refused.md` (B), `issues/four-login-sessions-at-their-shares-take-a-file-server.md` (D, under review).
- `job/gate.sh` was the cloud's non-root host gate (user `gate`, one gate at a time, deletes target dirs; a host gate needs ~15–20 GB). A root gate killed mid-run leaves a root-owned `/tmp/toyos-tmp-<pid>-0` that makes the next non-root gate panic before any step.
- `--ci host` needs only a stock rustup toolchain, not the fork; CI's full run (host + cached toolchain + guest under KVM) took ~23 min on #740.
- Tailscale reached the T14 from the cloud (DERP relay, SSH banner answered); login was not set up. The cloud node `toyos-cloud` is still in the tailnet: remove it from the admin console.
