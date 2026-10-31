---
status: open
kind: defect
opened: 2026-10-28
---

# A file server request costs three to twelve times a kernel file call

Every file operation on DATA is a request to the DATA file server process,
but for a seek from the start or the current offset of a file no other
process shares, which the client keeps (#1110). On the AMD bench (2026-10-28, `osbench` at PR #1097's c8f3fbfb0, DATA on its
NVMe), that boot's cached rows on DATA against its kernel `/tmp`, and the
bench's Linux page cache on the same NVMe:

| row, p50 | ToyOS DATA | ToyOS `/tmp` | DATA / `/tmp` | Linux home |
|---|---|---|---|---|
| `read_4k_at` (seek and 4 KiB read) | 16,903 ns | 1,413 ns | 12.0x | 1,463–2,375 ns |
| `stat` | 10,530 ns | 2,596 ns | 4.1x | 1,453–2,794 ns |
| `open_close` | 18,686 ns | 6,091 ns | 3.1x | 3,056–4,400 ns |
| `rename` | 59,154 ns | 6,242 ns | 9.5x | 9,308–10,895 ns |
| `seq_read.1048576` | 6,401 MB/s | 8,041 MB/s | | 11,295–11,403 MB/s |

Four of these rows the disk does not set. On 2026-10-24's in-memory DATA the
same rows read within about 5%: `read_4k_at` 16,822, `stat` 10,450 and
`rename` 58,002 ns, and `seq_read.1048576` 6,080 MB/s. `open_close` read
14,178 ns there, so it is 32% dearer on the NVMe, and what that costs is
unattributed. Both tables are in PR #1097's evidence. What the round trip
spends its time on, on metal, is unmeasured.
`issues/a-cached-read-from-a-file-server-is-measured-only-under-tcg.md`
splits it under TCG.

**#1110, the client's offset** (base b3dbfbfb0, head 1293be570, two pairs
per bench, p50 µs, [evidence](https://github.com/ToyOSOrg/evidence/tree/d08c7dceb4e9f4301f0faff8905b867c467d1226/s1-ab)):
`seek` is local, and `read_4k_at` and `write_4k_at` are one round trip.

| row | T14 | AMD |
|---|---|---|
| `seek` | 8.43 → 0.24, 8.31 → 0.24 | 7.84 → 0.19, 9.79 → 0.20 |
| `read_4k_at` | 18.17 → 9.96, 17.82 → 10.13 | 17.10 → 9.48, 20.99 → 11.36 |
| `write_4k_at` | 18.22 → 10.05, 17.98 → 10.21 | 17.69 → 10.06, 21.64 → 11.98 |

**Exit**: the cached `fs.disk.*` rows within Linux's on both benches, or
each row's remaining difference measured to its parts and justified, in a
labelled current-state comparison.

**Owner**: the file server (`system/fileserver/`) and its transport
(`sdk/toyos/src/fs.rs`).
