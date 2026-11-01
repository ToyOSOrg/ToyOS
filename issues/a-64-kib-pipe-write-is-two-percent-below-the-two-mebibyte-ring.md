---
status: open
kind: defect
opened: 2026-10-28
---

# A 64 KiB pipe write is 2 % below the 2 MiB ring

Pipe bytes live in 4 KiB frames (`kernel/pure/pipe.rs`, `kernel/src/pipe.rs`)
instead of one physically contiguous 2 MiB page. On the T14 (NVMe boot, 4188
MHz), three interleaved boot pairs of the frame head against the ring base
read `pipe.throughput.65536` p50 7596 / 7547 / 7521 MB/s against 7616 / 7806
/ 7811. Every pair is lower (rank test p = 0.05, the floor for 3 against 3), a
mean of −2.4 %, with base's own boot-to-boot spread 2.5 %. 4096 and 16384 are
at base, and `pingpong.pipe.*` p50 is lower (better) on the head in every pair.

It is not per-piece overhead: a 16 KiB write crosses as many user-page
pieces per KiB and is at base, so a frame-aware walk that saves instructions
per piece is not expected to recover it.

The unmeasured candidate: a 64 KiB write spans 16 or 17 scattered frames
where the ring was physically contiguous, which a prefetcher that follows into
the adjacent page could use. The earlier image with physically adjacent frames
(PR #1074's evidence r6, diag4) was read while instructions dominated the gap,
on the old boot path at 3789 MHz, so it does not settle this.

Exit: a PMU session (the measurement-only counters of evidence r6) on the
landed head at 65536 beside base and beside a physically-adjacent-frames
image, then a fix or a folded note at `kernel/src/pipe.rs` stating the
measured cost.

Evidence: PR #1074, evidence r7 (the final A/B) and r6 (the PMU session).
