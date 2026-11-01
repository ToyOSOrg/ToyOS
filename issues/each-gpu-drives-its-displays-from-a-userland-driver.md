---
status: assigned
kind: track
opened: 2026-10-31
---

# Each GPU drives its displays from a userland driver

Held by the display task. Owner rulings, 2026-10-31:

- The display engine is release-1 scope on both benches: the internal panel and external outputs, modesetting, hotplug, EDID, refresh rates including high refresh and VRR where the sink offers them, and power (watermarks, SAGV, DMC and DC states, PSR). Video and render acceleration are out.
- **Several outputs, extended or mirrored**, each with its own mode and refresh rate, with hotplug.
- **Intel is a Rust driver from Intel's published PRMs** (IHD-OS-TGL-Vol 2c and Vol 12, 12.21), cited at each register.
- **AMD adapts AMD's Display Core**, under root `CLAUDE.md`'s vendor-component rule (#1124).
- **The driver registers a panic surface with the kernel.**

Earlier owner decisions this track builds on:

- 2026-09-01: "GOP is the floor, and the mode is the firmware's" (`issues/gop-is-not-the-path-every-boot-takes.md`).
- 2026-10-31: the approved power design keeps the firmware's mode and surface for Intel's power stages, adding only power programming.

The rest of this file is the display task's design, not a ruling.

## Ownership

One process per GPU function, an ordinary device claim (`drives = [...]`), brings its outputs to the compositor on a `display-output` port. This is how `hda` brings outputs to soundserver and a NIC driver brings a link to netstack.

| | owner |
| --- | --- |
| Registers, interrupts, firmware payloads, clocks, PHYs, link training, watermarks, DBUF, SAGV, DC states, PSR, the backlight | the driver |
| Outputs: connector, EDID bytes, modes, physical size, VRR range, backlight range; hotplug as a new output list | the driver states, the compositor reads |
| Configuration: outputs lit, mode, position, mirror or extend | the compositor chooses; the driver validates one atomic commit (with a test-only form) and refuses by name |
| Scan-out buffers | the driver allocates and lends them, because it alone knows alignment, stride, tiling, placement and memory type |
| Frame content | the compositor, composed in RAM and blitted into a lent buffer |
| Present | the compositor sends `present(buffer, damage, target time)`; the driver answers `presented(sequence, vblank time, refresh interval)` and the buffer's release. Damage drives PSR2 selective update; target time drives VRR. |
| Cursor | the driver's cursor plane |
| Blanking on idle or lid | the compositor decides, the driver does it |

Wire types and state live in `lib/display-output`, host-tested. The compositor is the server and never blocks on a driver (`system/CLAUDE.md`).

The compositor drives no device of its own. virtio-gpu becomes a driver process over `toyos-virtio-gpu`. The kernel's framebuffer claim is served by a driver with one output, its one mode and one buffer that is never flipped, so every machine reaches the compositor through the same port.

The kernel keeps only generic mechanisms:

- A claim keeps its function's firmware-reserved ranges mapped in its domain from attach on: VT-d RMRRs, and AMD-Vi's unity ranges.
- The panic surface: a linear buffer the driver lent, and the bounded register writes in its own BAR that make it visible.

## AMD's adapter boundary

- **DC is vendored unmodified where possible.** Its patch set covers only the OS interface DC is written against (`dm_services`, `os_types.h`) and its build. It is pinned with a hash under the fork rules, and its licence goes through the licence evaluator and `NOTICE`.
- **ToyOS's side is Rust and written clean-room.** Linux's `amdgpu_dm` and the DRM helpers are another implementation of that side, so a reader derives facts from them and a separate writer implements. DC itself is used as published, not read as a specification.
- **Where the line sits for external input.** Every byte a sink sends reaches a Rust parser first wherever DC takes it from its OS side:
  - EDID: DC receives capabilities ToyOS's side derives in Rust (`toyos-edid`), never raw bytes.
  - DPCD: read through the AUX path ToyOS's side carries, where DC routes AUX through its OS interface. Whether DC does so for every DPCD read is a fact the reader establishes first.
  - HDCP: DC's HDCP module is not built, because DRM is out of release 1.
- **What DC still parses itself is a recorded compromise.** That covers any DPCD and DSC capability DC reads straight off its AUX hardware. It stays contained by the driver's process and IOMMU domain, and it is filed in `issues/` with its exit: a Rust validator on that path, or upstream DC taking it from its OS side.

## Defaults the owner may overturn

- **Display audio** (HDMI/DP) is in, after modesetting. The driver powers the audio path and publishes ELD; `hda` keeps the codec.
- **Docks**: DP alt mode over USB-C is in. DisplayPort tunnelled over USB4/Thunderbolt is out of release 1, because it needs a connection manager no track holds.
- **Backlight**: the driver owns it and states it on `display-output`. Brightness keys reach the compositor as input, and the compositor asks the driver.

## Stages

1. **Intel watermarks and SAGV on the firmware's surface.**
   - Memory latencies and SAGV block time from pcode; WM levels 0-7 by Vol 12's algorithm.
   - The transition watermark stays disabled.
   - A plane that cannot tolerate SAGV is refused and left as found, because masking QGV points needs the bandwidth calculation the modesetting stage brings.
   - Needs the claim's reserved-range mapping. The plane's GGTT entry points into the T14's RMRR (BDSM 0x9c800000).
   - What the T14's GOP leaves (a read-only scout, 2026-10-31):
     - one linear RGB8888 plane, 1920x1080, htotal 2080, about 60 Hz;
     - WM level 0 only, with levels 1-7 and the transition watermark disabled;
     - a correct linetime;
     - a 682-block allocation (PLANE_BUF_CFG 0..681; Buffer End is inclusive, as Vol 12 "Display Buffer Allocation" gives 0-159 as 0x009F0000);
     - DC_STATE_EN 0, PSR1 and PSR2 off.
   - pcode on the T14 gives latencies 3, 54, 54, 54, 54, 73, 110, 115 µs after Vol 12's +3 µs rule, and an SAGV block time of 11 µs.
   - At that mode every level fits the GOP's allocation, so this stage writes no DBUF.
   - **Exit:** on the T14 with the panel lit, `osbench energy`'s idle row reads PC2 at most 400 ‰ and PC3 at least 600 ‰.
     - That is the residency stopping the plane's fetches altogether gave (pr-1100 r7: 338 ‰ and 656 ‰). PC3 is the most this stage can show until NVMe APST lands, because the NVMe caps the package there.
     - The power figure against Linux's 1622 mW idle is judged once APST lands.
2. **Intel DMC and DC5/DC6.** Intel's package, pinned by version and hash (`NOTICE`), with its main program loaded through the iGPU's own registers.
   - PG3-PG5 are powered down, since the GOP leaves PG1-PG4 requested. PG1 holds pipe A, transcoder A with its PSR, and DDI A-C. AUDIO_PIN_BUF_CTL's enable is cleared after PG3 (Vol 12 "Audio Bios Programming Sequence"). Then DC6 is allowed.
   - DC6 is allowed only with the panel on DDI A, since Vol 12 says only port A's PHY programming survives it.
   - With the pipe lit, the engine enters DC5 only while PSR is active (Vol 12, "Display C5 and C6"), so this stage alone saves little.
   - What it can show alone is the program held in storage, PG3-PG5 off, and the scan-out and `hda` unharmed. `hda`'s display codec is in PG3.
3. **Present and damage.** The `display-output` piece PSR needs is the compositor telling the driver which frame changed, and where. Otherwise PSR1 sees a CPU write only through FBC host tracking of a graphics-aperture fence (Vol 12 "CPU Host Front Buffer Tracking", "on projects that support that"). Whether that reaches the GOP's surface is unverified. This is ordered before PSR, rather than an interim signal. Its shape goes to the orchestrator before it is built, because it crosses the compositor.
4. **PSR on the eDP panel**, measured together with stage 2. PSR's first-frame exit latency goes to the owner in absolute numbers before PSR lands.
5. **The rest of `display-output`.** virtio-gpu and the framebuffer become drivers. The panic surface lands here.
6. **Intel modesetting**: combo-PHY DDIs, DPLLs and CDCLK, DP link training with DSC, HDMI 2.0 with SCDC, EDID over AUX and GMBUS, several outputs.
7. **Intel Type-C DP alt mode**: FIA/IOM state, TCCOLD, the Dekel PHY, hotplug.
8. **Intel high refresh and VRR.**
9. **AMD**: the Display Core adapter, to the same protocol.
10. **Display audio.**
    - PG3 holds `hda`'s display codec, and since stage 2 `tgl-display` powers it down on its own. Display audio needs `hda` to ask the display driver for PG3.
    - Vol 12 "Audio Programming Sequence for Link Wakeup" orders the iDisp link: the audio driver stops it (LCTL1.SPA = 0, then CPA = 0) before AUDIO_PIN_BUF_CTL is cleared. Today only the buffer is cleared, after PG3, so the two drivers must coordinate here.

## Tests

- **Host:** a modelled engine per vendor (registers with their double-buffer arm points, DBUF and watermark limits, a pcode mailbox, an AUX/DPCD sink). It checks the PRM's worked examples, mode validation, and EDID/DPCD parsing of malformed input.
- **QEMU:** the protocol end to end over virtio-gpu, for order and contents.
- **Metal:** batched per boot.
  - The internal panel needs nothing attached.
  - External outputs need monitors on each bench's ports.
  - A capture device is the independent oracle for an external output.
