//! Whether the last boot stopped cleanly, and which image it ran: the record
//! this loader writes on the log partition before each handoff
//! (`toyos_update::record`), and the mark `logkeeper` writes there once a stop
//! has made the log durable (`toyos_logstream::STOPPED`).
//!
//! **Both are taken before the next kernel runs**, so a mark found here is
//! always the last boot's, and a boot that dies before its stop leaves none.

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
    /// Whether the record and the mark are both gone, which is what lets this
    /// pass write a record that names only its own boot.
    pub cleared: bool,
}

/// Read the last boot's record and mark, and delete both.
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
        let deleted = delete(root, RECORD);
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
        if let Err(why) = &deleted {
            notes.push(alloc::format!("{HEAD} {why}"));
        }
        Last { proven, notes, cleared: stopped.is_ok() && deleted.is_ok() }
    });
    taken.unwrap_or_else(|why| Last {
        proven: None,
        notes: alloc::vec![alloc::format!("{HEAD} {why}, so no image is credited and none is recorded")],
        cleared: false,
    })
}

/// Write down the image this pass boots, through the log's own open volume.
pub fn write(guid: &[u8; 16], booted: Booted) -> Result<(), String> {
    loaderlog::with_open_volume(|root| {
        let file = root
            .open(RECORD, Mode::CreateReadWrite)
            .map_err(|e| alloc::format!("{RECORD} would not be created ({e})"))?;
        let Some(mut file) = file.into_regular_file() else {
            return Err(alloc::format!("{RECORD} on the log partition is a directory"));
        };
        file.write(&record::encode(Some(booted), guid))
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
    record::decode(&buffer[..n], guid).map_err(|why| alloc::format!("{why}"))
}

fn delete(root: &mut File, name: &CStr16) -> Result<(), String> {
    match root.open(name, Mode::ReadWrite) {
        Ok(file) => file.delete().map_err(|e| alloc::format!("{name} would not delete ({e})")),
        Err(Status::NOT_FOUND) => Ok(()),
        Err(e) => Err(alloc::format!("{name} would not open ({e})")),
    }
}
