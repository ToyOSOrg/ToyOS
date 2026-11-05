//! Whether the last boot stopped cleanly, and which image it ran: the record
//! this loader writes on the log partition before each handoff
//! (`toyos_update::record`), and the mark `logkeeper` writes there once a stop
//! has synced its files (`toyos_logstream::STOPPED`).
//!
//! **The mark is taken before the next kernel runs, and the record is written
//! only once it is gone**, so a mark found here is the boot's that the record
//! names unless that pass's write of the record failed, which it says. Who may
//! write `/log` is `toyos_manifest::Program::view`'s to say; a boot that hangs
//! after its stop asked for the mark still leaves one.

use alloc::string::String;
use alloc::vec::Vec;
use toyos_update::record::{self, Booted};

use crate::efi::{cstr16, CStr16, CString16, File, Mode, Status, SystemTable};
use crate::loaderlog;

/// The record, at the root of the log partition beside `loader.log`.
const RECORD: &CStr16 = cstr16!("booted");

const HEAD: &str = "Proof:";

/// What [`take`] found and did.
pub struct Last {
    /// The image the last boot ran, where that boot stopped cleanly.
    pub proven: Option<Booted>,
    /// Lines to say once the log is open.
    pub notes: Vec<String>,
    /// Whether the mark is gone, which is what lets this pass write a record
    /// that a later mark can only be this boot's.
    pub unmarked: bool,
}

/// Read the last boot's record and mark, and take the mark.
///
/// Runs before `loaderlog::open`, which takes the volume exclusively.
pub fn take(system_table: &SystemTable, guid: &[u8; 16]) -> Last {
    let taken = loaderlog::with_volume(system_table, guid, |root| {
        let mut notes = Vec::new();
        let booted = match read(root, guid) {
            Ok(booted) => booted,
            Err(why) => {
                notes.push(alloc::format!("{HEAD} {RECORD} {why}, so no image is credited"));
                None
            }
        };
        let mark = CString16::new(toyos_logstream::STOPPED).expect("the mark's name is ASCII");
        let stopped = match root.open(&mark, Mode::ReadWrite) {
            Ok(file) => match file.delete() {
                Ok(()) => Ok(true),
                Err(e) => Err(alloc::format!("{} would not delete ({e})", toyos_logstream::STOPPED)),
            },
            Err(Status::NOT_FOUND) => Ok(false),
            Err(e) => Err(alloc::format!("{} would not open ({e})", toyos_logstream::STOPPED)),
        };
        let proven = match (&stopped, booted) {
            (Ok(true), Some(booted)) => {
                let mut hex = [0u8; 64];
                notes.push(alloc::format!(
                    "{HEAD} the last boot, of slot {}'s image {}, stopped cleanly",
                    booted.slot.letter(),
                    toyos_update::hex(&booted.digest, &mut hex)
                ));
                Some(booted)
            }
            (Ok(true), None) => {
                notes.push(alloc::format!("{HEAD} the last boot stopped cleanly, and no record names its image"));
                None
            }
            (Ok(false), _) => {
                notes.push(alloc::format!("{HEAD} the last boot left no stop mark, so it proves no image"));
                None
            }
            (Err(why), _) => {
                notes.push(alloc::format!("{HEAD} {why}, so no image is credited"));
                None
            }
        };
        Last { proven, notes, unmarked: stopped.is_ok() }
    });
    taken.unwrap_or_else(|why| Last {
        proven: None,
        notes: alloc::vec![alloc::format!("{HEAD} {why}, so no image is credited and none is recorded")],
        unmarked: false,
    })
}

/// Write down the image this pass boots, through the log's own open volume,
/// over the last record in place: the record is a fixed length, so nothing
/// of the last one survives a write that took.
pub fn write(guid: &[u8; 16], booted: Booted) -> Result<(), String> {
    loaderlog::with_open_volume(|root| {
        let file = root
            .open(RECORD, Mode::CreateReadWrite)
            .map_err(|e| alloc::format!("{RECORD} would not open ({e})"))?;
        let Some(mut file) = file.into_regular_file() else {
            return Err(alloc::format!("{RECORD} on the log partition is a directory"));
        };
        file.write(&record::encode(booted, guid))
            .map_err(|(e, _)| alloc::format!("{RECORD} would not write ({e})"))?;
        // Flushed here and not at the handoff: a byte in a cache survives no
        // power cut.
        file.flush().map_err(|e| alloc::format!("{RECORD} would not flush ({e})"))
    })
    .and_then(|inner| inner)
}

fn read(root: &mut File, guid: &[u8; 16]) -> Result<Option<Booted>, String> {
    let file = match root.open(RECORD, Mode::Read) {
        Ok(file) => file,
        Err(Status::NOT_FOUND) => return Ok(None),
        Err(e) => return Err(alloc::format!("would not open ({e})")),
    };
    let Some(mut file) = file.into_regular_file() else {
        return Err(String::from("is a directory"));
    };
    // One byte more than a record, so a longer file is told from a whole one.
    let mut buffer = [0u8; record::BYTES + 1];
    let n = file.read(&mut buffer).map_err(|e| alloc::format!("would not read ({e})"))?;
    record::decode(&buffer[..n], guid).map(Some).map_err(|why| alloc::format!("{why}"))
}
