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
| `testcases` | 100.1 s | 200.2 s | 501 s |

**It grows with every member and has no ceiling**: 0.6 s of deadline a Rust
member and 0.2 s a C case. The 88 Rust members and 137 C cases the readings
above took on `shared` and `ccorpus` ride `testcases` behind its rows' jobs,
and those two boots are gone. A clamp is no answer: a ceiling under the
derived bound ends a healthy list.

## The merged list

The T14 ran the merged `testcases` once, at `2c7e1be1a`, armed 200 200 and
100 100 ms by the kernel's own lines; each part between its own markers, the
kernel's clock counted from its `Boot: complete (1135ms)` line. Beside it, the
sum of the three boots it was, three readings each: `testcases` at
`9e70cd2e3`, `473efea22` and `accbd79dd`, `shared` and `ccorpus` at
`eff8b20ee`, `d6d008e88` and `49e12f23b`.

| part | merged, `2c7e1be1a` | its parts apart | its share of the bound |
|---|---|---|---|
| the boot, to its first job | 1 191 ms | 1 190 to 1 196 ms | |
| the rows' ten jobs | 41 924 ms | 41 913 to 41 983 ms | 60 000 ms |
| the 225 members | 9 253 ms | 8 735 to 9 298 ms | 40 100 ms |
| the list's last record | 52 352 ms | 51 838 to 52 477 ms | 100 100 ms |

**The two shares are wide by different factors, and the rows' is the
tighter.** The members took 9 253 ms of the 40 100 ms they add: the
allowances stand at about 4.3 times their work. The rows' jobs ended
43 115 ms into the kernel's clock, of the 60 000 ms `toyos_tco::JOB_BOUND_MS`
gives them: about 1.4 times theirs, `counters_metal` alone 32.7 s of it. That
constant is the same for a boot of no job and for this boot of ten, and no
allowance widens it. The list's last record left 47.7 s of its bound, and
nothing reads that margin.

**A late expiry adds to the longer bound.**
`issues/a-120000-ms-boot-deadline-fired-132859-ms-late-on-the-t14.md` is open:
that deadline was reached at 252 859 ms, and the same lateness on `testcases`
is 333 059 ms. The wait moves with the deadline, so what is left of it after
such an expiry is 167 s there and 168 s here; the machine is held 80.2 s
longer.

## Owner

The metal suite: `tests/common/metal.rs`, which derives the bounds, and
`toyos-tco`, which declares the allowances.

## Exit

Three parts, each on the T14. None is built.

1. **The members.** Each allowance is derived at its declaration from the
   members' sum between their own markers, by one stated factor: a factor over
   a per-member mean says nothing of a list of slow members. And the judge
   reds a boot whose members' sum is past the share they add, which today is a
   line a reader reads.
2. **The rows' jobs.** What a boot's rows' jobs are given is derived the same
   way from what they take, where today it is one constant,
   `toyos_tco::JOB_BOUND_MS`, for a boot of no job and a boot of ten, one of
   them 32.7 s. And the judge reds a boot whose rows' jobs end past their
   share, which today nothing reads.
3. **`testcases` reads both inside their shares**, with the armed pair read
   from the kernel's own `boot deadline:` and `hard lockup:` lines and the
   margin from the list's last record to its list bound stated beside them.
