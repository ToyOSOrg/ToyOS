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

`testcases` is such a boot: `test_rs_counters_metal` dumps every CPU's
counters and the boot writes about forty files.

## Measured

The T14 at `9ded9075c`, where `acpi_server_events` rode `testcases` with
`test_rs_acpi_hold` its last job. The readback carries no record between
34 s and 47 s on the log's clock, and fifteen lines of this shape, the first
at 47.238 s:

```
logkeeper: /log holds more than 16 logs, so /log/2026-10-08-140646_0012.log was deleted
```

`acpiserver` armed at 12.851 s and writes its count thirty seconds on, at
about 42.9 s, inside the hole. The row red:
`the server logged 1 first sighting(s) and None for counts`. The server was
serving throughout: cpu0's census reads `userdev=48` at the boot's end, as it
does on the quiet `testcases-hold` boot of `779330742` the same day, whose
count line is there at 42.579 s (`28 SCIs; embedded controller queries taken:
0x4f x14`). The same hole, 36 s to 47 s, is in `testcases`' readback of
2026-10-07.

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
