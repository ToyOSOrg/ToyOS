---
status: open
kind: defect
opened: 2026-08-01
---

# An idle soundd keeps the DMA engine and the codec voice open

Stopping the device voice while keeping the periodic timer wake recovers the DMA
engine and the codec — the battery-relevant hardware — and gives up only the wake
itself. Resume still works unchanged, because soundd keeps writing signal bytes,
so it does not need the missing client→soundd message that
`cpal-backend-hardcodes-the-format` is waiting on.
