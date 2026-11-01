//! The supplicant's side of the 4-way handshake (§12.7.6) and the group key handshake
//! (§12.7.7), and the pairwise key hierarchy between them (§12.7.1.3).
//!
//! - **Replays.** A frame whose Key Replay Counter is not above the last one whose MIC verified is
//!   refused, message 1 included; message 1 carries no MIC, so it never moves the counter.
//! - **Message 1 resent.** Until a message 3 is accepted, a message 1 is answered with the
//!   SNonce the first one drew, under whatever ANonce it carries; a message 3 refused after its
//!   MIC verified accepts nothing, and the caller leaves the network. Once a pairwise key is
//!   installed, a message 1 starts a rekey with a fresh SNonce.
//! - **No reinstallation.** A message 3 resent for a handshake already completed is answered and
//!   installs no pairwise key. A group key is handed out to install only when it is not the key
//!   already installed under its ID.

use alloc::vec::Vec;
use toyos_crypto::{
    Secret, ct,
    wpa2::{self, KeyEncryptionKey, PairwiseMasterKey},
};
use toyos_ieee80211::{
    Address,
    element::{self, Elements, Id, Rsn, Suite, Suites},
};

use crate::eapol::{Info, Key, Reply};
use crate::kde::KeyData;
use crate::{Refused, Unjoinable};

/// What the association settled, as the station saw and sent it.
pub struct Join<'a> {
    /// The access point's address, AA: the BSSID.
    pub authenticator: Address,
    /// The station's own address, SPA.
    pub supplicant: Address,
    /// The access point's RSNE as its Beacon or Probe Response carried it, Element ID and Length
    /// included. Message 3's must be identical (§12.7.6.4).
    pub beacon_rsne: &'a [u8],
    /// The access point's RSNXE likewise, if it sent one.
    pub beacon_rsnxe: Option<&'a [u8]>,
    /// The RSNE this station sent in its (Re)Association Request, which message 2 repeats.
    pub own_rsne: &'a [u8],
    /// The RSNXE it sent there, if any, which message 2 repeats too.
    pub own_rsnxe: Option<&'a [u8]>,
}

/// The reply to one frame, and the keys it delivered.
pub struct Step {
    /// The EAPOL-Key frame to send the authenticator. It goes out before any key below is
    /// installed, so message 4 leaves under the keys message 3 was received under.
    pub reply: Vec<u8>,
    pub pairwise: Option<PairwiseKey>,
    pub group: Option<GroupKey>,
    pub integrity: Option<IntegrityKey>,
}

/// CCMP-128's temporal key, the PTK's TK.
pub struct PairwiseKey(Secret<16>);

impl PairwiseKey {
    pub fn key(&self) -> &[u8; 16] {
        self.0.expose()
    }
}

/// A CCMP-128 GTK.
pub struct GroupKey {
    id: u8,
    key: Secret<16>,
    rsc: u64,
}

impl GroupKey {
    /// 1, 2 or 3.
    pub fn id(&self) -> u8 {
        self.id
    }

    pub fn key(&self) -> &[u8; 16] {
        self.key.expose()
    }

    /// The last packet number the authenticator sent under this key, which the receive replay
    /// check starts from.
    pub fn rsc(&self) -> u64 {
        self.rsc
    }
}

/// A BIP-CMAC-128 IGTK.
pub struct IntegrityKey {
    id: u16,
    key: Secret<16>,
    ipn: u64,
}

impl IntegrityKey {
    /// 4 or 5.
    pub fn id(&self) -> u16 {
        self.id
    }

    pub fn key(&self) -> &[u8; 16] {
        self.key.expose()
    }

    /// The last packet number the authenticator sent under this key.
    pub fn ipn(&self) -> u64 {
        self.ipn
    }
}

/// KCK, KEK and TK: AKM 2's 128-bit KCK and KEK, then CCMP-128's TK (§12.7.1.3).
struct Ptk(Secret<48>);

impl Ptk {
    fn derive(
        pmk: &PairwiseMasterKey,
        aa: Address,
        spa: Address,
        anonce: &[u8; 32],
        snonce: &[u8; 32],
    ) -> Self {
        let (a_low, a_high) = if aa.0 < spa.0 {
            (aa.0, spa.0)
        } else {
            (spa.0, aa.0)
        };
        let (n_low, n_high) = if anonce < snonce {
            (anonce, snonce)
        } else {
            (snonce, anonce)
        };
        let mut data = [0u8; 6 + 6 + 32 + 32];
        let (addresses, nonces) = data.split_at_mut(12);
        let (low, high) = addresses.split_at_mut(6);
        low.copy_from_slice(&a_low);
        high.copy_from_slice(&a_high);
        let (low, high) = nonces.split_at_mut(32);
        low.copy_from_slice(n_low);
        high.copy_from_slice(n_high);
        let mut ptk = Secret::new([0; 48]);
        wpa2::prf(
            pmk.as_bytes(),
            b"Pairwise key expansion",
            &data,
            ptk.expose_mut(),
        )
        .expect("a 48-octet PRF output is inside its bound");
        Self(ptk)
    }

    fn part(&self, n: usize) -> &[u8; 16] {
        self.0
            .expose()
            .as_chunks::<16>()
            .0
            .get(n)
            .expect("a PTK is three 16-octet keys")
    }

    fn kck(&self) -> &[u8; 16] {
        self.part(0)
    }

    fn kek(&self) -> &[u8; 16] {
        self.part(1)
    }

    fn tk(&self) -> &[u8; 16] {
        self.part(2)
    }
}

struct Handshake {
    anonce: [u8; 32],
    snonce: [u8; 32],
    ptk: Ptk,
}

/// More than the Key Data of any EAPOL-Key frame an MSDU of at most 2,304 octets carries.
const KEY_DATA_MAX: usize = 2304;

pub struct Supplicant {
    pmk: PairwiseMasterKey,
    authenticator: Address,
    supplicant: Address,
    beacon_rsne: Vec<u8>,
    beacon_rsnxe: Option<Vec<u8>>,
    /// Message 2's Key Data: the station's RSNE, then its RSNXE.
    own: Vec<u8>,
    protected: bool,
    replay: Option<u64>,
    /// The handshake message 2 answered, until a message 3 for it is accepted.
    pending: Option<Handshake>,
    /// The handshake whose TK is installed.
    current: Option<Handshake>,
    gtk: Option<(u8, Secret<16>)>,
    igtk: Option<(u16, Secret<16>)>,
}

/// `bytes` as one element under `id`, parsed.
fn single<'a>(bytes: &'a [u8], id: Id) -> Result<Elements<'a>, Unjoinable> {
    let mut list = element::iter(bytes);
    match (list.next(), list.next()) {
        (Some(Ok(e)), None) if e.id == id => {
            Elements::parse(bytes).map_err(|_| Unjoinable::Element)
        }
        _ => Err(Unjoinable::Element),
    }
}

fn rsn<'a>(bytes: &'a [u8]) -> Result<Rsn<'a>, Unjoinable> {
    single(bytes, Id::RSN)?.rsn.ok_or(Unjoinable::Element)
}

/// Whether a suite list is `want` alone, an absent one meaning `default`: the RSNE's defaults are
/// CCMP-128 for the ciphers, 802.1X for the AKM and BIP-CMAC-128 for group management.
fn only(suites: Option<Suites<'_>>, default: Suite, want: Suite) -> bool {
    match suites {
        Some(s) => s.iter().eq([want]),
        None => default == want,
    }
}

/// Whether a suite list holds `want`, an absent one meaning `default`.
fn offers(suites: Option<Suites<'_>>, default: Suite, want: Suite) -> bool {
    match suites {
        Some(s) => s.contains(want),
        None => default == want,
    }
}

impl Supplicant {
    pub fn new(pmk: PairwiseMasterKey, join: &Join<'_>) -> Result<Self, Unjoinable> {
        let ap = rsn(join.beacon_rsne)?;
        let own = rsn(join.own_rsne)?;
        for rsnxe in [join.beacon_rsnxe, join.own_rsnxe].into_iter().flatten() {
            single(rsnxe, Id::RSNX)?;
        }
        if !only(own.akm, Suite::AKM_8021X, Suite::AKM_PSK)
            || !offers(ap.akm, Suite::AKM_8021X, Suite::AKM_PSK)
        {
            return Err(Unjoinable::Akm);
        }
        if !only(own.pairwise, Suite::CCMP_128, Suite::CCMP_128)
            || !offers(ap.pairwise, Suite::CCMP_128, Suite::CCMP_128)
        {
            return Err(Unjoinable::Pairwise);
        }
        for group in [own.group_data, ap.group_data] {
            if group.unwrap_or(Suite::CCMP_128) != Suite::CCMP_128 {
                return Err(Unjoinable::Group);
            }
        }
        if (ap.mfp_required() && !own.mfp_capable()) || (own.mfp_required() && !ap.mfp_capable()) {
            return Err(Unjoinable::Protection);
        }
        let protected = own.mfp_capable() && ap.mfp_capable();
        if protected {
            for management in [own.group_management, ap.group_management] {
                if management.unwrap_or(Suite::BIP_CMAC_128) != Suite::BIP_CMAC_128 {
                    return Err(Unjoinable::Management);
                }
            }
        }
        let mut own_key_data = join.own_rsne.to_vec();
        own_key_data.extend_from_slice(join.own_rsnxe.unwrap_or_default());
        Ok(Self {
            pmk,
            authenticator: join.authenticator,
            supplicant: join.supplicant,
            beacon_rsne: join.beacon_rsne.to_vec(),
            beacon_rsnxe: join.beacon_rsnxe.map(<[u8]>::to_vec),
            own: own_key_data,
            protected,
            replay: None,
            pending: None,
            current: None,
            gtk: None,
            igtk: None,
        })
    }

    /// Whether management frame protection was negotiated, so an IGTK is required.
    pub fn protected(&self) -> bool {
        self.protected
    }

    /// Answers one EAPOL-Key frame from the authenticator: an EAPOL PDU, from its Protocol
    /// Version octet. `nonce` is asked for 32 random octets only when a handshake starts.
    pub fn receive(
        &mut self,
        frame: &[u8],
        nonce: impl FnOnce() -> [u8; 32],
    ) -> Result<Step, Refused> {
        let key = Key::parse(frame)?;
        let info = key.info;
        if !info.has(Info::ACK) || info.has(Info::REQUEST) || info.has(Info::ERROR) {
            return Err(Refused::NotFromAuthenticator);
        }
        if self.replay.is_some_and(|last| key.replay <= last) {
            return Err(Refused::Replayed);
        }
        match (info.has(Info::PAIRWISE), info.has(Info::MIC)) {
            (true, false) => self.message_1(&key, nonce),
            (true, true) => self.message_3(&key),
            (false, true) => self.group_1(&key),
            (false, false) => Err(Refused::UnknownMessage),
        }
    }

    fn message_1(
        &mut self,
        key: &Key<'_>,
        nonce: impl FnOnce() -> [u8; 32],
    ) -> Result<Step, Refused> {
        if key.info.has(Info::INSTALL) || key.info.has(Info::ENCRYPTED) {
            return Err(if key.info.has(Info::ENCRYPTED) {
                Refused::Plaintext
            } else {
                Refused::UnknownMessage
            });
        }
        if key.key_length != 16 {
            return Err(Refused::KeyLength);
        }
        let snonce = self.pending.as_ref().map_or_else(nonce, |p| p.snonce);
        let ptk = Ptk::derive(
            &self.pmk,
            self.authenticator,
            self.supplicant,
            &key.nonce,
            &snonce,
        );
        let reply = Reply {
            version: key.version,
            info: Info::PAIRWISE,
            replay: key.replay,
            nonce: &snonce,
            data: &self.own,
        }
        .seal(ptk.kck());
        self.pending = Some(Handshake {
            anonce: key.nonce,
            snonce,
            ptk,
        });
        Ok(Step {
            reply,
            pairwise: None,
            group: None,
            integrity: None,
        })
    }

    fn message_3(&mut self, key: &Key<'_>) -> Result<Step, Refused> {
        let info = key.info;
        if !info.has(Info::INSTALL) || !info.has(Info::SECURE) {
            return Err(Refused::UnknownMessage);
        }
        if key.key_length != 16 {
            return Err(Refused::KeyLength);
        }
        let (fresh, ptk) = match (&self.pending, &self.current) {
            (Some(p), _) if p.anonce == key.nonce => (true, &p.ptk),
            (_, Some(c)) if c.anonce == key.nonce => (false, &c.ptk),
            (None, None) => return Err(Refused::NoMessage1),
            _ => return Err(Refused::AnonceChanged),
        };
        if !key.mic_verifies(ptk.kck()) {
            return Err(Refused::Mic);
        }
        self.replay = Some(key.replay);
        let mut unwrapped = Secret::new([0; KEY_DATA_MAX]);
        let data = unwrap(key, ptk.kek(), unwrapped.expose_mut())?;
        if data.rsne != Some(self.beacon_rsne.as_slice()) {
            return Err(Refused::RsneDiffers);
        }
        if data.rsnxe != self.beacon_rsnxe.as_deref() {
            return Err(Refused::RsnxeDiffers);
        }
        let (group, integrity) = self.group_keys(&data, key.rsc)?;
        let reply = Reply {
            version: key.version,
            info: Info::PAIRWISE | Info::SECURE,
            replay: key.replay,
            nonce: &[0; 32],
            data: &[],
        }
        .seal(ptk.kck());
        let pairwise = if fresh {
            let handshake = self.pending.take().expect("message 3 matched it");
            let tk = PairwiseKey(Secret::new(*handshake.ptk.tk()));
            self.current = Some(handshake);
            Some(tk)
        } else {
            None
        };
        Ok(Step {
            reply,
            pairwise,
            group: self.install_group(group),
            integrity: self.install_integrity(integrity),
        })
    }

    fn group_1(&mut self, key: &Key<'_>) -> Result<Step, Refused> {
        if key.info.has(Info::INSTALL) || !key.info.has(Info::SECURE) {
            return Err(Refused::UnknownMessage);
        }
        let ptk = &self.current.as_ref().ok_or(Refused::NoPairwiseKey)?.ptk;
        if !key.mic_verifies(ptk.kck()) {
            return Err(Refused::Mic);
        }
        self.replay = Some(key.replay);
        let mut unwrapped = Secret::new([0; KEY_DATA_MAX]);
        let data = unwrap(key, ptk.kek(), unwrapped.expose_mut())?;
        let (group, integrity) = self.group_keys(&data, key.rsc)?;
        let reply = Reply {
            version: key.version,
            info: Info::SECURE,
            replay: key.replay,
            nonce: &[0; 32],
            data: &[],
        }
        .seal(ptk.kck());
        Ok(Step {
            reply,
            pairwise: None,
            group: self.install_group(group),
            integrity: self.install_integrity(integrity),
        })
    }

    /// The GTK, and the IGTK when management frame protection was negotiated, checked.
    fn group_keys(
        &self,
        data: &KeyData<'_>,
        rsc: [u8; 8],
    ) -> Result<(GroupKey, Option<IntegrityKey>), Refused> {
        let gtk = data.gtk.as_ref().ok_or(Refused::NoGtk)?;
        if gtk.id == 0 {
            return Err(Refused::GroupKeyIdZero);
        }
        let group = GroupKey {
            id: gtk.id,
            key: Secret::new(*gtk.key),
            rsc: pn(rsc)?,
        };
        if !self.protected {
            return Ok((group, None));
        }
        let igtk = data.igtk.as_ref().ok_or(Refused::NoIgtk)?;
        if !matches!(igtk.id, 4 | 5) {
            return Err(Refused::IgtkKeyId);
        }
        let mut ipn = [0; 8];
        if let Some(low) = ipn.first_chunk_mut::<6>() {
            *low = igtk.ipn;
        }
        Ok((
            group,
            Some(IntegrityKey {
                id: igtk.id,
                key: Secret::new(*igtk.key),
                ipn: u64::from_le_bytes(ipn),
            }),
        ))
    }

    fn install_group(&mut self, key: GroupKey) -> Option<GroupKey> {
        if self
            .gtk
            .as_ref()
            .is_some_and(|(id, k)| *id == key.id && ct::equal(k.expose(), key.key()))
        {
            return None;
        }
        self.gtk = Some((key.id, Secret::new(*key.key())));
        Some(key)
    }

    fn install_integrity(&mut self, key: Option<IntegrityKey>) -> Option<IntegrityKey> {
        let key = key?;
        if self
            .igtk
            .as_ref()
            .is_some_and(|(id, k)| *id == key.id && ct::equal(k.expose(), key.key()))
        {
            return None;
        }
        self.igtk = Some((key.id, Secret::new(*key.key())));
        Some(key)
    }
}

/// Key RSC as CCMP's packet number: its first six octets, least significant first (§12.7.2).
fn pn(rsc: [u8; 8]) -> Result<u64, Refused> {
    let value = u64::from_le_bytes(rsc);
    if value >> 48 != 0 {
        return Err(Refused::Rsc);
    }
    Ok(value)
}

/// The Key Data of a message 3 or group message 1, unwrapped under `kek` into `out` and parsed.
fn unwrap<'a>(
    key: &Key<'_>,
    kek: &[u8; 16],
    out: &'a mut [u8; KEY_DATA_MAX],
) -> Result<KeyData<'a>, Refused> {
    if !key.info.has(Info::ENCRYPTED) {
        return Err(Refused::Plaintext);
    }
    let plain_len = key
        .data
        .len()
        .checked_sub(8)
        .ok_or(Refused::WrappedLength)?;
    let out = out.get_mut(..plain_len).ok_or(Refused::WrappedLength)?;
    KeyEncryptionKey::new(*kek)
        .unwrap(key.data, out)
        .map_err(|e| match e {
            toyos_crypto::Error::Authentication => Refused::Unwrap,
            _ => Refused::WrappedLength,
        })?;
    KeyData::parse(out)
}
