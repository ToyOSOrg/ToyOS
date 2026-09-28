---
status: open
kind: tooling
opened: 2026-09-28
---

# Loom never moves a store before a load another thread made first, when the storer loaded the word too

Loom 0.7.2, as `Cargo.lock` pins it, keeps one last access per atomic
(`src/rt/atomic.rs`: `last_access`, `last_non_load_access`). Its DPOR step
(`src/rt/execution.rs`, `schedule`) races a pending load only against the last
store or RMW, and a pending store or RMW against the last access of any kind,
which the storing thread's own earlier load of the word has overwritten. So when
thread A loads a word and thread B then loads and writes it, no execution runs
B's write before A's load.

Measured on a bare loom `AtomicU64`, counting closure runs in a `std` atomic: a
spawned thread doing two `fetch_add`s beside one `load` on the model's thread
explored 10 executions; the same with a `load` at the top of the spawned thread
explored 1; the reader spawned beside the adder on the model's thread explored
19.

`kernel-loom/tests/i8042_tally.rs` had that shape: the model's thread read the
tally, and the spawned ISR's `Tally::record` opens with its saturation check's
load. Its models ran one execution, and a mutation that counted an empty
interrupt as a carrying one passed them. They now spawn the reader and run the
ISR on the model's thread, so the reader's pending load is raced against the
ISR's write, and they refuse a run of one execution (`explored`).

A spawned thread that opens with a load is not the trigger on its own. Every
model below has one, and each explores more than one execution unless it joins
the spawned thread before its own thread races it. None has been read for an
A-then-B pair on one word.

Executions per model, from `LOOM_LOG=loom::model=info` and a throwaway count in
the two `Builder::check` helpers:

| model | the spawned thread's first atomic access | executions |
|---|---|---|
| `kernel-loom` `dump_request` `a_request_filed_during_a_report_is_reported` | `DumpRequest::update`'s load | 16 |
| `dump_request` `one_request_is_taken_once` | `update`'s load | 28 |
| `dump_request` `a_request_is_announced_at_most_once` | `update`'s load | 5777 |
| `dump_request` `a_request_left_during_a_report_is_still_taken_by_its_end` | `update`'s load; joined at once | 1 |
| `log_ring` `a_published_record_is_whole_and_read_once` | `push`'s `tail_then_head` | 1442 |
| `log_ring` `a_slot_is_reused_only_after_its_record_was_read` | `push`'s `tail_then_head` | 43 |
| `log_ring` `a_lane_publishes_whole_and_reuses_only_after_a_read` | `Reader::next_lane`'s `head` | 51 |
| `log_wake` `exactly_one_producer_owns_a_park` | `signal_after_commit`'s load | 7 |
| `panic_capture` `a_reader_and_refresh_never_overlap_on_the_snapshot` | `CaptureAccess::read`, a `fetch_update`, which loom runs as a load then a CAS | 351 |
| `panic_capture` `discard_cannot_admit_a_writer_under_a_fatal_reader` | `read`; `CaptureLatch::owned_by` | 116870 |
| `panic_console_publish` `a_snapshot_is_one_publication_whole` | `publish`'s `seq` | 23613 |
| `reap_gate` `one_raise_is_claimed_once` | `ReapGate::take`'s load | 7 |
| `sleep_lock` `try_lock_observes_the_previous_holders_writes` | `try_lock`'s `now` | 13 |
| `sleep_lock` `a_parking_contender_observes_the_holders_writes`, `a_queued_contender_is_served_and_named`, `two_holders_never_overlap` | `lock`'s `holder` | 225 each |
| `smp_bringup` `a_committed_count_never_outruns_its_slot` | `commit`'s `debug_assert` load; `count` | 45 |
| `smp_bringup` `a_released_machine_is_answering` | `released` | 37 |
| `ticket_lock` `try_lock_observes_the_previous_owners_writes`, `two_try_locks_do_not_both_succeed` | `try_lock`'s `now` | 13 each |
| `tlb_shootdown` `an_acknowledged_flush_postdates_the_page_table_write` | `Shootdown::serve`'s `requested` | 95 |
| `tlb_shootdown` `one_serve_answers_two_concurrent_shootdowns` | `serve`'s `requested` | 70855 |
| `toyos-sched/loom` `loom_park`: all three | `TaskShared::post`'s `state`, through `park::notify` | 79, 18, 18 |
| `loom_retire` `a_wake_and_a_retire_ride_distinct_nodes` | `post`'s `state` | 61 |
| `loom_retire` `a_retire_and_a_wake_never_both_claim_a_parked_task` | `post`'s `state` | 165 |
| `loom_retire` `the_retire_arm_never_loses_a_parked_task_to_a_racing_wake` | `post`'s `state` | 1193 |
| `loom_retire` `the_retire_chase_reuses_one_node_under_a_racing_migration` | `transition`'s `state` | 4 |
| `loom_retire` `an_adopting_cpu_always_observes_the_kill_bit` | `transition`'s `state` | 24 |
| `loom_watch` `a_bounded_post_racing_a_timeout_reaches_a_live_waiter` | `claim_wake`'s `state` | 39 |
| `loom_watch` `a_revoke_racing_the_wait_loop_ends_it_in_every_arm` | `begin_commit`'s `state`, through `prepare` | 199 |
| `loom_watch` `a_transition_racing_an_opening_gate_is_never_missed` | `begin_commit`'s `state` | 2449 |
| `toyos-transport` `loom` `a_published_entry_is_read_whole` | the consumer's `published` | 39 |

**Exit**: each model above read for a word one thread only loads while another
loads then writes it, and every such pair driven with the writer on the model's
thread, as `i8042_tally.rs` now is; or a loom that races a store against every
thread's last load. Owner: orchestrator.
