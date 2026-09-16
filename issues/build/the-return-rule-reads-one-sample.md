---
status: open
kind: tooling
opened: 2026-09-16
---

# The return rule reads one sample, so a straddler returns to Fast on the nightly that happens to price it under the line

`src/tiers.rs`'s `ci_profile_verdicts` refuses a `Why::Cost` row whose every
label in the merged profile is at or under `FAST_COMMIT_MS` (8,000 ms), and
refuses a Fast name priced over it. Both directions read the one profile the
run wrote, so a name whose price crosses the line run to run has no state the
rule accepts on every run: the run that prices it under the line demands the
return, the next one that prices it over demands the relegation, and
`FAST_COMMIT_MS`'s own doc — "A straddler cannot be Fast" — describes a name
the rule cannot hold anywhere.

Three names returned on the nightly `35072262489` partition under that rule
with other hosted samples over the line:

| name | this nightly | other hosted samples |
|---|---|---|
| `log_conservation_smp4` | 7,991 | 8,572 (`ci` run 33202812787) and 8,248 (33212528174), both `main` pushes on 2026-08-28 — its standing `src/redlist.rs` row |
| `screen_gop_firmware_mode` | 7,667 | 12,624, the `ci_ms` its removed `RELEGATED` row carried |
| `screen_console_scroll` | 7,513 | 13,401, the `ci_ms` its removed `RELEGATED` row carried |

All three are `Tier::Fast`, which is where the recorded rule puts a name every
label prices at or under the line; that is the state that holds them while this
stands. A price red on any of them under the merge queue is read against this
record and, for `log_conservation_smp4`, against its standing redlist row —
never re-run away, and never answered by a `Why::Cost` row the next quiet
nightly would refuse again.

Site: the `Why::Cost` arm of the `RELEGATED` loop in `ci_profile_verdicts`,
`src/tiers.rs`. `issues/build/defect-events.md` already records "a relegation
table's cost rule invites back whatever one quiet nightly prices under the
line" as a lesson; this is the rule still doing it.

**Exit condition**: the return rule reads more than one sample — a `Why::Cost`
row returns only when every one of an agreed number of consecutive hosted
partitions prices the name at or under `FAST_COMMIT_MS`, with the samples kept
somewhere a run can read them — and the three names above are placed by that
rule.
