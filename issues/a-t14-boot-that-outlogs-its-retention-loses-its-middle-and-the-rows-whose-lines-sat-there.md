---
status: open
kind: tooling
opened: 2026-10-08
---

# A T14 boot that outlogs its retention loses its middle, and the rows whose lines sat there

`logkeeper` keeps sixteen files of a megabyte each (`MAX_LOG_FILES`,
`userland/logkeeper/src/store.rs`) and deletes the writing boot's own middle
past that, by design: one boot's flood costs its own middle and nobody's
start. A metal judge reads what came back, so a line a flooding boot wrote in
its middle is a line the judge reports missing, and nothing in the harness
says the readback has a hole.

`testcases` is such a boot, and the flood was the kernel's, not the job's:
`test_rs_counters_metal`'s `loaded` phase spawns a child per CPU in a loop for
twenty seconds, and the kernel wrote fourteen records for each.

## What a child cost, and what it costs now

The readback of `testcases` at `809c33c0c`, 16,645,533 bytes of which the
parts that survived hold 8,304 of those children: per child 1,989 bytes in 14
records. Eight `irq: cpuN` lines, a census of the whole machine at every
process's end, were 1,288 of them; `syscalls:` 110, `memory:` 85, `exit:` 95,
`ELF:` 107 and the two `spawn:` records 304.

A process's end now writes one record, its `exit:`, carrying what `syscalls:`
and `memory:` said; a spawn writes one, its `spawn:`; and the machine's census
is taken once, where the machine ends (`kernel/src/census.rs`).

`testcases` on the T14 at `f2b337afd`, the first head with two records a
process: 8,730,435 bytes, no part deleted, 22,091 `spawn:` and 22,081 `exit: …
pid=` records, 390 bytes a child against 1,989.

**The margin is a factor, not a bound.** 8.73 MB is 52% of the sixteen
megabytes kept, 98.7% of it still that one job's `spawn:` and `exit:` records,
and the phase spawns a child per CPU for twenty seconds: about 1.9 times the
children, a sixteen-CPU machine or a faster one, outlogs the retention again.
**The harness is as silent about a hole as it was**: nothing reds a readback
whose own boot deleted a part of its log.

## Measured

The T14 at `9ded9075c`, where `acpi_server_events` rode `testcases` with
`test_rs_acpi_hold` its last job. On the log's clock the readback's last
record before the hole is at 34.875 s and its first after it at 47.220 s. The
boot went from part `_0026` to `_0041`; what came back is its first part and
its fifteen newest, and fifteen deletion lines survive in them, the earlier
ones having gone with the parts that held them. The first that survives, at
47.238 s:

```
logkeeper: /log holds more than 16 logs, so /log/2026-10-08-140646_0012.log was deleted
```

`acpiserver` armed at 12.851 s and writes its count thirty seconds on, at
about 42.9 s, inside the hole. The row red:
`the server logged 1 first sighting(s) and None for counts`. The server was
serving throughout: cpu0's census reads `userdev=48` at the boot's end, as it
does on the quiet `testcases-hold` boot of `779330742` the same day, whose
count line is there at 42.579 s (`28 SCIs; embedded controller queries taken:
0x4f x14`). `testcases`' readback of 2026-10-07 has the same hole: no record
in seconds 36 to 47.

So `acpi_server_events` and `acpi_tables_loaded` keep `testcases-hold`, a boot
whose log is whole.

## Owner

The harness's author, with the work that makes the T14's boots fewer and
fuller: a boot that carries every shipping-kernel member logs more than
`testcases` does.

## Exit condition

No boot of the metal profile writes past `logkeeper`'s retention, or a
readback whose own boot deleted a part of its log reds by name before any row
is judged on it; and `acpi_server_events` passes on `testcases` on the T14.
