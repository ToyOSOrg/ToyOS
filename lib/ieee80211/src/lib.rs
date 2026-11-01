//! IEEE Std 802.11-2024 frames as a station meets them, with the Wi-Fi Alliance's WMM element
//! (WMM Specification v1.2.0): parsed in place from bytes a radio or a peer wrote, and refused by
//! the first rule they break, by name.
//!
//! [`frame::Frame::parse`] takes one MPDU without its FCS and yields a typed management, control
//! or data frame whose body still borrows the input. Elements are read by [`element`], A-MSDU
//! and the translation to and from Ethernet by [`data`], and the receive reordering buffer is
//! [`reorder::Window`].
//!
//! A [`Refused`] is either [`Class::Malformed`], input no correct sender produces, or
//! [`Class::Unsupported`], a well-formed frame this station does not handle.

#![no_std]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::as_conversions
    )
)]

extern crate alloc;

pub mod control;
pub mod data;
pub mod element;
pub mod frame;
pub mod mgmt;
pub mod reorder;

mod read;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// No correct sender produces these bytes.
    Malformed,
    /// Well formed, and not something this station handles.
    Unsupported,
}

macro_rules! refusals {
    ($($variant:ident = $class:ident, $why:literal;)*) => {
        /// Why a frame or element was not accepted, or a reordering buffer not made.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Refused {
            $(#[doc = $why] $variant,)*
        }

        impl Refused {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            pub const fn class(self) -> Class {
                match self {
                    $(Self::$variant => Class::$class,)*
                }
            }

            pub const fn reason(self) -> &'static str {
                match self {
                    $(Self::$variant => $why,)*
                }
            }
        }
    };
}

refusals! {
    // The MAC header, §9.2.
    Truncated = Malformed, "the bytes end inside a field";
    ProtocolVersion = Unsupported, "the protocol version is not 0 (§9.2.4.1.2)";
    ExtensionFrame = Unsupported, "a DMG or S1G Beacon, the frames of type Extension";
    ReservedSubtype = Malformed, "a type and subtype Table 9-1 reserves";
    QosManagement = Unsupported, "a QoS management frame: To DS set, From DS clear (Table 9-4)";
    ManagementFromDs = Malformed, "a management frame with From DS set, which Table 9-4 reserves";
    UnhandledManagement = Unsupported, "a management subtype a station does not handle";
    UnhandledControl = Unsupported, "a control frame other than BlockAckReq or BlockAck";
    UnhandledData = Unsupported, "an EBCS Data frame, or a QoS data subtype with +CF-Ack or +CF-Poll";
    HtcInNonQosData = Malformed, "+HTC set in a data frame other than QoS Data or QoS Null (§9.2.4.1.10)";
    Fragmented = Unsupported, "a fragment of an MSDU or MMPDU";
    GroupTransmitter = Malformed, "a transmitter address with the group bit set";
    // Management bodies, §9.3.3.
    ZeroBeaconInterval = Malformed, "a beacon interval of 0 time units";
    AssociationId = Malformed, "an association ID outside 1 to 2007 in a successful response";
    // Elements, §9.4.2.
    ElementPastBody = Malformed, "an element's length runs past the frame body";
    ExtensionWithoutId = Malformed, "an Element ID Extension element of length 0";
    DuplicateElement = Malformed, "an element that may appear once appears twice";
    ElementLength = Malformed, "an element whose length its definition does not allow";
    SsidLength = Malformed, "an SSID longer than 32 octets";
    RateCount = Malformed, "a Supported Rates element of 0 or more than 8 rates";
    DtimPeriod = Malformed, "a TIM element with a DTIM period of 0";
    TimBitmap = Malformed, "a TIM partial virtual bitmap past AID 2007";
    CountryString = Malformed, "a country string whose first two octets are not upper-case letters";
    CountryPad = Malformed, "a Country element of odd length, or whose pad octet is not 0";
    CountryTriplet = Malformed, "a Country element whose triplets do not tile it";
    RsnVersion = Unsupported, "an RSN element of a version other than 1";
    RsnCount = Malformed, "an RSN suite count running past the element";
    MfpRequiredNotCapable = Malformed, "management frame protection required but not capable";
    ExtendedRsnLength = Malformed, "an RSNXE whose field length runs past the element";
    SecondaryChannelOffset = Malformed, "an HT Operation secondary channel offset of 2";
    VhtReserved = Malformed, "a VHT capability or operation field of a reserved value";
    PpeThresholds = Malformed, "HE PPE Thresholds whose length its own NSTS and RU mask do not give";
    WmmVersion = Unsupported, "a WMM element of a version other than 1";
    WmmAifsn = Malformed, "a WMM AC Parameter Record with an AIFSN below 2";
    WmmAccessCategory = Malformed, "a WMM Parameter element that does not give each access category once";
    // Control frames, §9.3.1.
    ReservedBlockAckType = Malformed, "a BAR or BA Type Table 9-36 or Table 9-37 reserves";
    BlockAckVariant = Unsupported, "a block-ack variant other than Compressed";
    BlockAckFragmentNumber = Malformed, "a BlockAckReq Fragment Number other than 0, or a BlockAck one Table 9-38 reserves";
    FragmentLevel = Unsupported, "a BlockAck acknowledging fragments: Fragment Number B0 set";
    ControlLength = Malformed, "a block-ack frame whose length its variant does not give";
    // Data, §9.3.2 and §12.5.2.
    SubframeLength = Malformed, "an A-MSDU subframe whose length runs past the MPDU";
    SubframePadding = Malformed, "an A-MSDU subframe whose padding runs past the MPDU";
    SnapHeaderAsAddress = Malformed, "an A-MSDU whose first destination is an LLC/SNAP header";
    NotSnap = Unsupported, "an MSDU that is not LLC/SNAP";
    UnknownOui = Unsupported, "an LLC/SNAP header with an OUI other than RFC 1042's";
    LengthField = Unsupported, "a Length/Type value below 0x0600, which IEEE Std 802.3-2022 §3.2.6 does not read as a type";
    ExtendedIvClear = Malformed, "a CCMP header without its Ext IV bit";
    // The reordering buffer, §10.25.6.6.
    WindowSize = Malformed, "a reordering buffer of 0 or more than 256 MPDUs";
}

impl core::fmt::Display for Refused {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.reason())
    }
}

/// A 48-bit MAC address (IEEE 802-2014 §8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Address(pub [u8; 6]);

impl Address {
    pub const BROADCAST: Self = Self([0xFF; 6]);

    /// The Individual/Group bit, the least significant bit of the first octet.
    pub const fn is_group(self) -> bool {
        let [first, ..] = self.0;
        first & 1 != 0
    }
}

/// A 12-bit sequence number, compared modulo 4096 (§9.2.4.4.3, §10.3.2.14).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Seq(u16);

impl Seq {
    pub const MODULUS: u16 = 4096;

    /// The low 12 bits of `value`.
    pub const fn new(value: u16) -> Self {
        Self(value & 0x0FFF)
    }

    pub const fn get(self) -> u16 {
        self.0
    }

    pub const fn add(self, n: u16) -> Self {
        Self::new(self.0.wrapping_add(n))
    }

    /// How far `self` is ahead of `base`, modulo 4096.
    pub const fn since(self, base: Self) -> u16 {
        self.0.wrapping_sub(base.0) & 0x0FFF
    }
}
