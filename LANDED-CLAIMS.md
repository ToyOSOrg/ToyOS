# Claims in PRs landed on 2026-08-31 and 2026-09-01

## Result and scope

**Two checkable claims are CONTRADICTED.** Both are in PR #357 and concern the retirement of the two 2026-08-31 locale rows. The remaining checkable claims below are VERIFIED; run-output and remote-package claims that have no committed independent artifact are marked UNVERIFIABLE rather than inferred from prose.

`git log --since=2026-08-31 --merges origin/main` returned 23 merge commits. Ten are first-parent merge-queue landings and therefore PRs merged *into main*; the other thirteen are branch-internal merges carried by those PRs. This audit has one section for each of the ten first-parent landings. Every merge SHA cited below was checked with `git cat-file -t` and resolved as `commit`.

## `273a321cc44e` — #352, broken pipe means `Gone`

- **VERIFIED — producer and error vocabulary.** `kernel/src/object/ops.rs:381` maps `PipeWrite::BrokenPipe` to `SyscallError::Gone`; `userland/libc/src/posix_io.rs:51` maps `Gone` to `EPIPE`.
- **VERIFIED — named consumers moved.** Soundd tests `Gone` at `userland/soundd/src/mix.rs:69`; netd does so at `userland/netd/src/main.rs:1046` and `:1083`; `toyos::net::hangup` maps `Gone` to `NetdNotFound` while its test maps `NotFound` to `Io` (`toyos/src/net.rs:351`, `:587`, `:594`). The guest assertions named in the message now expect `Gone`, including `connect_before_serve.rs:165`, `kill_while_blocked.rs:176`, `handle_transfer.rs:235`, and `pipe_flag_forgery.rs:67`.
- **VERIFIED — the residual was not falsely closed.** `issues/isolation/a-broken-pipe-answers-not-found.md:7` is still open and states that the remaining Rust result is `ErrorKind::Other`; the linked `rust/` worktree is an empty stub here, so the fork implementation itself is **UNVERIFIABLE** from this checkout.
- **UNVERIFIABLE — pasted negative-control, guest, clippy, and build outputs.** No committed output artifact independently replays those historical runs.

Current-tree evidence:

```text
kernel/src/object/ops.rs:381: Some(pipe::PipeWrite::BrokenPipe) => Some(SyscallError::Gone.to_u64()),
userland/libc/src/posix_io.rs:51: SyscallError::Gone => EPIPE,
toyos/src/net.rs:594: assert_eq!(hangup(IpcError::Syscall(SyscallError::NotFound)), NetError::Io);
```

## `7855c75dd060` — #349, `Rights::LOG` holders

- **VERIFIED — wording and manifests are mechanically coupled.** `Rights::LOG` is at `toyos-abi/src/handle.rs:96`; `src/build.rs:1974` implements `the_log_right_doc_names_exactly_the_manifests_holders`, parses all `logread` rows, and compares their holder set to the doc block.
- **VERIFIED — the stale issue was closed.** The diff deletes `issues/design-debt/rights-log-names-a-holder-that-does-not-hold-it.md`; the path and slug have no current-tree hit.
- **UNVERIFIABLE — the historical negative-control and build outputs.** The current gate exists and is run by `cargo test --lib`, but the pasted old-wording failure is not a committed result artifact.

```text
src/build.rs:1974: fn the_log_right_doc_names_exactly_the_manifests_holders() {
src/build.rs:2001: `logread` to {holders:?}
```

## `445d1f5c7547` — #357, paced typed lines

- **VERIFIED — the implementation now waits for the guest.** `shell_type_once` splits through `ps2_bursts`, sends one burst, and calls `await_drained` before the next (`tests/toyos.rs:6017`, `:6257`, `:6280`). `Drained::Panel` checks the decoded input row and `Drained::Bytes` checks the kernel's byte count.
- **CONTRADICTED — the `console_locale_detect` sighting was adjudicated by `TYPING_PACED`.** The merge says both 2026-08-31 rows are retired against this mechanism, and `src/redlist.rs:2520` does so. But the live tracker says the opposite: an ordinary overflow shortens one line and recovers, while a stopped input path explains the exact ten-of-ten verdict (`issues/build/the-console-input-path-can-stop-after-a-ps2-overflow.md:26`). It records a shipping-kernel boot frozen after sixteen bytes and taking zero bytes thereafter. Pacing avoids the trigger; it does not establish that the sighting was queue accumulation.
- **CONTRADICTED — the `desktop_locale_detect` sighting was adjudicated by `TYPING_PACED`.** `src/redlist.rs:2542` retires it, while the same open issue explicitly names *both* locale sightings as the stopped-path shape (`issues/build/the-console-input-path-can-stop-after-a-ps2-overflow.md:29`).
- **VERIFIED — the separate defect was filed and remains open.** The file above has `status: open`, `kind: defect`, and an exit that reads `RX_BYTES` on a reproduced wedge.
- **UNVERIFIABLE — the stated 20/12/120/50-run rates and guest PASS set.** They exist only in the merge message and tracker prose, not in a committed machine-readable result.

```text
issues/build/the-console-input-path-can-stop-after-a-ps2-overflow.md:26: This is the shape of the sightings, and a dropped byte is not.
issues/build/the-console-input-path-can-stop-after-a-ps2-overflow.md:29: what `console_locale_detect` and `desktop_locale_detect` reported on CI
```

## `a71c1479b213` — #351, namespace build flags

- **VERIFIED — ABI and kernel behavior.** `NAMESPACE_KEEP_ALL` and `NAMESPACE_FLAGS_KNOWN` are defined at `toyos-abi/src/syscall.rs:1337` and `:1340`; the kernel rejects unknown bits and `keep_all` plus a keep list before reading the lists (`kernel/src/arch/syscall/ipc.rs:111-116`).
- **VERIFIED — SDK and guest differential.** `Builder::keep_all` sets the flag (`toyos/src/namespace.rs:158`), while `endowment_denied` compares its result to explicit `keep` and expects `InvalidArgument` for an undefined bit (`tests/toyos-rust-tests/src/bin/endowment_denied.rs:169-207`).
- **VERIFIED — the PR did not claim the std half complete.** `issues/isolation/a-provided-name-cannot-reach-an-undeclared-child.md:16` says the ABI half is closed and `:39` still names the direct-spawn `keep_all(parent) + add(extras)` exit. The fork file is **UNVERIFIABLE** here because linked worktrees intentionally have an empty `rust/` stub.
- **UNVERIFIABLE — historical negative-control and guest output.** The assertions remain, but the pasted runs are not stored as results.

## `4c35c92070e6` — #348, five fail-safe fixes

- **VERIFIED — worktree operands.** `src/worktree.rs:48` rejects a path beginning with `-` before the `statvfs` call at `:309`.
- **VERIFIED — bcachefs deletion.** The merge stat removes the abandoned walk and helpers from `bcachefs/src/fs.rs` and its integration tests; no named unbounded delete-prefix walker remains in that crate.
- **VERIFIED — GPT floor ownership.** `BlockDevice` supplies `lba_count_granularity` (`toyos-gpt/src/lib.rs:69`), and parsing derives its allowed slack from that caller value (`:232-269`).
- **VERIFIED — reversible FAT replacement.** `Fat32::replace_rename` remains at `toyos-fat32/src/fs.rs:903` with a staged `Replaced` destination; the separately discovered rollback-error defect remains openly tracked rather than hidden.
- **VERIFIED — panic snapshot reader/writer exclusion.** `CaptureAccess` implements `EMPTY/WRITING/READY/READING`, with `READING` terminal (`kernel/src/drivers/panic_console/access.rs:1-67`), and `discard_capture` uses it at `kernel/src/drivers/panic_console/mod.rs:518`.
- **VERIFIED — the final message correctly narrows its Loom claim.** The message explicitly retracts oracle coverage for non-owner `CaptureLatch::release`; the current model imports the real `CaptureLatch` and `CaptureAccess`, but its six threads do not call a non-owner release. This is a scope limitation, not a contradiction in the final record.
- **UNVERIFIABLE — all pasted test counts, negative controls, and checker outputs.** The tests and controls exist, but their historical outputs are not committed result artifacts.

## `750b1a726304` — #353, mapped pipe data has no slice

- **VERIFIED — no slice crosses the mapped data region.** `toyos-abi/src/ring.rs` exposes direction-specific `Src` and `Dst` (`:47`, `:79`), while the gate `no_slice_or_exclusive_reference_is_built_over_a_mapped_page` is at `src/sourcegate.rs:704` and refuses slice constructors and exclusive references.
- **VERIFIED — overlapping copies use memmove semantics.** `UserBytes::read_run` and `UserBytesMut::write_run` use `core::ptr::copy` (`kernel/src/user_ptr.rs:200`, `:261`), and the whole `PIPE_SIZE` is mapped read-write (`kernel/src/arch/syscall/ipc.rs:62`).
- **VERIFIED — the review correction is reflected.** The gate is specifically about slices or exclusive references; it does not claim all shared references are forbidden. This matches the message's correction about the sound `RingHeader` shared reference.
- **UNVERIFIABLE — the historical negative-control, guest results, and absence of Miri.** Current code and tests verify the shape; the old command outputs are not durable artifacts.

## `355dffa2f64f` — #355, wave-4 triage execution

- **VERIFIED — historical defect counts.** A direct `git grep` count produced 146 `kind: defect` files at `72f0218f6cc0` and 132 at this merge. Current HEAD has 130 after later closures, which does not contradict the dated statement.
- **VERIFIED — deletion/fold arithmetic.** The issue diff contains ten deletions: the one already-fixed file plus nine folded files. Four named files now say `kind: track` (`clippy-has-never-run-here`, `i8042-health-sits-on-the-ten-second-line`, `toyos-cc-has-no-codegen-gate`, and `bcachefs-crate-is-not-bcachefs`).
- **VERIFIED — refusing new findings follows the tracker rule.** `issues/README.md:34` says a finding is promoted or folded at its next review, and `:63` says findings do not accumulate.
- **VERIFIED — the two one-queue defects were not closed.** Both issue files remain; the merge message records why the proposed common fix removed only one sufficient condition.
- **UNVERIFIABLE — “17 retypes refused” as an exhaustive review count.** Git proves which files changed, but the rejected candidate list lives only in the external triage document and merge prose, not in this tree.

Historical count output:

```text
72f0218f6cc05d3278920911b9ed09bf3db68565 146
355dffa2f64f62dbe2d20ed1cd83a89c3136a5cb 132
HEAD 130
```

## `72f0218f6cc0` — #347, hardware/diagnostics bundle F

- **VERIFIED — kernel log tagging.** The kernel formats through `record.tagged("kernel")` (`kernel/src/log/console.rs:217`), and logd's module documentation names the same `LogRecord::tagged` path (`userland/logd/src/main.rs:378`). The final message correctly says the old surgery is removed, not unrepresentable.
- **VERIFIED — xHCI simulator physical presence.** `FakePort` has `present: bool` (`toyos-xhci/sim/src/hub.rs:60`), and the independent invariant retains `EnumeratedNothing` (`toyos-xhci/src/invariants.rs:22`, `:46`).
- **VERIFIED — guest-driven resolution check and residual.** `gpu_set_resolution` is registered and dispatched (`tests/toyos.rs:204`, `:12659`); current duration data records `gpu_set_resolution 8610 shards=12`. The follow-up `issues/kernel/a-mode-change-writes-the-registry-twice.md` remains open, as promised.
- **VERIFIED — tearing was not falsely closed.** `Window::present` delegates to full-rectangle `present_damage` (`userland/window/src/lib.rs:602`), while the tearing issue remains and records the missing sub-region comparison.
- **UNVERIFIABLE — the QMP/guest mutation outputs and comment-count instruments.** The present source supports the mechanisms and corrections, but not the historical run transcripts.

## `7ac1071332ea` — #350, dated hosted-QEMU archive

- **VERIFIED — the pin is current and mechanically gated.** `.github/apt-snapshot:11` is `20260831T000000Z`; `.github/qemu-version:8` is `11.1.0`; the five current hosted install sites name the same snapshot (`.github/workflows/ci.yml:184`, `:428`, `:544`, `.github/workflows/gate-a.yml:87`, and `.github/workflows/probe-green.yml:68`), and `src/ci.rs:181-249` checks the declaration against installing workflows.
- **VERIFIED — the self-hosted image remains on the declared QEMU.** `.github/runner/Dockerfile:21` asserts version `11.1.0`.
- **UNVERIFIABLE — Debian snapshot package content and the two external run IDs.** The repository declares that the snapshot carried `1:11.1.0+ds-2`, but the package index is not committed and this audit did not use the network.

```text
.github/apt-snapshot:11:20260831T000000Z
.github/qemu-version:8:11.1.0
```

## `8896a9befa1b` — #345, kernel and panic-path bundle A

- **VERIFIED — early panic polling and capture latch.** `screen_early_panic` uses `screendump_until("EARLY PANIC:")` (`tests/toyos.rs:3960-3977`); `CaptureLatch` remains the shared primitive (`kernel/src/drivers/panic_console/latch.rs:14`, imported by `kernel-loom/tests/panic_capture.rs:11`). The later reader/writer exclusion in #348 fixes a different overlap and does not falsify the one-writer claim.
- **VERIFIED — TLB census and unclaimed vectors.** `arch::tlb::shootdown` takes an `Origin` and records per-origin issues (`kernel/src/arch/tlb.rs:23-50`); unfilled IDT entries install `unclaimed_entry` (`kernel/src/arch/idt/mod.rs:428`) and the IRQ census includes `unclaimed` (`kernel/src/irq_census.rs:50`).
- **VERIFIED — last-slot wide BAR refusal.** `decode(MAX_INDEX, TYPE_64)` returns `WideAtLastIndex` and host tests assert both encodings (`toyos-pci/src/bar.rs:173`, `:321-324`).
- **VERIFIED — remaining named mechanisms exist.** `ftruncate_flush_race` is registered (`tests/toyos.rs:991`), the i8042 arm edge is staged (`kernel/src/drivers/i8042/mod.rs:1352`), `driver_wait_refused` is registered (`tests/toyos.rs:615`), and the inbox event completion helper is private with every event routed through `Source::wake` (`kernel/src/inbox.rs:713`).
- **VERIFIED — exclusions stayed open.** The hashmap collision and two-ledger mapping issues were not removed by this merge.
- **UNVERIFIABLE — all guest mutations, model exploration counts, and build outputs.** Their code and gates remain, but the numerical transcripts live only in the merge message.

## Conclusion

The landed-record corpus is substantially self-correcting: PRs #347, #348, #351, and #353 include review corrections that narrow earlier overclaims, and the current tree reflects those corrections. The one material record failure is localized: PR #357 simultaneously files evidence that a stopped input path explains both locale sightings and retires both sightings against ordinary queue pacing. That produces the same two FALSE rows reported independently in `RETIREMENTS.md`.
