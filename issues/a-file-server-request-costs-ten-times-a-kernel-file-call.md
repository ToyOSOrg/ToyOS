---
status: open
kind: defect
opened: 2026-10-28
---

# A file server request costs ten times a kernel file call

Every file operation on DATA is a request to the DATA file server process.
On the AMD bench (2026-10-28, `osbench persist` at PR #1097's c8f3fbfb0,
DATA on its NVMe) that round trip sets the cached rows, against the bench's
Linux page cache on the same NVMe and ToyOS's own kernel `/tmp`:

| row, p50 | ToyOS DATA | ToyOS `/tmp` | Linux home |
|---|---|---|---|
| `read_4k_at` (seek and 4 KiB read) | 16,903 ns | 1,323 ns | 1,463–2,375 ns |
| `stat` | 10,530 ns | 2,615 ns | 1,453–2,794 ns |
| `open_close` | 18,686 ns | 6,072 ns | 3,056–4,400 ns |
| `rename` | 59,154 ns | 6,082 ns | 9,308–10,895 ns |
| `seq_read.1048576` | 6,401 MB/s | 7,868 MB/s | 11,295–11,403 MB/s |

The disk adds nothing to these. The same rows on 2026-10-24's in-memory DATA
read 16,822, 10,450, 14,178 and 58,002 ns and 6,080 MB/s. The `/tmp` column
is that boot's kernel tmpfs (the orchestrator's full-final session; both
tables are in PR #1097's evidence). What the round trip spends its time on,
on metal, is unmeasured. `issues/a-cached-read-from-a-file-server-is-measured-only-under-tcg.md`
splits it under TCG.

**Exit**: the cached `fs.disk.*` rows within Linux's on both benches, or
each row's remaining difference measured to its parts and justified, in a
labelled current-state comparison.

**Owner**: the file server (`system/fileserver/`) and its transport
(`sdk/toyos/src/fs.rs`).
