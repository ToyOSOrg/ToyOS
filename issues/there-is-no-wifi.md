---
status: open
kind: track
opened: 2026-08-03
---

# There is no Wi-Fi

Nothing 802.11 exists in the tree. What is to be built is #876's design,
accepted by the orchestrator in issuecomment-6125972534 with the seven
changes its body carries:

- `ax2xx`, one driver program per Intel AX2xx radio function: the AX210 on
  the T14, the AX200 on the AMD bench;
- `wlan`, a supplicant program apart from it, alone holding a network's
  credential;
- `wifi`, the network settings program;
- the ports between them: `link` (netstack to each NIC driver), `radio`
  (`ax2xx` to `wlan`) and `wifi` (`wlan` to settings programs);
- netstack running every link a driver brings.

The first version, W6, is an HT, VHT and HE station with block-ack both ways
that joins WPA2-Personal and WPA3-Personal networks with PMF. The design is
#876's body; this file repeats only what a stage needs. The specification
the driver is written from lives outside the tree, revision by revision, on
`ToyOSOrg/evidence` branch `pr-876`.

## Lines the owner drew

As the orchestrator recorded them on #876, quoted in its body's §0:

- **The clean room** (issuecomment-6125437246): "one reads source writes a
  doc. the other reads from the doc not the source."
  - A reader reads the references and the demo and writes the
    specification.
  - A writer writes the driver from it and the public standards, and never
    reads the references or the demo: "a sentence in the writer's brief and
    role, not machinery".
- **The references**: "reference code may only be used as clean room
  information. we want no notice for it. full speed from start." #876 reads
  full speed as HT, VHT and HE rates and block-ack in the first version.
- **The split** (issuecomment-6120490125, ruling 3): "Each card's driver is
  its own program, and netstack runs every link a driver brings, wired
  first, with failover. The LAN track's stage 3 (every card behind a link
  session) is a prerequisite, as the design reads it." The owner chose
  several interfaces over one picked by the manifest.
- **WPA3 and PMF in the first version** (ruling 2): "The crypto crate
  (`lib/crypto`, still being written outside the tree) gains SAE over P-256
  and AES-CMAC first."
- **The MSI-X table's page** (issuecomment-6121747502, Q2): "The page is
  handed over only where interrupt remapping verifies the source, and only
  once S1's T14 boots read compatibility format blocked. If they don't, that
  is filed as a kernel-wide defect."
- **6 GHz** (Q3): "right after the first version, as W9, once a 6 GHz access
  point is at the bench."
- **Saved networks** (Q4): "plain text whatever until the key store concept
  exists", held by the supplicant alone. This overrides ruling 1's
  "encrypted at rest", and stage P7, the key store, went with it.
- **Throughput is reported and never blocks**: the standing ruling's "Bench
  numbers are reported and never block", for a design the manufacturers back,
  as #876 §0 applies it to the driver.
- **The link hop's cost is measured and reported, and decides nothing by
  itself**: the split and `link` are this design's, so the standing ruling's
  "Review on merits" applies, "reviewed on correctness and simplicity like
  any code. A speed claim is measured and reported but does not decide by
  itself, unless the design's only purpose is speed" (#876 §0).
  `.claude/agents/implementer.md` carries both clauses.

## Stages

Each stage is its own design round. The orchestrator dispatches every stage
and owns each one this list gives no other owner.

- **Writers.** Every W stage's owner is the orchestrator, who briefs a
  writer for it: an implementer told never to read the references or the
  demo. The stage's design round is that writer's. Root `CLAUDE.md`'s
  clean-room rule is that sentence; no prompt carries another.
- **Reviews.** The orchestrator's brief for each W stage's review and design
  acceptance says that they quote no reference. A defect found through a
  reference reaches the writer only as a reader's revision of the
  specification.
- **The S and P stages** are no part of the driver and need neither role.
- **Metal rows.** Every stage with a row on the T14 or the AMD bench first
  needs `issues/a-metal-row-names-no-hardware-and-a-bench-without-it-reds.md`.

**S1, the T14's read-out and the compatibility-format A/B: done, both exits
met.** The orchestrator's scout, never landing, on `ToyOSOrg/evidence`
`pr-876`: the A/B and the PCI and DMAR read-out in `s1/`
(https://github.com/ToyOSOrg/evidence/blob/cd368b25b2259a76b80dc31f4219dac1c91d84dc/s1/judge.log),
and the ACPI read-out in `s1-r2/`
(https://github.com/ToyOSOrg/evidence/blob/abd63019cfc896d69b59a8acba29fedaec0b48cc/s1-r2/judge.log).
- **The A/B: compatibility format blocked.** Boot A left the MSI of the xHCI
  at 00:0d.0 in compatibility format with `IRE` set and `CFIS` clear: fault
  0x25 and no delivery. Boot B left it remapped: delivered, no fault. P1's
  Q2 condition and W5's S1 dependency are met.
- **The read-out.** The AX210 is 09:00.0, behind root port 00:1c.4: BAR 0
  alone, 16 KiB, mem64, with its MSI-X table at BAR 0 + 0x2000, its PBA at
  BAR 0 + 0x3000, and MSI with one vector. No DMAR unit's scope names it, so
  it is the INCLUDE_PCI_ALL unit's, and no reserved region names it. The
  ACPI read-out found the radio's device under its root port, with its names
  below it. The T14's NVMe keeps its MSI-X table at BAR 0 + 0x2000 too,
  which puts it in P1's T14 exit.
- **What S1 does not separate.** The T14 runs with `EIME` set, and VT-d
  §5.1.4 blocks compatibility format where `EIME` is set or `CFIS` is
  clear. Boot A read the block with both true on every unit, so S1 does not
  show `CFIS=0` alone blocking it, which exit 1 of
  `issues/qemu-passes-compatibility-format-interrupts.md` still asks. A VT-d
  machine the kernel leaves without `EIME` (an xAPIC machine, or a unit
  without `ECAP.EIM`: `kernel/src/arch/x86_64/vtd/mod.rs`) rests on `CFIS=0`
  alone.

**S2, the link hop's cost on the T14: done, reported under P4.** A scout,
never landing: two processes over `toyos::transport`, one entry per 1,514-byte frame, batches of
1, 8 and 32, both directions, each side copying each frame once. Owner: the
orchestrator. Depends on: nothing. Exit: cycles per frame and frames a
second on one CPU, reported in P4's design round. Beside them stands the
200,163 packets of 1,500 bytes a second each way that full rate needs:
HE-MCS 11, two streams, 160 MHz, 0.8 µs GI, a standard's bound, not a
measurement
(https://github.com/ToyOSOrg/evidence/blob/1e0625eaf0feb22df2a760976b7ac3a2858b4e2c/e4-ceiling.log).
By itself it decides nothing (the link hop's line above).

**The specification.** Owner: the orchestrator, who briefs its readers.
Depends on: nothing.
- **Exit, per W stage:** the revision that stage's brief names, committed
  to `pr-876`, carries every fact the stage needs. Each fact is a sentence
  of meaning with its source: a reference by name, a firmware container, a
  measurement with its run, or a standard's clause. The revision holds no
  code or pseudo-code of a reference.
- **Its sources.** Where Intel's published firmware-interface headers state
  a fact, the specification takes it from them and the bench checks that it
  works; another reference disagreeing does not reopen it. A fact those
  headers leave unsaid is taken from the other references. Only a fact those
  disagree on, or all of them leave unsaid, stays open until a reader's scout
  measures it. The scout runs on a branch kept off `origin`.

**P1, a BAR stays where firmware put it, in 4 KiB pages, its MSI-X table's
page included.** Owner: the orchestrator. Built: a claim hands each BAR over
in place, in the 4 KiB pages it covers, and withholds one whose pages another
function decodes in (`toyos_pci::pages`); nothing moves a BAR, and
`msix-exclusive-bar=on` is gone, so the NVMe guests drive QEMU's default
controller through its table in BAR 0.
- **Exit, owed on the benches:**
  - on the T14, `msix_table_read_in_place` reading the AX210's armed MSI-X
    entry back through its holder's mapping at BAR 0 + 0x2000, and
    `claim_reuses_its_remapping_entry` unchanged; the NVMe's table, also at
    BAR 0 + 0x2000, is the nvme driver's in every image (#1031), so no test
    claims it;
  - the T14's NVMe driven through that table: an Identify Controller admin
    command whose completion arrives through the in-place MSI-X entry, which
    touches none of the other system's data. Every image's nvme driver now
    brings it up, but no row judges that a completion arrived by interrupt:
    `admin_command` reaps again after each bounded wait, so a lost interrupt
    only slows it;
  - on the AMD bench, `amdvi_usbd_drives_its_own_entry`: `03:00.4`
    (which no test image's usbd row names, its MSI-X table in BAR 0, its only BAR, behind
    `00:08.1`, whose `0xd0000000..0xd0500000` bus 3 fills) claimed in place
    and driven by usbd through its own remapping entry, every unit filtering
    SMIs: the `usbd_spare`, `usbd_spare_fault`, `end_returns_claims` and
    `usbd_spare_vector` jobs pass, the aimed fault is recorded against the
    claim at its address, and the holder's rewrite of its entry to a vector
    not its own is refused, recorded, and absent from the stop's census of
    unclaimed vectors.
- **Its rows need no metal-row naming first:** `msix_table_read_in_place`
  reads whichever of its named functions the machine has and reds only on a
  machine with none, and `amdvi_usbd_drives_its_own_entry` is run on the AMD
  bench alone, by row filter.

**P2 and P3, AMD-Vi translation and interrupt remapping.** Owner: the
orchestrator's AMD track. Translation (PR #909) and remapping with the SMI
filter (PR #921) have landed. Depends on: P1, for the claim.
- **Exit:** three readings on the AMD bench:
  - the stick's xHCI and the NVMe working in their domains under
    translation;
  - the AX200's claim handed over with its own remapping table;
  - before the table's page is mapped there, a message naming no valid entry
    of the radio's own table, refused into the event log.
- Only W7 waits on them.

**P4, the LAN track's stage 3, whole.** Owner: the orchestrator; P4 is
stage 3 of `issues/the-lan-is-not-yet-production-grade.md`, whole.
- **Depends on:** the T14's outbound rows, which
  `issues/toyos-has-its-own-network-stack.md` owns; and S2, which its round
  reports.
- **Exit:** that stage's exit, a driver killed and restarted with the
  stack's connections intact (held in a guest) and an unlisted but matching
  NIC up with no configuration change; and with it:
  - the `link` protocol on `toyos::transport`, adding no transport primitive;
  - `Card` gone;
  - the supervisor binding drivers from a declared match table by vendor,
    device and class;
  - the network suite unchanged under QEMU;
  - a T14 row reading the I219 working, with nothing regressed;
  - S2 rerun and reported.
- **Built:** `toyos-link` (`lib/link`), and the I219 and virtio drivers as
  `drives` rows of their own (`system/i219`, `system/virtio-net`), which the
  supervisor's match table already binds; netstack claims no device and
  refuses a link of another address by name. A link whose driver wrote what
  no driver writes ends, and that address's next hello is refused
  `Faulted`, on which the driver ends: the supervisor's restart budget (3 in
  10 s) bounds a driver that keeps doing it. virtio's row restarts; the I219's
  does not, until
  `issues/no-machine-has-read-an-intel-nic-across-a-link-change.md` reads its
  reset over a live ring. In a guest:
  `a_link_ends_under_a_held_stream_and_comes_back` (a driver killed and
  started again by its job, the stream intact; the supervisor's restart of a
  `drives` row is the lifecycle crate's host tests'), and
  `netstack_holders_exported_to_children` on virtio and on QEMU's `e1000e`.
- **Read on the T14:** `i219_carries_netstacks_link` and
  `i219_leaves_ulp_unarmed` passed on one boot at 8bdf49156
  (https://github.com/ToyOSOrg/evidence/blob/4d818727fb372e181c402149aeb2779808fa2ef2/p4-r1/metal-t14-judge.log).
- **S2, reported:** on the T14, the hop costs one CPU about 3,300 TSC
  cycles a frame with a doorbell per frame, 920 per batch of 8 and 620 per
  batch of 32, both directions alike: about 740,000, 2.6 million and 3.9
  million frames a second of one CPU at 2.42 GHz, against the 200,163 a
  second each way full rate needs
  (https://github.com/ToyOSOrg/evidence/blob/4a8d02243d626df929fe05365e991873574e0dc2/s2/README.md).

**P5, netstack over several links: built.** Owner: the orchestrator.
Depends on: P4.
- **Exit:**
  - host tests of rank (wired before wireless, then arrival), failover and
    return, resolvers, mDNS per link, and a flow bound to a lost address;
  - a guest with two virtio NICs, in which the first's link is set off over
    QMP and traffic leaves on the second;
  - its defect closed, and the NIC taken out of the virtio bullet of
    `issues/first-match-selection-that-remains.md`.
- **What it is.** One stack over every link a driver brings, up to eight:
  each an interface of `ip`'s with a rank (a hello says Wired or Wireless),
  its own lease, DHCP client and mDNS responder under one host name; lookups
  ask the resolvers of the first link up holding a lease. A connection whose
  address goes ends `AddressGone`; one whose link only goes down keeps it.
  netstack serves from the start, so `LINK_BOUND` and `OneLink` are gone.
- Its T14 row, both links leasing with the wired one the default route, is
  W6's: it needs a radio.

**P6, `lib/crypto`'s AES-CMAC and SAE.** Owner: the orchestrator, whose
pull request brings the crate in unchanged at the commit the crypto agent,
who writes it outside the tree, names as qualified. Depends on: that commit.
- **Exit:** `lib/crypto` in the tree with these, each with the crypto
  agent's vectors:
  - AES-CMAC-128 (RFC 4493);
  - SAE over P-256 by hash-to-element (IEEE 802.11 Annex J, RFC 9380);
  - a constant-time comparison;
  - an owned secret that wipes on drop.
- **Nothing more enters:** no RustCrypto crate, and none of the crate's
  `tools/*.py`.

**P8, acpiserver serves a driver its own device's tables.** Owner: the
orchestrator; its defect is `issues/acpiserver-serves-no-client.md`.
Depends on: nothing; W6 waits on it.
- **Exit:**
  - acpiserver evaluates the methods under one PCI function's ACPI device for
    that function's driver alone, over a connection the supervisor opens for
    that binding;
  - host tests of the binding's refusals;
  - a T14 row reads the AX210's tables.

**W1, frames**, a `lib/` crate. Depends on: nothing.
- **Scope:**
  - management, control (BAR, BA) and data frames;
  - the elements: SSID, rates, DS, TIM, country, RSN, RSNXE, HT, VHT, HE,
    extended capabilities and WMM;
  - A-MSDU;
  - 802.11 to Ethernet and back (RFC 1042);
  - the reorder window.
- **Exit:** host tests of every refusal and hostile length, a malformed
  country element among them; a seeded structured fuzz of a fixed count, as
  `lib/ssh/tests/fuzz.rs`; and a third-party capture corpus from outside the
  references, beside the crate, with provenance, terms and `NOTICE` rows,
  pinned by hash.
- **Landed** in #924 and #930; the exit is met, the readings checked
  against the text (#924, issuecomment-6173453493).
- **No bridge-tunnel.** Every MSDU is sent under RFC 1042's header, and one
  under any other OUI is refused `UnknownOui`, the bridge-tunnel one
  included: a departure from §5.1.4's recommendation of ISO/IEC TR
  11802-5:1997's translation. What the selective translation table holds
  stays unchecked (#924, issuecomment-6173453493, row 62) and nothing
  depends on it. The decision rests instead on ToyOS's own EtherTypes
  arriving under RFC 1042, which the capture corpus shows for IPv4 and ARP;
  it holds no IPv6 or EAPOL frame.

**W2, the handshakes, in `wlan`**, sans-IO: the 4-way and group-key
handshakes, SAE by hash-to-element, PMF's keys, and the Transition Disable
KDE read from message 3 of an SAE join once its MIC verifies. Depends on:
W1, P6.
- **Exit:** host tests against Annex J, RFCs 6070, 3394, 4493 and 9380, and
  a captured handshake; and these refusals, each tested:
  - a replayed counter;
  - a retransmitted message 3 that reinstalls nothing;
  - a resent message 1 keeping its SNonce;
  - message 3's RSNE or RSNXE differing from the beacon's;
  - group key ID 0;
  - a MIC mismatch;
  - an SAE Confirm that does not verify;
  - an SAE Commit with an invalid scalar or element;
  - a PSK handshake for a network marked by Transition Disable.
- **W2a, WPA2-Personal: built** (`system/wlan/handshake`,
  `toyos-wlan-handshake`), on `lib/crypto`'s `ct::equal` and `Secret`. The 4-way and group key
  handshakes under AKM 2 with CCMP-128, the IGTK under BIP-CMAC-128 when
  PMF is negotiated. The first six refusals above are host tests against an
  authenticator in the tests that shares no crypto code with the crate;
  Annex J's PRF and pass-phrase vectors, and RFC 6070's one case a
  pass-phrase and SSID can be, are `lib/crypto`'s.
  - **The captured handshake** is two of wpa_supplicant v2.10's, one an
    access point, over `mac80211_hwsim`: plain PSK and PMF required. Messages
    2 and 4 are answered byte for byte, and the keys are the station's own.
    So are two PTK rekeys per run, which the station asked for. As the
    station did, they reinstall no group key.
  - **No group key handshake is recorded.** v2.10's access point parsed
    `group_rekey` and never rekeyed, and its `wpa_cli` has no `REKEY_GTK`.
    Group message 1 is tested only against the tests' authenticator, and is
    first met for real in W6's T14 rows.
  - Its readings of the text are
    `issues/wlans-readings-of-802-11-are-unchecked-against-its-text.md`.
  - **W2b waits for P6:** SAE, AES-CMAC, AKM 6 (PSK-SHA256), and the
    Transition Disable KDE with its PSK refusal. RFCs 4493 and 9380 are its
    vectors.

**W3, the firmware: done.** Depends on: the specification, revision 2.
- **Scope:**
  - the firmware containers;
  - commands and notifications typed per declared version;
  - the parts served, and any other refused by name;
  - each part's newest linux-firmware build at the pin: one file for the
    AX200, two for the AX210, committed unmodified in
    `system/ax2xx/firmware/` with `NOTICE` and `COMMITTED_FILES` rows.
- **Each part's newest build ships unmodified, pinned by hash, whether or
  not a vendor statement shows its part verifies it**: the owner's ruling.
  That nothing shows it is
  `issues/nothing-shows-the-ax200-or-ax210-verifies-its-firmware.md`.
- **Its licence forbids reverse engineering**, so of a file, only the
  container a loader parses is read.
- **Exit:** host tests that the pinned files decode; that every command
  sent is declared at a typed version; that sizes match those the
  specification asserts; and that recorded bench traces, addresses
  scrubbed, are answered.
- **Done** (`system/ax2xx/driver`, `toyos-ax2xx`). The first three exits
  are host tests. The fourth is `tests/traces.rs`, over the AX200's two
  recorded runs (§15): Run13 through a passive scan, Run11 through a
  refused command.
  - Each device payload's decisions are the same with every unrecorded bit
    complemented.
  - From those decisions and the file, Run13's commands 0 to 11 and its
    scan request, and Run11's 0 to 8, are built byte for byte.
  - Not built: commands 12 to 14, which are the demo's (§15.3), and the
    scan request's probe template and segments, which are the caller's and
    are taken from the run.
  - Run11's error tables name the refused command and both its lengths.
  - A version lookup takes a message's first entry and reads 99 as no
    entry (§6.5).
- **No AX210 trace exists**: no run drove one (§15.1). The AX210's own paths,
  its alive SKU and platform NVM among them, are first checked by W5's T14
  rows.

**W4, `wlan`, `radio`, `wifi` and the settings program.** Depends on: W1,
W2.
- **Exit:** host tests of:
  - BSS choice: joinable security, SAE before PSK, band, signal, and a
    marked network's PSK-only BSS refused by name ("transition disabled:
    this network is WPA3-only");
  - the mark, kept per network for the boot;
  - connect, disconnect and status;
  - the wrong-credential refusal, on evidence the station holds;
  - hostile `radio` answers.
- The pull request landing the mark files the Transition Disable weakness below.
- **W4a, WPA2-Personal: built.** `wlan` (`system/wlan`, its decisions
  `toyos-wlan-station` in `system/wlan/station`), the `radio` and `wifi`
  protocols (`toyos-radio`, `toyos-wifi`) and the settings program `wifi`
  (`system/wifi`).
  - **The ports run the other way from #876 §2's table**: `wlan` serves
    `radio` and each driver connects to it, as each NIC driver connects to
    netstack's `link`, because a port has one server and there is one driver
    per radio function. The orchestrator accepted this on #1140, as the
    owner's split ruling's pattern. The build lets only `ax2xx` receive `radio` and only
    `wifi` receive `wifi`.
  - **`Join` names its BSS whole**: the SSID, and the elements of the Beacon
    or Probe Response it was chosen by, so a driver keeps no scan results.
  - **Host tests** of BSS choice (joinable security, band, signal, every
    refusal by name), connect, disconnect and status, the wrong-credential
    refusal (the AP did not answer message 2), a lost connection joined
    again, and hostile `radio` answers: every frame that does not decode,
    every frame out of the protocol's order and every bound missed ends the
    radio by name. A whole join runs against both of W2a's recorded
    handshakes, with the recorded keys installed and the recorded message 4
    sent.
  - **Not built, with W2b:** SAE before PSK (a transition-mode network is
    joined by PSK, an SAE-only one refused "this station does not run SAE
    yet"), the mark, and its refusal of a PSK-only BSS.
  - **Not joined:** an open network (by design), a hidden one (no directed
    probe), and a 64-digit hexadecimal PSK, which `lib/crypto` gives no PMK
    constructor for. `wlan` drives one radio at a time.
  - No image ships `ax2xx`, so no radio says hello until W5. `wlan` runs in
    the system image (`images/system.toml`), where `wifi status` says
    `no radio`.

**W5, bring-up and passive scan on the T14**, the first stage that ships
`ax2xx`. Depends on: S1 (met), P1, P4 (its match table), W3, W4; and the test
networks, an access point at the T14's bench serving a WPA2-Personal network
and a WPA3-Personal one by hash-to-element, with PMF and HE, an instrument
the orchestrator owns. Exit: a T14 row reads the firmware up, its MAC
address, and the test network seen, its beacon's Country element read: a
missing pad (`CountryPad`) refuses the whole beacon. The network's name arrives per run
through a test job holding `wifi`, so no shipping path reads the ESP.
- **The test networks** (the orchestrator's): the AMD bench's AX200 as the
  access point under Linux, `wpa_supplicant` in `mode=2`, installing nothing
  and running only for the run, while the AMD bench sits in Linux. Its SSID
  and passphrase are generated per run and never published. Unread until
  the orchestrator's bench read when W5 is planned: whether that access
  point offers SAE by hash-to-element, PMF and HE, which W5's WPA3 network
  and W6's HE row need.

**W6, the first version: a full-rate join.** Depends on: W5 and its test
networks, P4, P5, P6, P8.
- **Exit, host tests:**
  - block-ack state;
  - a firmware restart's decisions;
  - each channel's limit as the tighter of the platform's table and the
    AP's country element;
  - a country element that would raise a limit;
  - under operating classes, a country element with an invalid first
    channel or a reserved class read up to it, as §10.22.3 has;
  - a platform with no table, refused by name.
- **Exit, T14 rows:**
  - WPA2 and WPA3 joins, each with a lease, DNS and TCP;
  - HE, else VHT, at the AP's width;
  - block-ack both ways;
  - transmit limits applied before the first frame;
  - a wrong credential refused with its evidence;
  - `ax2xx` killed, and rejoining;
  - both links leasing, the I219's and the radio's, with the wired one the
    default route (P5's).
- **Reported, never gated:** the HT, VHT and HE rates, and throughput
  against Linux on the same machine, AP and position.
- `issues/a-platform-with-no-transmit-power-table-joins-no-wifi-network.md`
  is its.

**W12, AP mode on the AX2xx**, directly after W6: ToyOS as the access point,
Linux the independent station (the owner's direction). Scope: beacons,
association, and the authenticator's side of the 4-way and group key
handshakes. Depends on: W6; and a specification revision stating the
firmware's AP-mode support and its band and regulatory limits, a reader's
question.
- **Exit:** a Linux station joins a ToyOS access point on the bench.
- **ToyOS to ToyOS** is a later regression row, never the exit: Linux is
  the oracle.

**W7, the AX200 on the AMD bench.** Depends on: P2, P3, W6, and its test
networks. Exit: W5's and W6's rows on the AMD bench, all but the I219's.
- **Its test networks** swap W5's roles: the T14's AX210 is the access point
  under Linux, `wpa_supplicant` in `mode=2`, installing nothing and running
  only for the run, while the AMD bench runs ToyOS. Its SSID and passphrase
  are generated per run and never published. Unread, as for W5's: whether
  that access point offers SAE by hash-to-element, PMF and HE.

**W8, power save, background scan, roaming, beacon loss and RF-kill.**
Depends on: W6; W7, for the AMD bench's rows; and, at each bench, two
instruments of `issues/no-access-point-answers-to-the-harness.md`: an
access point the harness silences, for beacon loss, and two access points of
one network, for roaming. Exit: host tests of the decisions, and rows on both
benches; power and latency are reported.

**W9, 6 GHz on the AX210**, directly after W6. Depends on: W6, and a 6 GHz
access point at the T14's bench, an instrument the orchestrator owns (Q3).
Exit: host tests of 6 GHz discovery's rules, and a T14 row joining the
6 GHz access point.

**W10, the desktop's Wi-Fi status and settings.** Depends on: W4. Exit: a
host test of its state machine, and a guest test on the compositor's
surface whose round says why no cheaper tier holds it.

**W11, saved networks**: for each network, its passphrase as typed and its
Transition Disable mark, in a plain-text file in `wlan`'s own state that no
other row names. Depends on: W6, and stage 2 of
`issues/every-program-sees-only-the-files-it-was-given.md`, with `wlan`'s
row the only one whose view holds the file. Exit: host tests of the file's
format (a mark included) and its refusals, and a T14 row joining a saved
network at boot. The pull request landing it files the plain-text weakness
below.

## Weaknesses the rulings record

**Saved networks are plain text** (Q4). From W11's landing, whoever reads the
disk reads every saved network's passphrase: another OS booted on the
machine, the disk taken out, or a program holding the disk's claim or
serving the file. With it they join that network, decrypt every handshake
they saw under PSK, and hold the passphrase wherever else it is used.
- **Owner:** the orchestrator.
- **Exit:** a ToyOS key store, a concept the owner rules on.

**A network's Transition Disable mark lasts only the boot until W11.** From
W4's landing a reboot loses every mark. Until a network's first SAE join of
the boot, a BSS offering its SSID with PSK alone draws the station's message
2 and attacks the passphrase offline. That is the downgrade of Vanhoef and
Ronen, "Dragonblood" (IEEE S&P 2020, §4.1.1).
- **Owner:** the orchestrator.
- **Exit:** W11 keeps the mark in the saved file.

## Owed beside the stages

- `issues/no-access-point-answers-to-the-harness.md`: deauthentication and a
  group rekey on demand, and W8's access points.
- `issues/the-wifi-demos-commits-still-resolve-on-origin.md`.
