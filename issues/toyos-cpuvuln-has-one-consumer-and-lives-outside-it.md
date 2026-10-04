---
status: open
kind: defect
opened: 2026-10-04
---

# toyos-cpuvuln has one consumer and lives outside it

Since pull request #705 the kernel depends on `toyos-cpuvuln` for which
counters a CPU has (`toyos_cpuvuln::counters`), and nothing else depends on
it: a crate with one consumer, which `.claude/agents/reviewer.md`'s Fit puts
under that consumer as #703 put every other, still at the repository root as
a host workspace member. #705 left it
there because the move was outside its brief.

Owner: the orchestrator, who briefs the move. **Exit**: the crate lives under
`kernel/`, every path naming `toyos-cpuvuln/` moved with it (the root
`Cargo.toml`'s members, `kernel/Cargo.toml`, and the issues citing its
fixtures), and `src/hostws.rs` green; or a second consumer depends on it.
