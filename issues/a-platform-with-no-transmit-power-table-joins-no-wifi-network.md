---
status: open
kind: defect
opened: 2026-10-12
---

# A platform with no transmit power table joins no Wi-Fi network

The accepted Wi-Fi design (#876 §4) sets each channel's transmit limit to the
tighter of two values:

- the platform's per-device power table, which its ACPI namespace carries
  under the radio's device;
- the access point's country element.

The design gives no limit at all where the platform has no table. There,
`ax2xx` joins no network and says so: "no transmit limits for this
platform". Its passive scan still runs.

What such a platform may transmit at is unknown. So is what the firmware
enforces where the platform gives no limit (#876 §11). From W6's landing, a
machine whose firmware carries no table has no Wi-Fi.

Owner: W6 of `issues/there-is-no-wifi.md`.

**Exit**:
- the specification or a measurement says what the firmware enforces and
  what such a platform may transmit at;
- the owner rules on it;
- a platform with no table joins under that ruling, with a host test of the
  limit it uses.
