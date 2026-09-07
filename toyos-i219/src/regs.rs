//! The register file, as the specification defines it.
//!
//! Every offset and every bit below is cited to the *Intel 82574 GbE
//! Controller Family Datasheet*, order number 317694-018, revision 2.7 — the
//! document that defines this register family. The I219 the T14 carries and
//! QEMU's `e1000e` model are both parts of it; §10.2's Table 77 is the register
//! summary every offset here is copied from.
//!
//! Nothing here has behaviour. A number that is not in the datasheet does not
//! belong in this file.

/// Device Control (§10.2.2.1, `0x00000`).
pub const CTRL: usize = 0x00000;
/// Device Status (§10.2.2.2, `0x00008`), read-only.
pub const STATUS: usize = 0x00008;
/// Interrupt Cause Read (§10.2.4.1, `0x000C0`), read-to-clear and
/// write-1-to-clear.
pub const ICR: usize = 0x000C0;
/// Interrupt Throttling (§10.2.4.2, `0x000C4`).
pub const ITR: usize = 0x000C4;
/// Interrupt Cause Set (§10.2.4.4, `0x000C8`), write-only.
pub const ICS: usize = 0x000C8;
/// Interrupt Mask Set/Read (§10.2.4.5, `0x000D0`).
pub const IMS: usize = 0x000D0;
/// Interrupt Mask Clear (§10.2.4.6, `0x000D8`), write-only.
pub const IMC: usize = 0x000D8;
/// Receive Control (§10.2.5.1, `0x00100`).
pub const RCTL: usize = 0x00100;
/// Receive descriptor ring, queue 0 (§10.2.5.5-§10.2.5.9).
pub const RDBAL: usize = 0x02800;
pub const RDBAH: usize = 0x02804;
pub const RDLEN: usize = 0x02808;
pub const RDH: usize = 0x02810;
pub const RDT: usize = 0x02818;
/// Rx Interrupt Delay Timer (§10.2.5.10, `0x02820`) and its absolute
/// counterpart (`0x0282C`). Both are written to zero: an interrupt this driver
/// waits on may not be held back by a timer nothing else expires.
pub const RDTR: usize = 0x02820;
pub const RADV: usize = 0x0282C;
/// Multicast Table Array, 128 dwords (§10.2.5.21, `0x05200`..`0x053FC`).
pub const MTA: usize = 0x05200;
pub const MTA_DWORDS: usize = 128;
/// Receive Address Low/High, entry 0 (§10.2.5.22, §10.2.5.23). Entry 0 is the
/// station address and the one exact-match filter this driver uses.
pub const RAL0: usize = 0x05400;
pub const RAH0: usize = 0x05404;
/// Transmit Control (§10.2.6.1, `0x00400`) and the inter-packet gap
/// (§10.2.6.2, `0x00410`).
pub const TCTL: usize = 0x00400;
pub const TIPG: usize = 0x00410;
/// Transmit descriptor ring (§10.2.6.4-§10.2.6.8).
pub const TDBAL: usize = 0x03800;
pub const TDBAH: usize = 0x03804;
pub const TDLEN: usize = 0x03808;
pub const TDH: usize = 0x03810;
pub const TDT: usize = 0x03818;
/// Transmit Interrupt Delay Value (`0x03820`) and Transmit Descriptor Control
/// (§10.2.6.10, `0x03828`).
pub const TIDV: usize = 0x03820;
pub const TXDCTL: usize = 0x03828;
pub const TADV: usize = 0x0382C;

/// The registers this driver reaches, and therefore the smallest BAR it can be
/// driven through. The whole file is 128 KiB on every part in reach; this is
/// the bound the register accessor's contract rests on, and it is above the
/// highest offset named above.
pub const REGISTER_BYTES: usize = 0x06000;

const _: () = {
    assert!(MTA + MTA_DWORDS * 4 <= REGISTER_BYTES);
    assert!(RAH0 < REGISTER_BYTES);
    assert!(TADV < REGISTER_BYTES);
};

/// Device Control bits (§10.2.2.1).
pub mod ctrl {
    /// Full Duplex (bit 0), reflected from the PHY unless `FRCDPLX` forces it.
    pub const FD: u32 = 1 << 0;
    /// Auto-Speed Detection Enable (bit 5). **Written as zero**: §10.2.2.1 says
    /// "This bit must be set to 0b in the 82574", and the driver takes the
    /// speed the PHY resolved rather than making the MAC guess it.
    pub const ASDE: u32 = 1 << 5;
    /// Set Link Up (bit 6). §10.2.2.1: it "MUST be set to 1b to permit the MAC
    /// to recognize the link signal from the PHY [...] and to receive and
    /// transmit data".
    pub const SLU: u32 = 1 << 6;
    /// Invert Loss of Signal (bit 7). §10.2.2.1: "Reserved. Must be set to 0b."
    pub const ILOS: u32 = 1 << 7;
    /// Force Speed (bit 11) and Force Duplex (bit 12). Both cleared: the PHY's
    /// auto-negotiation is what resolves the link.
    pub const FRCSPD: u32 = 1 << 11;
    pub const FRCDPLX: u32 = 1 << 12;
    /// Device Reset (bit 26). §10.2.2.1: "writing 1b initiates the reset. This
    /// bit is self-clearing."
    pub const RST: u32 = 1 << 26;
    /// Receive and Transmit Flow Control Enable (bits 27, 28). Both cleared:
    /// this driver negotiates no flow control, so it sends and honours no
    /// pause frame.
    pub const RFCE: u32 = 1 << 27;
    pub const TFCE: u32 = 1 << 28;
    /// VLAN Mode Enable (bit 30). Cleared: a VLAN tag stays in the frame and
    /// smoltcp sees the wire's own bytes.
    pub const VME: u32 = 1 << 30;
}

/// Device Status bits (§10.2.2.2).
pub mod status {
    pub const FD: u32 = 1 << 0;
    /// Link Up (bit 1). §10.2.2.2: valid only while `CTRL.SLU` is set.
    pub const LU: u32 = 1 << 1;
    /// Link speed (bits 7:6): `00b` 10 Mb/s, `01b` 100 Mb/s, `10b` and `11b`
    /// 1000 Mb/s.
    pub const SPEED_SHIFT: u32 = 6;
    pub const SPEED_MASK: u32 = 0b11;
}

/// Interrupt cause bits, shared by `ICR`, `ICS`, `IMS` and `IMC`
/// (§10.2.4.1).
pub mod cause {
    /// Transmit Descriptor Written Back (bit 0).
    pub const TXDW: u32 = 1 << 0;
    /// Transmit Queue Empty (bit 1).
    pub const TXQE: u32 = 1 << 1;
    /// Link Status Change (bit 2) — "set whenever the link status changes
    /// (either from up to down, or from down to up)".
    pub const LSC: u32 = 1 << 2;
    /// Receive Descriptor Minimum Threshold Hit (bit 4).
    pub const RXDMT0: u32 = 1 << 4;
    /// Receiver Overrun (bit 6) — the receive FIFO overran, which on this
    /// driver means its buffers were not returned fast enough.
    pub const RXO: u32 = 1 << 6;
    /// Receiver Timer Interrupt (bit 7): with `RDTR` zero, one per packet.
    pub const RXT0: u32 = 1 << 7;
    /// Interrupt Asserted (bit 31). §10.2.4.1 notes it is not writable and
    /// clears only when every cause has.
    pub const INT_ASSERTED: u32 = 1 << 31;

    /// What §4.6.5 tells a driver to unmask: "Suggested bits include RXT, RXO,
    /// RXDMT and LSC. There is no reason to enable the transmit interrupts."
    /// Transmit completions are reclaimed on the next send, so nothing waits
    /// on one.
    pub const ENABLED: u32 = RXT0 | RXO | RXDMT0 | LSC;
}

/// Receive Control bits (§10.2.5.1).
pub mod rctl {
    /// Enable (bit 1). Written last: §4.6.5.1 says the receiver is enabled
    /// "only after all other setup is accomplished".
    pub const EN: u32 = 1 << 1;
    /// Store Bad Packets (bit 2). Cleared: a frame the PHY saw an error on is
    /// the hardware's to drop, not this driver's to hand up.
    pub const SBP: u32 = 1 << 2;
    /// Unicast and Multicast Promiscuous (bits 3, 4). Both cleared: the
    /// station address in `RAL0`/`RAH0` and broadcast are the whole filter.
    pub const UPE: u32 = 1 << 3;
    pub const MPE: u32 = 1 << 4;
    /// Long Packet Enable (bit 5). Cleared, so §10.2.5.1's "long packet" — one
    /// over 1522 bytes — is discarded by the hardware and no frame can span
    /// two 2048-byte buffers.
    pub const LPE: u32 = 1 << 5;
    /// Broadcast Accept Mode (bit 15). Set: ARP and DHCP arrive on it.
    pub const BAM: u32 = 1 << 15;
    /// Receive Buffer Size (bits 17:16) with `BSEX` clear: `00b` is 2048 bytes.
    pub const BSIZE_2048: u32 = 0b00 << 16;
    /// VLAN Filter Enable (bit 18). Cleared, so §4.6.5's "no need to
    /// initialize the VFTA array" holds.
    pub const VFE: u32 = 1 << 18;
    /// Buffer Size Extension (bit 25). Cleared, so `BSIZE` means what
    /// `BSIZE_2048` says.
    pub const BSEX: u32 = 1 << 25;
    /// Strip Ethernet CRC (bit 26). §10.2.5.1: the stripped CRC "is not DMA'd
    /// to host memory and is not included in the length reported in the
    /// descriptor", which is what makes a descriptor's length the frame's.
    pub const SECRC: u32 = 1 << 26;
}

/// Transmit Control bits (§10.2.6.1) and the values §4.6.6 suggests.
pub mod tctl {
    pub const EN: u32 = 1 << 1;
    /// Pad Short Packets (bit 3), so a frame under 64 bytes is padded by the
    /// hardware rather than by this driver.
    pub const PSP: u32 = 1 << 3;
    /// Collision Threshold (bits 11:4). §4.6.6: `CT = 0x0F`.
    pub const CT_SHIFT: u32 = 4;
    pub const CT: u32 = 0x0F << CT_SHIFT;
    /// Collision Distance (bits 21:12). §4.6.6: full duplex is `63`.
    pub const COLD_SHIFT: u32 = 12;
    pub const COLD_FULL_DUPLEX: u32 = 0x3F << COLD_SHIFT;
}

/// The inter-packet gap §4.6.6 names: `IPGT = 8`, `IPGR1 = 2`, `IPGR2 = 10`,
/// "the minimum legal IPG".
pub const TIPG_DEFAULT: u32 = 8 | (2 << 10) | (10 << 20);

/// Transmit Descriptor Control bits (§10.2.6.10) and what §4.6.6 suggests:
/// `GRAN = 1b` (descriptors), `WTHRESH = 1b`, everything else zero.
pub mod txdctl {
    pub const WTHRESH_SHIFT: u32 = 16;
    pub const GRAN: u32 = 1 << 24;
    /// §4.6.6's suggested write-back policy: one descriptor's worth, counted
    /// in descriptors. Anything larger holds a completion back until the ring
    /// fills, which on a sixteen-deep ring is a stall.
    pub const SUGGESTED: u32 = GRAN | (1 << WTHRESH_SHIFT);
}

/// Receive Address High bits (§10.2.5.23).
pub mod rah {
    /// Address Valid (bit 31). §10.2.5.23: after reset "if the NVM is present,
    /// the first register (Receive Address Register 0) is loaded from the IA
    /// field in the NVM [...] and its Address Valid field will be 1b. If no
    /// NVM is present the Address Valid field for n=0b will be 0b."
    pub const AV: u32 = 1 << 31;
    /// The high sixteen bits of the address (bits 15:0).
    pub const ADDRESS_MASK: u32 = 0xFFFF;
}

/// One legacy receive descriptor, sixteen bytes (§7.1.3, Figure 23):
/// `Buffer Address[63:0]`, then `Length[15:0]`, `Packet Checksum[31:16]`,
/// `Status[39:32]`, `Errors[47:40]`, `VLAN Tag[63:48]`.
pub mod rx_desc {
    pub const BYTES: usize = 16;
    pub const LENGTH_SHIFT: u32 = 0;
    pub const LENGTH_MASK: u64 = 0xFFFF;
    pub const STATUS_SHIFT: u32 = 32;
    pub const ERRORS_SHIFT: u32 = 40;
    pub const BYTE_MASK: u64 = 0xFF;

    /// Receive status bits (§7.1.3.3, Figure 24).
    pub mod status {
        /// Descriptor Done (bit 0) — "indicates whether hardware is done with
        /// the descriptor".
        pub const DD: u8 = 1 << 0;
        /// End of Packet (bit 1). §7.1.3.3: "If EOP is not set for a
        /// descriptor, only the Address, Length, and DD bits are valid."
        pub const EOP: u8 = 1 << 1;
    }

    /// Receive error bits (§7.1.3.4, Figure 25), valid only with `EOP` and
    /// `DD` set. Any of them is a frame this driver drops.
    pub mod errors {
        pub const CE: u8 = 1 << 0;
        pub const SE: u8 = 1 << 1;
        pub const SEQ: u8 = 1 << 2;
        pub const CXE: u8 = 1 << 4;
        pub const TCPE: u8 = 1 << 5;
        pub const IPE: u8 = 1 << 6;
        pub const RXE: u8 = 1 << 7;
        /// The ones that say the frame's own bytes are wrong. The two checksum
        /// bits are left out: this driver asks for no checksum offload, so
        /// §7.1.3.4's "if receive checksum offloading is disabled [...] the IPE
        /// and TCPE bits are 0b" makes them noise if a device sets them anyway.
        pub const FRAME_IS_BAD: u8 = CE | SE | SEQ | CXE | RXE;
    }
}

/// One legacy transmit descriptor, sixteen bytes (§7.2.10.1, Figure 31):
/// `Buffer Address[63:0]`, then `Length[15:0]`, `CSO[23:16]`, `CMD[31:24]`,
/// `STA[35:32]`, `ExtCMD[39:36]`, `CSS[47:40]`, `VLAN[63:48]`.
pub mod tx_desc {
    pub const BYTES: usize = 16;
    pub const LENGTH_MASK: u64 = 0xFFFF;
    pub const CMD_SHIFT: u32 = 24;
    pub const STATUS_SHIFT: u32 = 32;
    pub const STATUS_MASK: u64 = 0xF;

    /// Command byte fields (§7.2.10.1.4, Table 36).
    pub mod cmd {
        /// End of Packet (bit 0).
        pub const EOP: u8 = 1 << 0;
        /// Insert FCS (bit 1) — the hardware appends the Ethernet CRC.
        pub const IFCS: u8 = 1 << 1;
        /// Report Status (bit 3). §7.2.4.2: a descriptor with `RS` set is
        /// written back, which is the only way this driver learns the buffer
        /// is free again.
        pub const RS: u8 = 1 << 3;
        /// Descriptor Extension (bit 5). §7.2.10: "The legacy Tx descriptor is
        /// defined by setting the DEXT bit in the command field to 0b."
        pub const DEXT: u8 = 1 << 5;
        /// Interrupt Delay Enable (bit 7). Cleared: §7.2.10.1.4 says a
        /// descriptor with `RS` and no `IDE` is written back at once, and this
        /// driver has no timer to wait on.
        pub const IDE: u8 = 1 << 7;

        /// One whole frame in one descriptor, written back when it is sent.
        pub const ONE_FRAME: u8 = EOP | IFCS | RS;
    }

    /// Transmit status bits (§7.2.10.1, Figure 31): `DD` is bit 0 of `STA`.
    pub const STATUS_DD: u8 = 1 << 0;
}
