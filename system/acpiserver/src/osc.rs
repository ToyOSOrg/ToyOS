//! `_OSC` negotiated (ACPI 6.5 §6.2.11; Intel 302223-007 §2.2): `\_SB._OSC`
//! under the platform-wide UUID (§6.2.11.2), a query with the Query flag and
//! then a commit of what the query granted (§6.2.11.1.1, §6.2.11.1.3); then,
//! on an Intel processor, each processor's `_OSC` under Intel's, committed
//! with what ToyOS claims and no query first; and each PCI root bridge's
//! under the PCI host bridge UUID, a query and, where it offers native PCI
//! Express hot-plug with the capability structure, the commit of both, which
//! the kernel's hot-plug slots act on ([`negotiate_root`], PCI Firmware
//! Specification 3.3 §4.5), by `crate::apertures`.
//!
//! **A processor's bits are statements, not requests for control**: each of
//! 302223-007 Table 1 says what OSPM "is capable of", and "ACPI firmware will
//! interpret and record the set bits" (§2.1). ACPI 6.5 §6.2.11.1.1 "strongly
//! recommends" the query, for negotiating control; it does not require it,
//! and what the firmware does with a statement is in the objects it then
//! returns (`_CST` among them, [`crate::cst`]). A query some firmware answers
//! with the same bit whatever was asked, nothing included, would otherwise
//! decide what ToyOS says it does. So a processor commit that fails, is
//! refused or masks bits gates nothing: `_CST` is read either way, and each
//! state it returns is checked on its own terms.
//!
//! **What ToyOS claims is one declaration a UUID** ([`PLATFORM`],
//! [`PROCESSOR`]), each bit set only where ToyOS does what the bit promises,
//! the owner's condition on `_OSI` ("the interpreter must then actually
//! support what it claims") held to `_OSC`.
//!
//! **The answer is untrusted**: a buffer of the length passed, or the
//! negotiation is refused by name; its status bits are read by name; a bit
//! granted that was not asked for is dropped, and the commit sends at most
//! what the declaration holds. A query that answers failure or an
//! unrecognised UUID is not committed. The query is made once and not
//! repeated while the platform answers Capabilities Masked, as §6.2.11.1.1
//! recommends: a firmware that masks every set but one never ends it.
//!
//! What each negotiation answered is said: the platform's on one line, the
//! processors' counted on one, each processor's path under [`OWN`].

use toyos_aml::Value;

use crate::aml::{kind, Aml};
use crate::host::{Kernel, OWN};
use crate::init::Names;

/// A UUID's 16 bytes from its text, `aabbccdd-eeff-gghh-iijj-kkllmmnnoopp`,
/// as ToUUID lays them out (Table 19.41): the first three fields' bytes
/// reversed, the rest in order.
pub(crate) const fn uuid(text: &[u8; 36]) -> [u8; 16] {
    const fn digit(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'A'..=b'F' => c - b'A' + 10,
            b'a'..=b'f' => c - b'a' + 10,
            _ => panic!("a UUID's digit"),
        }
    }
    const ORDER: [usize; 16] = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
    let mut bytes = [0u8; 16];
    let (mut at, mut i) = (0, 0);
    while i < 16 {
        if text[at] == b'-' {
            at += 1;
        }
        bytes[ORDER[i]] = digit(text[at]) << 4 | digit(text[at + 1]);
        at += 2;
        i += 1;
    }
    bytes
}

/// One UUID's claim: what OSPM passes in the second DWORD.
pub struct Claim {
    pub uuid: [u8; 16],
    pub capabilities: u32,
}

/// The platform-wide capabilities (§6.2.11.2, Table 6.13) ToyOS honours:
/// none. Bit 0, the Processor Aggregator Device: no `ACPI000C` is served.
/// Bits 1 to 22: nothing reads `_PPC`, `_CPC`, `_PRS`, `_PDC` or any object
/// those bits announce support for.
pub const PLATFORM: Claim = Claim { uuid: uuid(b"0811B06E-4A27-44F9-8D60-3CBBC22E7B48"), capabilities: 0 };

/// A processor's capabilities on Intel (302223-007 Table 1) ToyOS honours:
/// bit 4, each processor entering its C2 and C3 states independently, and
/// bit 9, native C-state instructions (`MWAIT`) for them
/// (`kernel/src/idle.rs`). Not bit 0: the kernel enables HWP itself
/// (`kernel/src/arch/x86_64/control_regs.rs`), and writes no `IA32_PERF_CTL`
/// through `_PCT`. Not bit 6: no C-state is coordinated in software through
/// `_CSD`. Not bit 8: C1 is `hlt`. Not bit 12: the kernel takes no HWP
/// interrupt.
pub const PROCESSOR: Claim = Claim { uuid: uuid(b"4077A616-290C-47BE-9EBD-D87058713953"), capabilities: 1 << 4 | 1 << 9 };

/// The first DWORD's bits (§6.2.11): passed, the Query flag; answered, the
/// status.
const QUERY: u32 = 1 << 0;
const FAILURE: u32 = 1 << 1;
const UNRECOGNIZED_UUID: u32 = 1 << 2;
const UNRECOGNIZED_REVISION: u32 = 1 << 3;
const MASKED: u32 = 1 << 4;

/// The revision of every UUID's capabilities buffer here (§6.2.11.2;
/// 302223-007 §2.2; PCI Firmware Specification 3.3 §4.5.1).
const REVISION: u64 = 1;

/// A status DWORD's bits by name.
fn status(dword: u32) -> String {
    let named = [(FAILURE, "failure"), (UNRECOGNIZED_UUID, "unrecognized UUID"), (UNRECOGNIZED_REVISION, "unrecognized revision"), (MASKED, "capabilities masked")];
    let set: Vec<&str> = named.iter().filter(|(bit, _)| dword & bit != 0).map(|(_, name)| *name).collect();
    let rest = dword & !(QUERY | FAILURE | UNRECOGNIZED_UUID | UNRECOGNIZED_REVISION | MASKED);
    match (set.is_empty(), rest) {
        (true, 0) => "nothing".into(),
        (_, 0) => set.join(", "),
        _ => format!("{}{}reserved bits {rest:#x}", set.join(", "), if set.is_empty() { "" } else { ", " }),
    }
}

/// What one `_OSC` answered: the query's status and grant, and the commit's
/// status and grant, where one was made.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Answered {
    pub query: u32,
    pub offered: u32,
    pub commit: Option<(u32, u32)>,
}

impl Answered {
    fn said(&self, asked: u32) -> String {
        let commit = match self.commit {
            Some((status_word, granted)) => format!("the commit answered {} and granted {granted:#x}", status(status_word)),
            None => "no commit was made".into(),
        };
        format!("the query answered {} and offered {:#x} of {asked:#x}; {commit}", status(self.query), self.offered)
    }
}

/// One evaluation of `path` with `N` DWORDs, its answer's `N` DWORDs.
fn evaluate<K: Kernel, const N: usize>(aml: &mut Aml<'_, K>, path: &str, uuid: &[u8; 16], dwords: [u32; N]) -> Result<[u32; N], String> {
    let capabilities: Vec<u8> = dwords.iter().flat_map(|dword| dword.to_le_bytes()).collect();
    let args = [Value::Buffer(uuid.to_vec()), Value::Integer(REVISION), Value::Integer(N as u64), Value::Buffer(capabilities)];
    match aml.interpreter.evaluate(&mut aml.host, path, &args) {
        Ok(Value::Buffer(answer)) if answer.len() == 4 * N => {
            Ok(core::array::from_fn(|i| u32::from_le_bytes(answer[4 * i..4 * i + 4].try_into().expect("four bytes"))))
        }
        Ok(Value::Buffer(answer)) => Err(format!("its answer is {} bytes, where {} were passed (§6.2.11)", answer.len(), 4 * N)),
        Ok(_) => Err("its answer is no Buffer (§6.2.11)".into()),
        Err(why) => {
            println!("{OWN}{path} was refused {why:x?}");
            Err(kind(&why))
        }
    }
}

/// What a processor's commit answered: its status, and what it acknowledged
/// of what was claimed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Committed {
    pub status: u32,
    pub granted: u32,
}

/// `path`'s `_OSC` committed with `claim` whole, no query first.
pub fn commit<K: Kernel>(aml: &mut Aml<'_, K>, path: &str, claim: &Claim) -> Result<Committed, String> {
    let [status, granted] = evaluate(aml, path, &claim.uuid, [0, claim.capabilities])?;
    Ok(Committed { status, granted: granted & claim.capabilities })
}

/// `path`'s `_OSC` negotiated under `claim`: the query, and the commit of
/// what it granted that `claim` asked for.
pub fn negotiate<K: Kernel>(aml: &mut Aml<'_, K>, path: &str, claim: &Claim) -> Result<Answered, String> {
    let [query, offered] = evaluate(aml, path, &claim.uuid, [QUERY, claim.capabilities])?;
    let offered = offered & claim.capabilities;
    if query & (FAILURE | UNRECOGNIZED_UUID) != 0 {
        return Ok(Answered { query, offered, commit: None });
    }
    let [commit, granted] = evaluate(aml, path, &claim.uuid, [0, offered])?;
    Ok(Answered { query, offered, commit: Some((commit, granted & offered)) })
}

/// The PCI host bridge UUID (PCI Firmware Specification 3.3 §4.5.1).
const HOST_BRIDGE: [u8; 16] = uuid(b"33DB4D5B-1FF7-401C-9657-7441C03DD766");

/// The Support field ToyOS states for a root (§4.5.1, Table 4-4): extended
/// configuration space, which the kernel reaches through ECAM (bit 0), and
/// MSI, which every claim is armed with (bit 4). Nothing else it does.
const ROOT_SUPPORT: u32 = 1 << 0 | 1 << 4;

/// The Control field ToyOS asks of a root (Table 4-5): native PCI Express
/// hot-plug, and the PCI Express capability structure it needs with it.
pub const ROOT_CONTROL: u32 = toyos_abi::acpi::NATIVE_HOT_PLUG | toyos_abi::acpi::CAPABILITY_STRUCTURE;

/// What a root's `_OSC` at `path` offers of [`ROOT_CONTROL`] (§4.5.1), and
/// what a commit was granted: the query, then, where `commit` allows it, a
/// commit of [`ROOT_CONTROL`] only where the query offered all of it, since
/// the kernel's slots need both bits and take nothing less. Nothing is offered where the query
/// answered failure, an unrecognised UUID or revision; a commit's grant is
/// what it answered of what was offered, nothing where it answered failure.
/// An answer that is no buffer of the three DWORDs passed refuses the
/// negotiation by name.
pub fn negotiate_root<K: Kernel>(aml: &mut Aml<'_, K>, path: &str, commit: bool) -> Result<(u32, Option<u32>), String> {
    const REFUSED: u32 = FAILURE | UNRECOGNIZED_UUID | UNRECOGNIZED_REVISION;
    let [query, _, offered] = evaluate(aml, path, &HOST_BRIDGE, [QUERY, ROOT_SUPPORT, ROOT_CONTROL])?;
    if query & REFUSED != 0 {
        println!("{OWN}{path}'s query answered {}", status(query));
        return Ok((0, None));
    }
    let offered = offered & ROOT_CONTROL;
    if offered != ROOT_CONTROL || !commit {
        return Ok((offered, None));
    }
    let [commit, _, granted] = evaluate(aml, path, &HOST_BRIDGE, [0, ROOT_SUPPORT, ROOT_CONTROL])?;
    if commit & REFUSED != 0 {
        println!("{OWN}{path}'s commit answered {}", status(commit));
        return Ok((offered, Some(0)));
    }
    Ok((offered, Some(granted & offered)))
}

/// What the negotiation answered, for the line the server says.
#[derive(Default)]
pub struct Negotiated {
    pub platform: Option<Result<Answered, String>>,
    /// Each processor's, in walk order.
    pub processors: Vec<(String, Result<Committed, String>)>,
}

/// `\_SB._OSC`, then each processor's `_OSC` committed where the processor is
/// Intel's (`intel`): every
/// `Processor` object and every `ACPI0007` device (ACPI 6.5 §8.4) that names
/// one, in the namespace's order.
pub fn negotiate_all<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, intel: bool) -> Negotiated {
    let mut negotiated = Negotiated::default();
    if names.paths.contains("\\_SB_._OSC") {
        let answered = negotiate(aml, "\\_SB_._OSC", &PLATFORM);
        match &answered {
            Ok(answered) => println!("acpiserver: \\_SB._OSC negotiated: {}", answered.said(PLATFORM.capabilities)),
            Err(why) => println!("acpiserver: \\_SB._OSC was refused: {why}"),
        }
        negotiated.platform = Some(answered);
    }
    let processors: Vec<String> = names
        .objects
        .iter()
        .filter(|named| match named.kind {
            toyos_aml::Kind::Processor { .. } => true,
            toyos_aml::Kind::Device => processor_device(aml, names, &named.path),
            _ => false,
        })
        .filter(|named| names.paths.contains(&format!("{}._OSC", named.path)))
        .map(|named| named.path.clone())
        .collect();
    if processors.is_empty() || aml.host.stopping {
        return negotiated;
    }
    if !intel {
        println!("acpiserver: {} processors' _OSC are not negotiated: the processor is not Intel's, whose interface their UUID names", processors.len());
        return negotiated;
    }
    for processor in processors {
        let path = format!("{processor}._OSC");
        let answered = commit(aml, &path, &PROCESSOR);
        match &answered {
            Ok(c) => println!("{OWN}{path} committed {:#x}: it answered {} and acknowledged {:#x}", PROCESSOR.capabilities, status(c.status), c.granted),
            Err(why) => println!("{OWN}{path} was refused: {why}"),
        }
        negotiated.processors.push((processor, answered));
        if aml.host.stopping {
            break;
        }
    }
    let refused = negotiated.processors.iter().filter(|(_, answered)| answered.is_err()).count();
    let granted = negotiated.processors.iter().filter_map(|(_, answered)| answered.as_ref().ok()).fold(0, |all, c| all | c.granted);
    println!(
        "acpiserver: the processors' _OSC committed, Intel's: {} evaluated, {refused} refused; acknowledged {granted:#x} of {:#x}",
        negotiated.processors.len(),
        PROCESSOR.capabilities
    );
    negotiated
}

/// Whether the device at `path` is a processor by its `_HID` (§8.4):
/// ACPI0007.
pub fn processor_device<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, path: &str) -> bool {
    let hid = format!("{path}._HID");
    names.paths.contains(&hid) && aml.interpreter.evaluate(&mut aml.host, &hid, &[]).ok().as_ref().and_then(crate::devices::id).as_deref() == Some("ACPI0007")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Table 19.41's layout, against the bytes the first DWORD of each UUID
    /// and the rest read as.
    #[test]
    fn a_uuid_is_laid_out_as_to_uuid_lays_it_out() {
        assert_eq!(PLATFORM.uuid, [0x6E, 0xB0, 0x11, 0x08, 0x27, 0x4A, 0xF9, 0x44, 0x8D, 0x60, 0x3C, 0xBB, 0xC2, 0x2E, 0x7B, 0x48]);
        assert_eq!(PROCESSOR.uuid, [0x16, 0xA6, 0x77, 0x40, 0x0C, 0x29, 0xBE, 0x47, 0x9E, 0xBD, 0xD8, 0x70, 0x58, 0x71, 0x39, 0x53]);
    }

    #[test]
    fn a_status_is_read_by_name() {
        assert_eq!(status(0), "nothing");
        assert_eq!(status(MASKED | QUERY), "capabilities masked");
        assert_eq!(status(FAILURE | UNRECOGNIZED_UUID | 1 << 9), "failure, unrecognized UUID, reserved bits 0x200");
    }
}
