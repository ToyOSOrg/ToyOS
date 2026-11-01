//! Key Data (§12.7.2): elements and KDEs, after AES key wrap's padding is gone.
//!
//! A KDE is a vendor-specific element under OUI 00-0F-AC. One this module does not
//! type, and any other element but the RSNE and RSNXE, is skipped, as a receiver ignores what it
//! does not know (§12.7.2).

use crate::Refused;

const RSNE: u8 = 48;
const RSNXE: u8 = 244;
const VENDOR: u8 = 0xdd;
const IEEE80211_OUI: [u8; 3] = [0x00, 0x0f, 0xac];
const KDE_GTK: u8 = 1;
const KDE_IGTK: u8 = 9;

/// The GTK KDE's key, as CCMP-128 sizes it.
pub(crate) struct Gtk<'a> {
    pub(crate) id: u8,
    pub(crate) key: &'a [u8; 16],
}

/// The IGTK KDE, as BIP-CMAC-128 sizes it.
pub(crate) struct Igtk<'a> {
    pub(crate) id: u16,
    pub(crate) ipn: [u8; 6],
    pub(crate) key: &'a [u8; 16],
}

#[derive(Default)]
pub(crate) struct KeyData<'a> {
    /// The RSNE and RSNXE whole, Element ID and Length included.
    pub(crate) rsne: Option<&'a [u8]>,
    pub(crate) rsnxe: Option<&'a [u8]>,
    pub(crate) gtk: Option<Gtk<'a>>,
    pub(crate) igtk: Option<Igtk<'a>>,
}

fn once<T>(slot: &mut Option<T>, value: T) -> Result<(), Refused> {
    if slot.is_some() {
        return Err(Refused::Duplicate);
    }
    *slot = Some(value);
    Ok(())
}

impl<'a> KeyData<'a> {
    pub(crate) fn parse(data: &'a [u8]) -> Result<Self, Refused> {
        let mut kd = Self::default();
        let mut rest = data;
        while let [id, tail @ ..] = rest {
            // The padding AES key wrap's input needs: 0xDD, then zeros to the end (§12.7.2).
            if *id == VENDOR && tail.iter().all(|&b| b == 0) {
                break;
            }
            let (&len, tail) = tail.split_first().ok_or(Refused::KeyDataElement)?;
            let whole_len = usize::from(len).saturating_add(2);
            let whole = rest.get(..whole_len).ok_or(Refused::KeyDataElement)?;
            let body = tail
                .get(..usize::from(len))
                .ok_or(Refused::KeyDataElement)?;
            rest = rest.get(whole_len..).ok_or(Refused::KeyDataElement)?;
            match *id {
                RSNE if kd.rsne.is_some() => return Err(Refused::SecondRsne),
                RSNE => kd.rsne = Some(whole),
                RSNXE => once(&mut kd.rsnxe, whole)?,
                VENDOR => kd.kde(body)?,
                _ => {}
            }
        }
        Ok(kd)
    }

    fn kde(&mut self, body: &'a [u8]) -> Result<(), Refused> {
        let Some(([o0, o1, o2, kind], body)) = body.split_first_chunk::<4>() else {
            return Ok(());
        };
        if [*o0, *o1, *o2] != IEEE80211_OUI {
            return Ok(());
        }
        match *kind {
            KDE_GTK => {
                // Key ID in bits 0 and 1, Tx in bit 2, then a reserved octet.
                let [info, _reserved, key @ ..] = body else {
                    return Err(Refused::GtkLength);
                };
                let key: &[u8; 16] = key.try_into().map_err(|_| Refused::GtkLength)?;
                once(
                    &mut self.gtk,
                    Gtk {
                        id: info & 0b11,
                        key,
                    },
                )
            }
            KDE_IGTK => {
                let (id, rest) = body.split_first_chunk::<2>().ok_or(Refused::IgtkLength)?;
                let (ipn, key) = rest.split_first_chunk::<6>().ok_or(Refused::IgtkLength)?;
                let key: &[u8; 16] = key.try_into().map_err(|_| Refused::IgtkLength)?;
                once(
                    &mut self.igtk,
                    Igtk {
                        id: u16::from_le_bytes(*id),
                        ipn: *ipn,
                        key,
                    },
                )
            }
            _ => Ok(()),
        }
    }
}
