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

Twenty-six `Why::Cost` names returned on the nightly `35072262489` partition
under that rule, and **every one of the 26 has another hosted sample over the
line**: the `ci_ms` its removed `RELEGATED` row carried, from 8,070 ms
(`wall_clock_rtc_dead`) to 13,401 ms (`screen_console_scroll`). That property
picks none of them out, so the exposure is all 26. What orders them is how far
under the line this partition priced each; the last column is the next hosted
partition to price them, pull request #467's own `ci` run `35352795338` two days
later, in ms:

| name | removed row's `ci_ms` | this nightly | under the line by | run `35352795338` |
|---|---|---|---|---|
| `log_conservation_smp4` | 8,248 | 7,991 | 9 | 4,881 |
| `screen_gop_firmware_mode` | 12,624 | 7,667 | 333 | 9,090 |
| `screen_console_scroll` | 13,401 | 7,513 | 487 | 8,235 |
| `log_partition_identity` | 9,516 | 6,896 | 1,104 | 7,854 |
| `fat_backing_revoked` | 8,226 | 6,719 | 1,281 | 8,333 |
| `xhci_full_speed_device` | 8,833 | 6,695 | 1,305 | 7,130 |
| `writeback_durability` | 8,888 | 6,555 | 1,445 | 6,803 |
| `dump_nmi_probe` | 8,098 | 6,023 | 1,977 | 6,374 |
| `fs_rename_durable` | 9,346 | 5,999 | 2,001 | 5,296 |
| `locale_detect` | 9,959 | 5,961 | 2,039 | 5,028 |
| `ftruncate_flush_race` | 9,452 | 5,844 | 2,156 | 8,111 |
| `screen_survived_panic_not_blamed` | 8,477 | 5,794 | 2,206 | 6,548 |
| `xhci_slot_exhaustion` | 8,149 | 5,198 | 2,802 | 5,260 |
| `wall_clock_zone` | 9,347 | 4,988 | 3,012 | 6,756 |
| `esp_filesystem` | 10,123 | 4,970 | 3,030 | 4,791 |
| `wall_clock_rtc_dead` | 8,070 | 4,929 | 3,071 | 6,507 |
| `wall_clock_century_register` | 9,030 | 4,866 | 3,134 | 5,199 |
| `usb_short_read` | 8,150 | 4,751 | 3,249 | 4,510 |
| `writeback_spawn` | 8,820 | 4,748 | 3,252 | 4,658 |
| `idle_stack_guard` | 9,601 | 4,642 | 3,358 | 5,560 |
| `console_line_atomicity` | 8,925 | 4,608 | 3,392 | 4,786 |
| `gpu_set_resolution` | 8,610 | 4,554 | 3,446 | 5,626 |
| `heap_ceiling_recovery` | 10,371 | 4,516 | 3,484 | 4,431 |
| `cache_eviction` | 8,165 | 4,515 | 3,485 | 4,621 |
| `fsync_failed_commit` | 8,386 | 4,248 | 3,752 | 4,394 |
| `double_panic_names_the_fault` | 9,120 | 2,735 | 5,265 | 2,700 |

Three sit within 500 ms of the line on the nightly and the fourth is 1,104 ms
under it; a shard's common price factor spreads about 1.28x from p10 to p90
(`issues/build/a-shards-boot-width-does-not-price-its-tests.md`). **The next
partition put four of the 26 back over the line** — `screen_gop_firmware_mode`
9,090, `fat_backing_revoked` 8,333, `screen_console_scroll` 8,235 and
`ftruncate_flush_race` 8,111 — two of them names the nightly had priced more
than 1,200 ms under it, and priced the nearest, `log_conservation_smp4`, at
4,881. `log_conservation_smp4`'s 8,248 is one of two `main` pushes on 2026-08-28
that priced it over the line — 8,572 (`ci` run 33202812787) and 8,248
(33212528174) — and its standing `src/redlist.rs` row holds both.

All 26 are `Tier::Fast`, which is where the recorded rule puts a name every
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
somewhere a run can read them — and the 26 names above are placed by that
rule.
