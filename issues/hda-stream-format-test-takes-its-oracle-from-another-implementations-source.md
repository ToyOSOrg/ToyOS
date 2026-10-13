---
status: open
kind: defect
opened: 2026-10-13
---

# The HDA stream-format test takes its oracle from another implementation's source

`lib/hda/src/stream.rs`'s `every_rate_linux_tabulates_encodes_to_the_number_linux_tabulates`
holds the rate half of `SDnFMT` to a table copied from Linux's
`sound/hda/hdac_device.c`, and its doc names the file. Root `CLAUDE.md` (#902)
reads another implementation's source only clean-room, and `reviewer.md`'s
**Fit** refuses a test whose oracle is another implementation's source.

Found while #877 changed `lib/hda/src/config.rs` beside it; #877 does not touch
`stream.rs`.

Owner: `lib/hda`.

## Exit condition

The test's expected words are derived from the Intel High Definition Audio
specification's stream-format table (base, multiplier and divisor bits), cited
at the site, and no source, test or comment under `lib/hda` names Linux.
