//! A WPA2-Personal station's handshakes with its access point (IEEE Std 802.11-2020 §12.7),
//! sans-IO: the 4-way handshake and the group key handshake over EAPOL-Key frames, under AKM 2
//! (PSK) with CCMP-128, and BIP-CMAC-128's IGTK when management frame protection is negotiated.
//!
//! [`Supplicant::new`] takes the PMK and what the association settled; each frame the
//! authenticator sends goes to [`Supplicant::receive`], whose [`Step`] is the reply to send and
//! the keys to install after it. Every frame that breaks a rule is refused by name and changes
//! nothing but, once its MIC verifies, the replay counter. A refusal after the MIC verified, an
//! RSNE differing from the beacon's above all, is a reason to leave the network, which is the
//! caller's to do.

#![no_std]
#![forbid(unsafe_code)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::arithmetic_side_effects,
        clippy::indexing_slicing,
        clippy::unwrap_used,
        clippy::as_conversions
    )
)]

extern crate alloc;

mod eapol;
mod kde;
mod supplicant;

pub use supplicant::{GroupKey, IntegrityKey, Join, PairwiseKey, Step, Supplicant};

macro_rules! reasons {
    ($(#[$meta:meta])* $name:ident { $($variant:ident = $why:literal;)* }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name {
            $(#[doc = $why] $variant,)*
        }

        impl $name {
            pub const ALL: &'static [Self] = &[$(Self::$variant,)*];

            pub const fn reason(self) -> &'static str {
                match self {
                    $(Self::$variant => $why,)*
                }
            }
        }

        impl core::fmt::Display for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                f.write_str(self.reason())
            }
        }
    };
}

reasons! {
    /// Why an association cannot be keyed by this supplicant.
    Unjoinable {
        Element = "an RSNE or RSNXE that is not one well-formed element of its kind";
        Akm = "the station's AKM is not PSK alone, or the access point does not offer PSK";
        Pairwise = "the station's pairwise cipher is not CCMP-128 alone, or the access point does not offer it";
        Group = "a group data cipher other than CCMP-128";
        Management = "management frame protection negotiated with a group management cipher other than BIP-CMAC-128";
        Protection = "one side requires management frame protection and the other is not capable of it";
    }
}

reasons! {
    /// Why an EAPOL-Key frame was refused.
    Refused {
        Truncated = "the frame ends inside its EAPOL-Key fields";
        ProtocolVersion = "an EAPOL Protocol Version other than 1, 2 or 3";
        PacketType = "an EAPOL packet other than EAPOL-Key";
        BodyLength = "a Packet Body Length past the end of the frame";
        DescriptorType = "a key descriptor other than IEEE 802.11's";
        DescriptorVersion = "a Key Descriptor Version other than 2, HMAC-SHA1-128 and AES key wrap, which AKM 2 with CCMP-128 uses";
        KeyDataLength = "a Key Data Length other than the octets that follow it";
        NotFromAuthenticator = "Key Ack clear, or Request or Error set: no authenticator sends such a frame";
        UnknownMessage = "Key Information no handshake message has: a message 1 with Install, a message 3 without Install or Secure, or a group key frame without a Key MIC or Secure, or with Install";
        KeyLength = "a pairwise Key Length other than CCMP-128's 16 octets";
        Replayed = "a Key Replay Counter not above the last one whose MIC verified";
        Mic = "a Key MIC that does not verify";
        NoMessage1 = "a message 3 with no message 1 before it";
        AnonceChanged = "a message 3 whose ANonce is not its message 1's";
        NoPairwiseKey = "a group key message before a 4-way handshake completed";
        EncryptedMessage1 = "a message 1 whose Key Data is encrypted";
        Plaintext = "a message 3 or group message 1 whose Key Data is not encrypted";
        WrappedLength = "encrypted Key Data whose length AES key wrap does not produce, or longer than a frame carries";
        Unwrap = "encrypted Key Data whose AES key wrap integrity check fails";
        KeyDataElement = "an element or KDE whose length runs past the Key Data";
        Duplicate = "a GTK, IGTK or RSNXE given twice";
        SecondRsne = "a message 3 with a second RSNE, offering a pairwise cipher of its own";
        RsneDiffers = "a message 3 whose RSNE is not the beacon's";
        RsnxeDiffers = "a message 3 whose RSNXE is not the beacon's, or that has one the beacon lacks";
        NoGtk = "no GTK KDE";
        GtkLength = "a GTK KDE whose key is not CCMP-128's 16 octets";
        GroupKeyIdZero = "a GTK under key ID 0, which names no group key";
        Rsc = "a receive sequence counter past CCMP's 48-bit packet number";
        NoIgtk = "management frame protection was negotiated and no IGTK KDE came";
        IgtkLength = "an IGTK KDE whose key is not BIP-CMAC-128's 16 octets";
        IgtkKeyId = "an IGTK under a key ID other than 4 or 5";
    }
}

impl Refused {
    /// Whether a frame refused for this reason had a Key MIC that verified: its sender holds
    /// the PTK, so the refusal is the authenticator's own, and the station leaves the network.
    /// Every other refusal drops the frame alone.
    pub const fn verified(self) -> bool {
        match self {
            Self::Truncated
            | Self::ProtocolVersion
            | Self::PacketType
            | Self::BodyLength
            | Self::DescriptorType
            | Self::DescriptorVersion
            | Self::KeyDataLength
            | Self::NotFromAuthenticator
            | Self::UnknownMessage
            | Self::KeyLength
            | Self::Replayed
            | Self::Mic
            | Self::NoMessage1
            | Self::AnonceChanged
            | Self::NoPairwiseKey
            | Self::EncryptedMessage1 => false,
            Self::Plaintext
            | Self::WrappedLength
            | Self::Unwrap
            | Self::KeyDataElement
            | Self::Duplicate
            | Self::SecondRsne
            | Self::RsneDiffers
            | Self::RsnxeDiffers
            | Self::NoGtk
            | Self::GtkLength
            | Self::GroupKeyIdZero
            | Self::Rsc
            | Self::NoIgtk
            | Self::IgtkLength
            | Self::IgtkKeyId => true,
        }
    }
}
