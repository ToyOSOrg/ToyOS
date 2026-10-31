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
- **AMD adapts AMD's Display Core.** It is pinned with a minimal patch set under the fork rules, runs in a userland driver behind its IOMMU domain, and keeps ToyOS's side and every parser of external input in Rust.
- **The driver registers a panic surface with the kernel.**
- The firmware's GOP stays the floor. Intel's first stages keep the firmware's mode and surface and add only power programming.

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

## Defaults the owner may overturn

- **Display audio** (HDMI/DP) is in, after modesetting. The driver powers the audio path and publishes ELD; `hda` keeps the codec.
- **Docks**: DP alt mode over USB-C is in. DisplayPort tunnelled over USB4/Thunderbolt is out of release 1, because it needs a connection manager no track holds.
- **Backlight**: the driver owns it and states it on `display-output`. Brightness keys reach the compositor as input, and the compositor asks the driver.

## Stages

1. **Intel watermarks and SAGV on the firmware's surface.**
   - Memory latencies and SAGV block time from pcode; WM levels 0-7 and transition by Vol 12's algorithm; SAGV masked when the plane cannot tolerate it.
   - Needs the claim's reserved-range mapping. The plane's GGTT entry points into the T14's RMRR (BDSM 0x9c800000).
   - What the T14's GOP leaves (a read-only scout, 2026-10-31):
     - one linear RGB8888 plane, 1920x1080, htotal 2080, about 60 Hz;
     - WM level 0 only, with levels 1-7 and the transition watermark disabled;
     - a correct linetime;
     - a 682-block allocation;
     - DC_STATE_EN 0, PSR1 and PSR2 off.
   - pcode on the T14 gives latencies 3, 54, 54, 54, 54, 73, 110, 115 µs after Vol 12's +3 µs rule, and an SAGV block time of 11 µs.
   - At that mode every level fits the GOP's allocation, so this stage writes no DBUF.
   - **Exit:** the T14's idle package with the panel lit, against Linux. Until NVMe APST lands, the NVMe caps the package at PC3, so that is the most this stage can show.
2. **Intel DMC and DC5/DC6** (Intel's signed payload, pinned by version and hash), and
3. **PSR on the eDP panel**. These two are measured together. DC5 with a pipe running needs PSR active (Vol 12, "Display C5 and C6"), so stage 2 alone saves nothing with the panel lit. PSR's first-frame exit latency goes to the owner in absolute numbers before PSR lands.
4. **`display-output`.** The compositor moves onto it, with virtio-gpu and the framebuffer as drivers. The panic surface lands here.
5. **Intel modesetting**: combo-PHY DDIs, DPLLs and CDCLK, DP link training with DSC, HDMI 2.0 with SCDC, EDID over AUX and GMBUS, several outputs.
6. **Intel Type-C DP alt mode**: FIA/IOM state, TCCOLD, the Dekel PHY, hotplug.
7. **Intel high refresh and VRR.**
8. **AMD**: the Display Core adapter, to the same protocol.
9. **Display audio.**

## Tests

- **Host:** a modelled engine per vendor (registers with their double-buffer arm points, DBUF and watermark limits, a pcode mailbox, an AUX/DPCD sink). It checks the PRM's worked examples, mode validation, and EDID/DPCD parsing of malformed input.
- **QEMU:** the protocol end to end over virtio-gpu, for order and contents.
- **Metal:** batched per boot.
  - The internal panel needs nothing attached.
  - External outputs need monitors on each bench's ports.
  - A capture device is the independent oracle for an external output.
