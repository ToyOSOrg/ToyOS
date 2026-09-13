//! What netd holds of the network it is on, and the one word a boot with no
//! console has to say it with.
//!
//! **The channel is a process's exit code**, thirty-two bits against the
//! pair's eighty, so what crosses is a fold of the pair: the judge already
//! holds what the pair must be and recomputes the same fold.
//!
//! Three crates read this file and none of them shares another's: netd answers
//! [`ASK`], the job that asked turns the answer into an exit code, and the
//! harness reads that code back out of the record.

#![no_std]
#![forbid(unsafe_code)]

#[cfg(test)]
extern crate std;

use core::net::Ipv4Addr;

/// The request netd answers with [`State::encode`]'s bytes.
///
/// **Not one of `toyos::net::MsgType`'s words**: nothing that *uses* the
/// network sends it — it is how a machine whose output reaches nobody reports
/// the network it is on — and it is held clear of every word the SDK does send
/// by `toyos_build::lan`'s scan of that file.
pub const ASK: u32 = 0x4c_41_4e;

/// netd's answer: the MAC of the card it drives, then the address its lease
/// gave this machine.
pub const ANSWER_LEN: usize = 10;

/// What netd holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub mac: [u8; 6],
    /// `None` where this machine took no lease.
    pub address: Option<Ipv4Addr>,
}

impl State {
    /// The answer as netd writes it. An absent address is four zero bytes,
    /// which no lease is: RFC 1122 §3.2.1.3 gives 0.0.0.0 to a host that does
    /// not yet know its own address.
    pub fn encode(&self) -> [u8; ANSWER_LEN] {
        let mut out = [0u8; ANSWER_LEN];
        out[..6].copy_from_slice(&self.mac);
        if let Some(address) = self.address {
            out[6..].copy_from_slice(&address.octets());
        }
        out
    }

    /// The answer as the job reads it. **A frame of any other length is not an
    /// answer at all**, refused where the length is read rather than here: this
    /// takes the bytes as a whole answer and so cannot fail.
    pub fn decode(bytes: &[u8; ANSWER_LEN]) -> Self {
        let mut mac = [0u8; 6];
        mac.copy_from_slice(&bytes[..6]);
        let address = Ipv4Addr::from([bytes[6], bytes[7], bytes[8], bytes[9]]);
        Self { mac, address: (!address.is_unspecified()).then_some(address) }
    }

    /// The code a job that got this answer exits with.
    pub fn code(&self) -> i32 {
        match self.address {
            Some(address) => fingerprint(self.mac, address),
            None => Refusal::NoLease.code(),
        }
    }
}

/// Why a job has no state to report, as the negative exit codes the host reads.
///
/// **This grammar reads `lan_state`'s exit code and no other**, as
/// `toyos_build::metaldevices::Refused` reads `metalprobe`'s: the same negative
/// numbers name different refusals in the two, and which one an `exit:` record
/// belongs to is the binary that wrote it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// This program's namespace holds no netd, or netd has gone.
    NoNetd = -1,
    /// netd took the question and refused it, or hung up on it.
    Unanswered = -2,
    /// netd answered bytes this grammar does not read.
    Malformed = -3,
    /// netd drives a card and this machine has no address.
    NoLease = -4,
}

impl Refusal {
    pub fn code(self) -> i32 {
        self as i32
    }

    /// The whole finding, because the code is the whole of what the host has.
    pub fn why(self) -> &'static str {
        match self {
            Self::NoNetd => "the job's namespace held no netd, or netd had already gone",
            Self::Unanswered => "netd refused the question or hung up on it",
            Self::Malformed => "netd answered bytes this grammar does not read",
            Self::NoLease => "netd drove the card and this machine had no address: no lease",
        }
    }

    fn from_code(code: i32) -> Option<Self> {
        [Self::NoNetd, Self::Unanswered, Self::Malformed, Self::NoLease]
            .into_iter()
            .find(|refusal| refusal.code() == code)
    }
}

/// The lowest fingerprint.
///
/// **A code below it is no word this grammar wrote.** A job the kernel killed
/// or one that panicked exits with a small positive number, and read as a
/// fingerprint that merely did not match it would be diagnosed as a card swap.
pub const FIRST_FINGERPRINT: i32 = 1 << 30;

/// The pair folded into the thirty bits left over [`FIRST_FINGERPRINT`], FNV-1a
/// (Fowler–Noll–Vo, 32 bit) over the MAC and then the address.
pub fn fingerprint(mac: [u8; 6], address: Ipv4Addr) -> i32 {
    const OFFSET_BASIS: u32 = 0x811c_9dc5;
    const PRIME: u32 = 0x0100_0193;
    let mut hash = OFFSET_BASIS;
    for byte in mac.iter().chain(address.octets().iter()) {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    let width = FIRST_FINGERPRINT as u32 - 1;
    FIRST_FINGERPRINT | (hash & width) as i32
}

/// What one exit code says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Said {
    /// The fold of the pair netd held.
    Fingerprint(i32),
    Refused(Refusal),
    /// A code no job of this family wrote.
    Foreign(i32),
}

/// Read one `exit: <name> pid=N code=<code>` record's code.
pub fn said(code: i32) -> Said {
    if code >= FIRST_FINGERPRINT {
        return Said::Fingerprint(code);
    }
    match Refusal::from_code(code) {
        Some(refusal) => Said::Refused(refusal),
        None => Said::Foreign(code),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAC: [u8; 6] = [0x54, 0xbf, 0x64, 0x2f, 0x0a, 0x1c];
    const ADDR: Ipv4Addr = Ipv4Addr::new(192, 168, 1, 42);

    #[test]
    fn an_answer_survives_the_wire_whether_or_not_it_carries_a_lease() {
        for address in [Some(ADDR), None] {
            let state = State { mac: MAC, address };
            assert_eq!(State::decode(&state.encode()), state);
        }
        // The absence is four zero bytes and not a shorter answer: a reader
        // that trusted the length would take a truncated frame for a lease.
        assert_eq!(State { mac: MAC, address: None }.encode()[6..], [0, 0, 0, 0]);
    }

    /// **The band is what tells a verdict from an accident.** Every code a
    /// process can leave that this grammar did not write — a zero exit, a
    /// panic, a kill — reads as foreign rather than as a fingerprint that
    /// disagreed, which is a different finding.
    #[test]
    fn a_code_no_job_of_this_family_wrote_is_refused_as_foreign() {
        for code in [0, 1, 101, 139, FIRST_FINGERPRINT - 1, -5, i32::MIN] {
            assert_eq!(said(code), Said::Foreign(code), "{code}");
        }
        assert_eq!(said(FIRST_FINGERPRINT), Said::Fingerprint(FIRST_FINGERPRINT));
    }

    #[test]
    fn every_refusal_reaches_the_judge_by_its_own_name() {
        for refusal in [Refusal::NoNetd, Refusal::Unanswered, Refusal::Malformed, Refusal::NoLease]
        {
            assert!(refusal.code() < 0, "{refusal:?}");
            assert_eq!(said(refusal.code()), Said::Refused(refusal));
            assert!(!refusal.why().is_empty());
        }
        assert_eq!(State { mac: MAC, address: None }.code(), Refusal::NoLease.code());
    }

    /// One byte of either half moves the fold, and the fold stays in the band:
    /// a fingerprint that could collide with a refusal or with a foreign code
    /// would make the grammar's three answers two.
    #[test]
    fn a_fingerprint_is_of_the_whole_pair_and_stays_in_its_band() {
        let whole = State { mac: MAC, address: Some(ADDR) }.code();
        assert_eq!(whole, fingerprint(MAC, ADDR));
        let mut moved = std::vec::Vec::new();
        for i in 0..6 {
            let mut mac = MAC;
            mac[i] = mac[i].wrapping_add(1);
            moved.push(fingerprint(mac, ADDR));
        }
        for i in 0..4 {
            let mut octets = ADDR.octets();
            octets[i] = octets[i].wrapping_add(1);
            moved.push(fingerprint(MAC, Ipv4Addr::from(octets)));
        }
        for other in &moved {
            assert_ne!(*other, whole);
            assert_eq!(said(*other), Said::Fingerprint(*other), "{other}");
        }
        // The order of the pair, and not only its content: a fold that merely
        // mixed the bytes together folds every permutation of one pair to one
        // code, and two cards that swapped a byte would agree.
        let mut swapped = MAC;
        swapped.swap(0, 1);
        assert_ne!(fingerprint(swapped, ADDR), whole);
        // The whole width of the band, and not the low byte of it.
        assert!(moved.iter().any(|got| got >> 8 != whole >> 8), "{moved:?}");
    }
}
