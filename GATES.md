# Vacuous-gate sweep

This sweep asks one question of each host gate: what wrong or partial change to the code under test would it still pass? It covers every `#[test]` in the nine first-priority host-gate modules. `SOUND` means the cited assertion rejects the concrete wrong change named in the last column; the other verdicts name what still passes.

## Gate-by-gate verdicts

### `sourcegate`

| Gate | Assertion | Verdict | What the gate distinguishes, or what slips through |
|---|---|---|---|
| `nmi_does_not_log` | `src/sourcegate.rs:466` | NARROW | A new logging wrapper or a direct shard write in the NMI handler passes because the absence scan recognizes only the fixed `LOG_PRODUCERS` spellings. |
| `every_hand_written_auto_trait_impl_is_declared` | `src/sourcegate.rs:555` | NARROW | A generic or line-wrapped `unsafe impl<T> Send/Sync` passes because the scan accepts only lines beginning exactly `unsafe impl Send for` or `unsafe impl Sync for`. |
| `nothing_in_the_kernel_counts_a_reference_by_hand` | `src/sourcegate.rs:581` | WEAK | Hand-counting through an import alias such as `Arc as A` passes because the gate bans fixed raw spellings rather than the operation. |
| `bus_mastering_is_armed_at_exactly_the_declared_sites` | `src/sourcegate.rs:590` | NARROW | A direct write to the PCI command register, or a renamed helper, arms bus mastering without containing `enable_bus_master(` and passes. |
| `every_named_exception_is_still_there` | `src/sourcegate.rs:611` | SOUND | Deleting one allowed call site while leaving its exception row makes the exact per-file count differ and fails. |
| `no_name_resolves_through_a_registry_any_more` | `src/sourcegate.rs:632` | NARROW | A new name registry under identifiers not listed in `RETIRED_REGISTRY` passes; the assertion proves only that the retired vocabulary is absent. |
| `a_retired_abi_name_is_gone_from_the_code` | `src/sourcegate.rs:651` | NARROW | Reusing a retired ABI number under a new name passes even though the test's premise says the number is never reused. |
| `the_registry_scan_reads_code_and_not_prose` | `src/sourcegate.rs:662` | SOUND | A scanner that counts comments, misses a live name, or matches `SYS_CONNECT` inside a longer identifier fails the positive and negative fixtures. |
| `the_scan_reaches_the_trees_it_claims_to` | `src/sourcegate.rs:688` | WEAK | A walker that reads one Rust file per tree plus the already-exempt `mem::forget` sites passes while omitting the rest of both trees. |
| `no_slice_or_exclusive_reference_is_built_over_a_mapped_page` | `src/sourcegate.rs:726` | NARROW | Constructing the same invalid slice through `transmute`, a safe wrapper, or a third mapped-page file passes because only fixed constructors in two files are scanned. |

### `prosegate`

| Gate | Assertion | Verdict | What the gate distinguishes, or what slips through |
|---|---|---|---|
| `no_file_carries_more_prose_than_the_ledger_permits` | `src/prosegate.rs:347` | NARROW | Arbitrary trailing `//` prose passes because the documented measurement deliberately counts a code line with a trailing comment as zero comment lines. |
| `no_claude_md_carries_a_date` | `src/prosegate.rs:364` | SOUND | Adding a `YYYY-MM-DD` line to a discovered guide fails, while the root-guide reachability assertion prevents an empty walk from passing. |
| `the_date_gate_names_the_file_and_the_line` | `src/prosegate.rs:380` | SOUND | Dropping either dated occurrence, its file, or its one-based line makes the exact fixture comparison fail. |
| `the_ratchet_refuses_what_it_is_for` | `src/prosegate.rs:414` | SOUND | Removing any over-ceiling, new-file, ghost-row, total, or stale-row refusal makes its dedicated negative control fail. |
| `the_measure_counts_what_this_module_says_it_counts` | `src/prosegate.rs:447` | SOUND | Counting a trailing comment, failing to count block-comment lines, or changing the documented date grammar fails an explicit case. |
| `the_ledger_is_read_strictly` | `src/prosegate.rs:477` | SOUND | Accepting malformed, blank, unsorted, or duplicate rows makes one of the explicit error assertions fail. |
| `the_walk_reaches_every_tree_and_only_those` | `src/prosegate.rs:500` | NARROW | A walker can retain the six sentinel paths yet omit another source subtree; the name claims every tree but the assertion samples representatives. |

### `writinglaw`

| Gate | Assertion | Verdict | What the gate distinguishes, or what slips through |
|---|---|---|---|
| `one_comment_line_per_four_code_lines_and_not_one_more` | `src/writinglaw.rs:150` | SOUND | Allowing six new comment lines against twenty code lines fails while exactly five passes. |
| `prose_alone_buys_nothing_and_neither_does_removed_code` | `src/writinglaw.rs:162` | SOUND | Funding prose with no new code or with deleted code makes the two concrete branches pass unexpectedly and fails the test. |
| `a_sweep_that_only_cuts_prose_passes` | `src/writinglaw.rs:181` | SOUND | Rejecting a pure 100-line prose deletion fails the positive control. |
| `a_new_file_counts_fully_and_a_deleted_one_counts_negatively` | `src/writinglaw.rs:189` | SOUND | Ignoring a new file or failing to subtract a deletion changes the two staged verdicts and fails. |
| `a_claude_md_never_grows` | `src/writinglaw.rs:206` | WEAK | Growing one `CLAUDE.md` while shrinking another by the same word count passes because `judge` enforces only the aggregate guide-word delta. |
| `the_fork_is_not_this_repositorys_prose` | `src/writinglaw.rs:219` | SOUND | Counting `rust/` comments against this repository changes the asserted zero delta and fails. |
| `the_table_lists_the_heaviest_writer_first` | `src/writinglaw.rs:233` | SOUND | Sorting ascending or including a code-only file changes the exact two-row order and fails. |

### `issuegate`

| Gate | Assertion | Verdict | What the gate distinguishes, or what slips through |
|---|---|---|---|
| `every_issue_file_says_what_it_is` | `src/issuegate.rs:381` | NARROW | An issue with no required `opened:` field passes because the validator checks only `status` and `kind`. |
| `every_citation_resolves` | `src/issuegate.rs:419` | WEAK | A partial citation repair that leaves a dangling path under a misspelled or no-longer-current area passes because unknown areas are skipped. |
| `the_citation_gate_refuses_a_dangling_claim_and_a_dead_name` | `src/issuegate.rs:443` | NARROW | The assertion explicitly permits dangling paths in unknown areas and cannot see deleted bare slugs outside its hyphenated-token grammar. |
| `a_tracker_file_is_recognised_wherever_the_tracker_stood` | `src/issuegate.rs:480` | SOUND | Restricting history recognition to the current `issues/` location fails the old `specs/issues/` fixture. |
| `the_gate_refuses_what_the_readme_does_not_define` | `src/issuegate.rs:506` | NARROW | Removing `opened:` still passes; the negative controls exhaust invalid `status`/`kind` shapes but never assert the README's third required field. |

### `forkcheck`

| Gate | Assertion | Verdict | What the gate distinguishes, or what slips through |
|---|---|---|---|
| `a_pin_at_the_branch_head_is_current` | `src/forkcheck.rs:709` | SOUND | Marking an exact branch-head pin stale changes `wrong` from zero and fails. |
| `a_branch_that_moved_leaves_the_pin_behind_and_the_fix_is_named` | `src/forkcheck.rs:723` | SOUND | Treating an old revision as current, or omitting the replacement head, fails the count and report assertions. |
| `a_second_lockfile_is_judged_on_its_own_pin` | `src/forkcheck.rs:746` | SOUND | Reading only the root lockfile makes `wrong` zero and omits the asserted `sub/Cargo.lock` finding. |
| `a_fork_no_manifest_consumes_is_reported_rather_than_fatal` | `src/forkcheck.rs:766` | VACUOUS | An unconsumed or unreadable fork can never red the check: the fixture deliberately asserts `wrong == 0` and checks only that prose was printed. |
| `an_unreachable_remote_is_not_silence` | `src/forkcheck.rs:777` | SOUND | Treating fetch failure as current or silently skipping it changes `wrong` from one and fails. |
| `an_identifier_is_found_whole_and_never_as_a_fragment` | `src/forkcheck.rs:787` | SOUND | Matching `restack_info` or missing the whole identifier in code/comment fixtures changes the exact line vectors and fails. |
| `a_checkout_is_found_by_repository_name_and_revision` | `src/forkcheck.rs:804` | SOUND | Matching a repository-name prefix or ignoring the revision selects the decoy or accepts the absent revision and fails. |

### `durations`

| Gate | Assertion | Verdict | What the gate distinguishes, or what slips through |
|---|---|---|---|
| `every_committed_price_names_the_partition_that_took_it` | `src/durations.rs:947` | NARROW | A row carrying a false but syntactically valid `shards=12` passes; no assertion relates the label to the run that actually took it. |
| `a_profile_row_reads_the_same_with_and_without_provenance` | `src/durations.rs:959` | SOUND | Parsing the provenance token as part of the duration/name, or accepting a bare name, fails exact cases. |
| `a_whole_run_is_every_shard_of_one_run_exactly_once` | `src/durations.rs:1001` | SOUND | Rejecting a complete 1-of-1 or 3-of-3 set fails the positive controls. |
| `a_partial_or_mixed_set_is_refused_by_name` | `src/durations.rs:1018` | SOUND | Accepting a missing, mixed, duplicated, invalid, or malformed shard set removes its named refusal and fails. |
| `a_fast_only_merge_preserves_committed_nightly_timings` | `src/durations.rs:1038` | SOUND | Dropping an absent ordinary Nightly timing or retaining the old Fast timing fails the two exact values. |
| `a_complete_fast_run_may_not_erase_an_unmeasured_fast_label` | `src/durations.rs:1050` | SOUND | Silently deleting a missing Fast label makes the expected panic disappear and fails. |
| `one_shard_may_not_report_the_same_execution_label_twice` | `src/durations.rs:1064` | SOUND | Overwriting the first duration with the duplicate makes the expected panic disappear and fails. |
| `an_unmeasured_marker_buys_one_red_measurement_commit` | `src/durations.rs:1087` | SOUND | Softening the marker at any base removes the per-scope refusal asserted in the loop and fails. |
| `a_changed_name_priced_without_margin_is_refused_on_a_pull_request_run` | `src/durations.rs:1105` | SOUND | Warning instead of refusing the touched in-band name makes `refused` lack the asserted name and reason. |
| `an_untouched_name_is_a_warning_on_a_landing_and_a_refusal_on_the_nightly` | `src/durations.rs:1128` | SOUND | Refusing the untouched landing price or warning on the nightly violates opposite-arm assertions. |
| `a_declaration_verdict_is_refused_at_every_base` | `src/durations.rs:1163` | SOUND | Softening missing Nightly evidence by base turns the asserted refusal into a warning and fails. |
| `a_missing_or_empty_base_enforces_everything` | `src/durations.rs:1181` | SOUND | Treating an absent or empty base as an empty touched set makes one of the three `renders` assertions fail. |
| `a_measured_nightly_timing_replaces_the_committed_one` | `src/durations.rs:1198` | SOUND | Preserving the stale Nightly value instead of the measured value fails the exact comparison. |
| `the_relegation_scan_reads_what_the_compiler_reads` | `src/durations.rs:1210` | SOUND | Skipping, inventing, or misreading a `RELEGATED` row disagrees with the compiler-built key/value view and fails. |
| `the_registration_scan_agrees_with_the_relegation_table` | `src/durations.rs:1235` | NARROW | A newly formatted Fast registration can be missed while the Nightly-set equality, two sentinels, and loose `>100` floor all remain true. |
| `the_base_a_run_names_is_read_out_of_git` | `src/durations.rs:1296` | SOUND | Reading HEAD for both sides or ignoring the named base makes `retiered` unrendered (or `kept` rendered) and fails. |
| `a_discovered_guest_test_is_named_by_the_file_that_registers_it` | `src/durations.rs:1353` | SOUND | Missing an added, edited, or removed direct `.rs` bin changes the exact touched-name set and fails. |
| `a_touched_name_is_one_whose_declaration_moved` | `src/durations.rs:1432` | SOUND | Treating comments/notes as declarations or missing add/remove/re-tier/re-schedule changes the exact sets and fails. |
| `audio_config_labels_follow_their_one_nightly_registration` | `src/durations.rs:1479` | SOUND | Failing to canonicalize the audio labels drops their committed Nightly rows, while wrongly canonicalizing `not_audio` changes its measured value; either fails. |

### `tiers`

| Gate | Assertion | Verdict | What the gate distinguishes, or what slips through |
|---|---|---|---|
| `the_ci_profile_and_tiers_agree` | `src/tiers.rs:1381` | NARROW | A registered Fast test absent from the profile passes because the validator iterates profile labels and Nightly rows, not all Fast registrations. |
| `the_profile_gate_refuses_missing_cost_evidence` | `src/tiers.rs:1390` | SOUND | Allowing a Cost row with no current label makes `unwrap_err` fail. |
| `the_profile_gate_refuses_a_slow_fast_label` | `src/tiers.rs:1399` | SOUND | Leaving an over-ceiling label Fast removes the expected refusal and fails. |
| `the_commitment_line_is_four_fifths_of_the_ceiling` | `src/tiers.rs:1408` | SOUND | Changing either constant away from 8,000/10,000 or below the 25% consequence fails arithmetic assertions. |
| `a_fast_label_priced_without_margin_is_refused` | `src/tiers.rs:1428` | SOUND | Moving either boundary by one millisecond or swallowing the ceiling verdict fails the three boundary cases. |
| `a_cost_row_returns_to_fast_only_with_margin` | `src/tiers.rs:1448` | SOUND | Returning with one label still in the band, or retaining Nightly when all labels have margin, fails opposite controls. |
| `a_nightly_measurement_drifts_ci_ms_and_still_validates` | `src/tiers.rs:1477` | SOUND | Comparing fresh prices to stale `ci_ms` documentation rejects the deliberately drifted profile and fails. |
| `a_cost_row_with_margin_still_reds_despite_drifted_ci_ms` | `src/tiers.rs:1488` | SOUND | Letting stale `ci_ms` launder a current with-margin Cost row makes `unwrap_err` fail. |
| `only_fast_can_carry_the_one_run_unmeasured_marker` | `src/tiers.rs:1497` | SOUND | Rejecting a Fast marker or accepting a Nightly marker violates the positive/negative pair. |
| `audio_profile_labels_have_one_registration_name` | `src/tiers.rs:1507` | SOUND | Canonicalizing all `(smp=...)` labels or failing to canonicalize the two audio registrations changes exact results and fails. |

### `redlist`

| Gate | Assertion | Verdict | What the gate distinguishes, or what slips through |
|---|---|---|---|
| `the_registry_is_read_out_of_the_harness_and_is_not_empty` | `src/redlist.rs:3425` | NARROW | Missing an unlisted registration after a formatting change passes as long as five sentinels, two guest bins, and the loose `>100` floor survive. |
| `every_row_can_say_what_it_claims` | `src/redlist.rs:3446` | NARROW | A row may cite an unrelated source location that merely contains the test name somewhere; the assertion validates shape and reachability, not the claimed evidence. |
| `the_index_prints_what_it_is_carrying` | `src/redlist.rs:3471` | VACUOUS | The test has no assertion at all: any counts or rendering are accepted provided computing them does not panic. |
| `the_gate_refuses_what_it_is_written_against` | `src/redlist.rs:3531` | NARROW | Dropping empty-field, future-date, duplicate-row, empty-retirement-reason, or one-sample-quiet refusals passes because none has a negative control here. |
| `a_zero_never_reads_as_a_red` | `src/redlist.rs:3575` | SOUND | Rendering quiet, live-red, never-measured, or unknown-name cases under the wrong headline violates explicit distinct answers. |
| `retired_and_disputed_rows_do_not_decide_a_name_by_themselves` | `src/redlist.rs:3611` | SOUND | Letting retired history silence a live row or treating a disputed-only row as known-red changes the asserted headlines. |
| `every_answer_names_its_instrument_and_what_that_instrument_cannot_say` | `src/redlist.rs:3629` | NARROW | Answers for every name except `screen_pager_keys` may omit their instrument or limitation; the universal name is backed by one fixture. |
| `a_name_that_is_also_exempted_says_so` | `src/redlist.rs:3641` | SOUND | Omitting either the overlap warning or the non-exemption warning for `hda_tone` fails. |

### `pr`

| Gate | Assertion | Verdict | What the gate distinguishes, or what slips through |
|---|---|---|---|
| `a_branch_mixing_the_sysroot_with_dependent_work_is_refused` | `src/pr.rs:609` | SOUND | Allowing a two-commit ABI-plus-dependent branch makes the expected error disappear and fails. |
| `the_inseparable_trailer_is_the_escape_and_it_is_in_the_history` | `src/pr.rs:637` | WEAK | Recognizing the bare `Abi-Inseparable:` prefix without requiring the promised reason passes; only a well-formed trailer is exercised. |
| `an_abi_only_branch_and_an_ordinary_branch_both_pass` | `src/pr.rs:646` | SOUND | Rejecting either a sysroot-only branch or a branch with no sysroot source fails the two positive controls. |
| `the_branch_gets_main_before_it_is_pushed` | `src/pr.rs:670` | WEAK | A `prepare` implementation that pushes first, merges main, then pushes again passes: this test calls only the merge helper and never observes remote ordering. |
| `a_conflict_is_left_in_the_worktree_and_recognised_next_time` | `src/pr.rs:701` | SOUND | Aborting a conflict or letting the next preflight continue removes the asserted merge state, markers, or refusal and fails. |
| `a_dirty_worktree_and_main_itself_are_refused_by_name` | `src/pr.rs:716` | SOUND | Accepting uncommitted work or running on `main` makes one of the two expected errors disappear. |
| `the_first_push_is_told_to_open_a_draft_and_later_ones_are_not` | `src/pr.rs:738` | SOUND | Giving the first push the later instructions, omitting `--draft`, or repeating the first-push prompt after the branch exists violates explicit before/after assertions. |

## Ranked non-SOUND findings

Worst first, ranked by how much the tree would believe wrongly if this gate were the only support for the claim:

1. **VACUOUS — `the_index_prints_what_it_is_carrying`** (`src/redlist.rs:3471`): a test advertised as run-time visibility has no assertion, so every incorrect count or presentation is green.
2. **VACUOUS — `a_fork_no_manifest_consumes_is_reported_rather_than_fatal`** (`src/forkcheck.rs:766`): a dependency declared but never compared is explicitly a zero-error result, letting the fork audit report without gating.
3. **WEAK — `the_branch_gets_main_before_it_is_pushed`** (`src/pr.rs:670`): the merged-result landing property can be broken by pushing before merging while the helper-only test remains green.
4. **NARROW — `no_slice_or_exclusive_reference_is_built_over_a_mapped_page`** (`src/sourcegate.rs:726`): a memory-safety claim is only a two-file spelling ban and misses equivalent constructors and locations.
5. **WEAK — `every_citation_resolves`** (`src/issuegate.rs:419`): misspelling an issue area turns a dangling claim into a deliberately ignored fixture and makes the tracker look internally resolved.
6. **WEAK — `a_claude_md_never_grows`** (`src/writinglaw.rs:206`): growth in one guide is laundered by an equal shrink in another despite the per-file name.
7. **NARROW — `the_ci_profile_and_tiers_agree`** (`src/tiers.rs:1381`): a Fast registration can have no timing evidence and remain invisible to the bidirectional-sounding gate.
8. **NARROW — `a_retired_abi_name_is_gone_from_the_code`** (`src/sourcegate.rs:651`): the asserted no-reuse policy can be violated by recycling the number under a new name.
9. **NARROW — `no_name_resolves_through_a_registry_any_more`** (`src/sourcegate.rs:632`): deleting old identifiers is not proof that the architecture has no replacement registry.
10. **NARROW — `every_issue_file_says_what_it_is`** (`src/issuegate.rs:381`): a required `opened` field may be absent from every issue without this gate noticing.
11. **NARROW — `the_gate_refuses_what_the_readme_does_not_define`** (`src/issuegate.rs:506`): its oracle omits that same required field while claiming the README as its boundary.
12. **WEAK — `nothing_in_the_kernel_counts_a_reference_by_hand`** (`src/sourcegate.rs:581`): importing a banned operation under an alias defeats the ownership gate.
13. **NARROW — `bus_mastering_is_armed_at_exactly_the_declared_sites`** (`src/sourcegate.rs:590`): direct hardware writes bypass the helper-name inventory.
14. **NARROW — `every_hand_written_auto_trait_impl_is_declared`** (`src/sourcegate.rs:555`): generic or rewrapped unsafe auto-trait impls bypass a safety inventory.
15. **NARROW — `nmi_does_not_log`** (`src/sourcegate.rs:466`): the absence oracle is coupled to a closed list of producer spellings, so a new producer has no positive control.
16. **WEAK — `the_scan_reaches_the_trees_it_claims_to`** (`src/sourcegate.rs:688`): one file per tree and one permitted needle can stand in for the full claimed walk.
17. **NARROW — `the_registration_scan_agrees_with_the_relegation_table`** (`src/durations.rs:1235`): missed Fast registrations are outside the compiler comparison that gives the scan its apparent independence.
18. **NARROW — `every_committed_price_names_the_partition_that_took_it`** (`src/durations.rs:947`): valid provenance syntax is accepted as true provenance without an independent relation to the measurement.
19. **NARROW — `the_registry_is_read_out_of_the_harness_and_is_not_empty`** (`src/redlist.rs:3425`): representative names and a loose count do not establish a complete registry.
20. **NARROW — `every_row_can_say_what_it_claims`** (`src/redlist.rs:3446`): source existence plus a name anywhere in the file does not validate the evidence a row claims.
21. **NARROW — `every_answer_names_its_instrument_and_what_that_instrument_cannot_say`** (`src/redlist.rs:3629`): one special answer stands in for the universal claim.
22. **NARROW — `the_gate_refuses_what_it_is_written_against`** (`src/redlist.rs:3531`): five implemented refusal classes have no negative control under an exhaustive-sounding name.
23. **NARROW — `the_citation_gate_refuses_a_dangling_claim_and_a_dead_name`** (`src/issuegate.rs:443`): its own fixtures codify dangling and dead-name shapes that remain green.
24. **NARROW — `no_file_carries_more_prose_than_the_ledger_permits`** (`src/prosegate.rs:347`): trailing prose is outside the measurement despite the all-prose name.
25. **NARROW — `the_walk_reaches_every_tree_and_only_those`** (`src/prosegate.rs:500`): six sentinels cannot prove complete subtree coverage.
26. **WEAK — `the_inseparable_trailer_is_the_escape_and_it_is_in_the_history`** (`src/pr.rs:637`): the escape can be asserted with an empty justification even though the protocol promises a recorded reason.

## Counts

| Verdict | Count |
|---|---:|
| SOUND | 54 |
| WEAK | 6 |
| NARROW | 18 |
| VACUOUS | 2 |
| **Total** | **80** |

## UNCOVERED

None recorded within the completed host-gate scope.

## Stopping point

Completed priority 1: all 80 tests in `sourcegate`, `prosegate`, `writinglaw`, `issuegate`, `forkcheck`, `durations`, `tiers`, `redlist`, and `pr`. Stopped before priority 2: the absence/refusal guest gates in `tests/toyos.rs` and `tests/common/`.
