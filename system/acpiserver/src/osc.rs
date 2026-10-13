//! `_OSC` negotiated (ACPI 6.5 §6.2.11; Intel 302223-007 §2.2): `\_SB._OSC`
//! under the platform-wide UUID (§6.2.11.2), then, on an Intel processor,
//! each processor's `_OSC` under Intel's, each a query with the Query flag
//! and then a commit of what the query granted (§6.2.11.1.1, §6.2.11.1.3).
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
const fn uuid(text: &[u8; 36]) -> [u8; 16] {
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
/// none. Bit 0: the kernel enables HWP itself
/// (`kernel/src/arch/x86_64/control_regs.rs`), and writes no `IA32_PERF_CTL`
/// through `_PCT`. Bit 12: the kernel takes no HWP interrupt. Bits 8 and 9:
/// the kernel enters no `MWAIT` state but C1's, and reads no `_CST`.
pub const PROCESSOR: Claim = Claim { uuid: uuid(b"4077A616-290C-47BE-9EBD-D87058713953"), capabilities: 0 };

/// The first DWORD's bits (§6.2.11): passed, the Query flag; answered, the
/// status.
const QUERY: u32 = 1 << 0;
const FAILURE: u32 = 1 << 1;
const UNRECOGNIZED_UUID: u32 = 1 << 2;
const UNRECOGNIZED_REVISION: u32 = 1 << 3;
const MASKED: u32 = 1 << 4;

/// The revision of both UUIDs' capabilities buffer (§6.2.11.2; 302223-007
/// §2.2).
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

/// One evaluation of `path` with the two DWORDs, its answer's two DWORDs.
fn evaluate<K: Kernel>(aml: &mut Aml<'_, K>, path: &str, uuid: &[u8; 16], first: u32, second: u32) -> Result<[u32; 2], String> {
    let capabilities = [first.to_le_bytes(), second.to_le_bytes()].concat();
    let args = [Value::Buffer(uuid.to_vec()), Value::Integer(REVISION), Value::Integer(2), Value::Buffer(capabilities)];
    match aml.interpreter.evaluate(&mut aml.host, path, &args) {
        Ok(Value::Buffer(answer)) if answer.len() == 8 => {
            let dword = |at: usize| u32::from_le_bytes(answer[at..at + 4].try_into().expect("four bytes"));
            Ok([dword(0), dword(4)])
        }
        Ok(Value::Buffer(answer)) => Err(format!("its answer is {} bytes, where 8 were passed (§6.2.11)", answer.len())),
        Ok(_) => Err("its answer is no Buffer (§6.2.11)".into()),
        Err(why) => {
            println!("{OWN}{path} was refused {why:x?}");
            Err(kind(&why))
        }
    }
}

/// `path`'s `_OSC` negotiated under `claim`: the query, and the commit of
/// what it granted that `claim` asked for.
pub fn negotiate<K: Kernel>(aml: &mut Aml<'_, K>, path: &str, claim: &Claim) -> Result<Answered, String> {
    let [query, offered] = evaluate(aml, path, &claim.uuid, QUERY, claim.capabilities)?;
    let offered = offered & claim.capabilities;
    if query & (FAILURE | UNRECOGNIZED_UUID) != 0 {
        return Ok(Answered { query, offered, commit: None });
    }
    let [commit, granted] = evaluate(aml, path, &claim.uuid, 0, offered)?;
    Ok(Answered { query, offered, commit: Some((commit, granted & offered)) })
}

/// What the negotiation answered, for the line the server says.
#[derive(Default)]
pub struct Negotiated {
    pub platform: Option<Result<Answered, String>>,
    /// Each processor's, in walk order.
    pub processors: Vec<(String, Result<Answered, String>)>,
}

/// `\_SB._OSC`, then each processor's `_OSC` where the processor is Intel's
/// (`intel`): every
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
            toyos_aml::Kind::Processor => true,
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
        let answered = negotiate(aml, &path, &PROCESSOR);
        match &answered {
            Ok(answered) => println!("{OWN}{path} negotiated: {}", answered.said(PROCESSOR.capabilities)),
            Err(why) => println!("{OWN}{path} was refused: {why}"),
        }
        negotiated.processors.push((processor, answered));
        if aml.host.stopping {
            break;
        }
    }
    let refused = negotiated.processors.iter().filter(|(_, answered)| answered.is_err()).count();
    let granted = negotiated.processors.iter().filter_map(|(_, answered)| answered.as_ref().ok()?.commit).fold(0, |all, (_, granted)| all | granted);
    println!(
        "acpiserver: the processors' _OSC negotiated, Intel's: {} evaluated, query then commit, {refused} refused; granted {granted:#x} of {:#x}",
        negotiated.processors.len(),
        PROCESSOR.capabilities
    );
    negotiated
}

/// Whether the device at `path` is a processor by its `_HID` (§8.4):
/// ACPI0007.
fn processor_device<K: Kernel>(aml: &mut Aml<'_, K>, names: &Names, path: &str) -> bool {
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
