---
status: open
kind: tooling
opened: 2026-10-09
---

# A shared member's allowance sets the kernel's deadline at several times the work, and no reading holds it

`toyos_tco::RUST_MEMBER_MS` and `toyos_tco::C_MEMBER_MS` are what one member
of a metal shared boot adds to its list's bound (`toyos_tco::list_bound_ms`).
The kernel's `boot-deadline=` is twice that bound and its hard-lockup bound is
half the deadline, so the two constants set how long a failed shared boot holds
the T14. They are 300 and 100 ms by the owner's ruling, not by a derivation:
nothing ties them to what the machine measures, and nothing reds when the
measurement moves.

## Readings

The T14, three boots at each of two heads; the judge's own lines. `eff8b20ee`
was armed with the 860 and 260 ms the allowances were then, `d6d008e88` with
300 and 100:

| boot | members | head | ms a member | the list's last record | allowance now | factor |
|---|---|---|---|---|---|---|
| `shared` | 88 | `eff8b20ee` | 80 | 8 264 ms | 300 | 3.7 |
| | | `d6d008e88` | 81 | 8 308 ms | 300 | 3.7 |
| `ccorpus` | 137 | `eff8b20ee` | 12 | 2 864 ms | 100 | 8.3 |
| | | `d6d008e88` | 15 | 3 223 ms | 100 | 6.7 |
| `shared-debug` | 7 | `eff8b20ee` | 32 | 1 380 ms | 300 | 9.4 |
| | | `d6d008e88` | 32 | 1 389 ms | 300 | 9.4 |

Each last record counts from the kernel's zero, so it holds about 1.2 s of
boot before the first job.

**The mean hides the member.** From each member's own start and end records in
the two kernel logs:

| boot | head | the members' sum | members past their allowance | their sum | the slowest |
|---|---|---|---|---|---|
| `shared` | `eff8b20ee` | 7 068 ms | 5 of 88 | 5 548 ms | `abuse_mmap_regions`, 2 875 ms |
| | `d6d008e88` | 7 115 ms | 5 of 88 | 5 616 ms | `abuse_mmap_regions`, 2 955 ms |
| `ccorpus` | `eff8b20ee` | 1 667 ms | 1 of 137 | 1 141 ms | `124_atomic_counter`, 1 141 ms |
| | `d6d008e88` | 2 031 ms | 1 of 137 | 1 517 ms | `124_atomic_counter`, 1 517 ms |

The five on `shared` at `d6d008e88` are `abuse_mmap_regions` 2 955 ms,
`fs_cache_eviction` 1 066, `abuse_thread_table` 787, `mutual_kill` 405 and
`allocator_stress` 403. The slowest Rust member takes nearly ten times its allowance
and the slowest C case fifteen times, and that case moved by a third between
two boots of one job list.

**The bound holds on the list's sum, not per member.** What the members add
without the 60 000 ms base, 26 400 ms on `shared` and 13 700 ms on `ccorpus`,
covers the 7 115 and 2 031 ms they took. It does not hold by construction: a
list made only of members as slow as `abuse_mmap_regions` passes its bound at
the 23rd.

## The price, on failure only

What `--metal --list` and `metal::return_secs` give at 300 and 100 ms, against
the 60 000 ms list bound and 120 000 ms deadline of a boot no member rides:

| boot | a hung member, and a CPU that takes no interrupt, hold the machine | a wedged kernel | a boot that never returns is waited |
|---|---|---|---|
| no member | 60.0 s | 120.0 s | 420 s |
| `shared-debug` | 62.1 s | 124.2 s | 425 s |
| `ccorpus` | 73.7 s | 147.4 s | 448 s |
| `shared` | 86.4 s | 172.8 s | 473 s |

**It grows with every member and has no ceiling**: 0.6 s of deadline a Rust
member and 0.2 s a C case. The 88 Rust members and 137 C cases now ride
`testcases` behind its rows' jobs, and arm a list bound of 100 100 ms, a
deadline of 200 200 ms and a hard-lockup bound of 100 100 ms, waited 501 s. A
clamp is no answer: a ceiling under the derived bound ends a healthy list.

## The merged list, summed from the boots it was

No machine has run the merged `testcases`. Each part between its own markers,
over three readings of the boot that carried it: `testcases` at `9e70cd2e3`,
`473efea22` and `accbd79dd`, `shared` and `ccorpus` at `eff8b20ee`,
`d6d008e88` and `49e12f23b`.

| part | least | most | its share of the bound |
|---|---|---|---|
| the boot, to its first job | 1 190 ms | 1 196 ms | |
| the rows' ten jobs | 41 913 ms | 41 983 ms | 60 000 ms |
| the 88 Rust members | 7 068 ms | 7 115 ms | 26 400 ms |
| the 137 C cases | 1 667 ms | 2 183 ms | 13 700 ms |
| the list's last record | 51 838 ms | 52 477 ms | 100 100 ms |

**Half the bound is 50 050 ms, and the sum is 1.8 to 2.4 s past it**, by the
rows' jobs: they take seven tenths of the base no allowance widens,
`counters_metal` alone 32.7 s of it, where the members take under a quarter of
what they add. The exit's second line reads the whole list against half its
bound, so a boot whose members are well inside their share does not meet it.

**A late expiry adds to the longer bound.**
`issues/a-120000-ms-boot-deadline-fired-132859-ms-late-on-the-t14.md` is open:
that deadline was reached at 252 859 ms, and the same lateness on `shared` is
305 659 ms. The wait moves with the deadline, so what is left of it after such
an expiry is the same 167 s; the machine is held 52.8 s longer.

## Owner

The metal suite: `tests/common/metal.rs`, which derives the bounds, and
`toyos-tco`, which declares the allowances.

## Exit

Both, on the T14:

- Each allowance is derived at its declaration from what the T14 measures of a
  list's total over its members, by one stated factor: a factor over a
  per-member mean says nothing of a list of slow members. And the judge reds a
  shared boot whose list's last record came past half its bound, which today
  is a line a reader reads.
- The boot that carries `shared`, `ccorpus` and `testcases` together reads its
  list's last record within half its list bound, with the pair read from the
  kernel's own `boot deadline:` and `hard lockup:` lines.
