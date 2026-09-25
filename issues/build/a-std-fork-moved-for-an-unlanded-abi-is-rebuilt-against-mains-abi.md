---
status: open
kind: tooling
opened: 2026-09-25
---

# A std fork moved for an unlanded ABI is rebuilt against main's ABI, and the shared toolchain is left without its `bin/`

The primary's `rust/` was at `3f0bda14850` ("toyos: a spawned child starts in
the directory the spawn states"), which is `wt/toyos-spawncwd`'s fork commit;
main pins `71911815325`. A worktree whose `toyos-abi` and `toyos` are main's
then ran `cargo run -- --build-only`. `src/toolchain.rs` found the recorded
sysroot witness equal to its own and the std fork stale, and took the
"rebuild std for a std fork that has moved" branch, whose header says it
"takes nothing from any of them". It compiled that std against main's
`toyos-abi` and failed:

```
error[E0560]: struct `SpawnArgs` has no field named `cwd_ptr`
error[E0560]: struct `SpawnArgs` has no field named `cwd_len`
error: could not compile `std` (lib) due to 2 previous errors
thread 'main' panicked at src/toolchain.rs:1583:5:
```

Afterwards `rust/build/aarch64-apple-darwin/stage2/` held only `lib/`, and
`rustc -vV` in `userland/` answered "'rustc' is not installed for the custom
toolchain 'toyos'". The lock log shows a second process (pid 31197) holding
the same step just before, so the step had already been attempted once.

The branch assumes a moved fork is main's fork moving. A fork moved to
an unlanded ABI branch's commit needs that branch's `toyos-abi`, so every
worktree identical to main fails the rebuild and takes the toolchain down with
it. Before this, such a worktree got the `Wait` refusal instead.

**Exit condition**: a worktree whose sysroot sources are main's refuses by name,
without touching `stage2`, when the primary's `rust/` is not at the commit
main pins, and a test covers that case.
