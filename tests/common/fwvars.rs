//! The firmware's variable store, as OVMF keeps it in its `VARS` file: a
//! firmware volume holding an authenticated variable store, read and written
//! by the layout EDK2 declares for it (`MdeModulePkg/Include/Guid/
//! VariableFormat.h`) and not by anything the loader shares.

use std::path::Path;

/// `EFI_FIRMWARE_VOLUME_HEADER`: its signature and its header's length.
const FV_SIGNATURE: (usize, &[u8]) = (0x28, b"_FVH");
const FV_HEADER_LEN_AT: usize = 0x30;
/// `VARIABLE_STORE_HEADER`: signature GUID, size, format, state, reserved.
const STORE_HEADER: usize = 16 + 4 + 1 + 1 + 2 + 4;
/// `AUTHENTICATED_VARIABLE_HEADER`: start id, state, reserved, attributes,
/// monotonic count, time stamp, public key index, name size, data size,
/// vendor GUID.
const HEADER: usize = 2 + 1 + 1 + 4 + 8 + 16 + 4 + 4 + 4 + 16;
const START_ID: u16 = 0x55AA;
const VAR_ADDED: u8 = 0x3F;
/// `VAR_ADDED & VAR_IN_DELETED_TRANSITION`: still the variable until the
/// copy replacing it is added.
const IN_TRANSITION: u8 = 0x3E;

pub struct Var {
    pub name: String,
    pub data: Vec<u8>,
}

/// Where the variables begin and where the store ends.
fn store(bytes: &[u8]) -> Result<(usize, usize), String> {
    let (at, sig) = FV_SIGNATURE;
    if bytes.get(at..at + sig.len()) != Some(sig) {
        return Err("the variable file is no firmware volume".into());
    }
    let header = u16::from_le_bytes([bytes[FV_HEADER_LEN_AT], bytes[FV_HEADER_LEN_AT + 1]]) as usize;
    let size = u32::from_le_bytes(bytes[header + 16..header + 20].try_into().expect("four bytes")) as usize;
    Ok((header + STORE_HEADER, header + size))
}

/// One variable header in the store.
struct Found {
    state: u8,
    vendor: [u8; 16],
    var: Var,
}

/// Every variable header in the store, and where the erased space after
/// them begins.
fn walk(bytes: &[u8]) -> Result<(Vec<Found>, usize), String> {
    let (mut at, end) = store(bytes)?;
    let mut out = Vec::new();
    while at + HEADER <= end && u16::from_le_bytes([bytes[at], bytes[at + 1]]) == START_ID {
        let word = |off: usize| u32::from_le_bytes(bytes[at + off..at + off + 4].try_into().expect("four bytes")) as usize;
        let (name_len, data_len) = (word(36), word(40));
        let vendor: [u8; 16] = bytes[at + 44..at + 60].try_into().expect("sixteen bytes");
        let name_at = at + HEADER;
        let units: Vec<u16> = bytes[name_at..name_at + name_len]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&u| u != 0)
            .collect();
        let data = bytes[name_at + name_len..name_at + name_len + data_len].to_vec();
        out.push(Found { state: bytes[at + 2], vendor, var: Var { name: String::from_utf16_lossy(&units), data } });
        at = (name_at + name_len + data_len).next_multiple_of(4);
    }
    Ok((out, at))
}

/// The live variables under `vendor`, a GUID in the byte order `EFI_GUID`
/// stores.
pub fn live(path: &Path, vendor: &[u8; 16]) -> Result<Vec<Var>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(walk(&bytes)?
        .0
        .into_iter()
        .filter(|found| found.vendor == *vendor && (found.state == VAR_ADDED || found.state == IN_TRANSITION))
        .map(|found| found.var)
        .collect())
}

/// Add `name` under `vendor` with `attributes` and `data`, as the firmware
/// would have added it.
pub fn plant(path: &Path, vendor: &[u8; 16], name: &str, attributes: u32, data: &[u8]) -> Result<(), String> {
    let mut bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let (_, end) = store(&bytes)?;
    let (_, at) = walk(&bytes)?;
    let mut units: Vec<u8> = name.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
    let mut var = vec![0u8; HEADER];
    var[..2].copy_from_slice(&START_ID.to_le_bytes());
    var[2] = VAR_ADDED;
    var[4..8].copy_from_slice(&attributes.to_le_bytes());
    var[36..40].copy_from_slice(&(units.len() as u32).to_le_bytes());
    var[40..44].copy_from_slice(&(data.len() as u32).to_le_bytes());
    var[44..60].copy_from_slice(vendor);
    var.append(&mut units);
    var.extend_from_slice(data);
    if at + var.len() > end || bytes[at..at + var.len()].iter().any(|&b| b != 0xFF) {
        return Err(format!("no erased room for {name} at byte {at} of the variable store"));
    }
    bytes[at..at + var.len()].copy_from_slice(&var);
    std::fs::write(path, bytes).map_err(|e| format!("{}: {e}", path.display()))
}
