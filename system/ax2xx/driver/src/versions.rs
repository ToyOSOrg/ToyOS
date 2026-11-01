//! The versions a file declares of each message (§6.5), and those this
//! crate's layouts are written for (§10.1). [`Versions::of`] refuses a file
//! that declares any message the driver exchanges at a version none of its
//! layouts is written for, so a driver holding a [`Versions`] sends and reads
//! nothing at a version it has no layout for.

use alloc::vec::Vec;

/// A declared version that means the caller's default, as if the file listed no entry (§6.5).
pub const UNKNOWN: u8 = 99;

/// A message's group and its identifier within the group.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Id {
    pub group: u8,
    pub id: u8,
}

/// One entry of a file's command-version record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub command: u8,
    pub notification: u8,
}

/// A file's command-version record, its entries in the file's order. A lookup takes the first
/// entry whose group and identifier match (§6.5): both pinned files list (0x0e, 0x17) twice, a
/// pair no message here is in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Declared(Vec<(Id, Entry)>);

impl Declared {
    /// `None` when the record is not whole 4-byte entries.
    pub(crate) fn parse(contents: &[u8]) -> Option<Declared> {
        let (entries, rest) = contents.as_chunks::<4>();
        rest.is_empty().then(|| {
            Declared(
                entries
                    .iter()
                    .map(|&[id, group, command, notification]| (Id { group, id }, Entry { command, notification }))
                    .collect(),
            )
        })
    }

    pub fn entries(&self) -> impl Iterator<Item = (Id, Entry)> + '_ {
        self.0.iter().copied()
    }

    /// What the file declares of one side of `id`, by its first entry for `id`: `None` where it
    /// lists none, as it lists none for a message with one layout, or where the entry says
    /// [`UNKNOWN`].
    pub fn of(&self, id: Id, side: Side) -> Option<u8> {
        let entry = self.0.iter().find(|&&(at, _)| at == id).map(|&(_, entry)| entry)?;
        let version = match side {
            Side::Command => entry.command,
            Side::Notification => entry.notification,
        };
        (version != UNKNOWN).then_some(version)
    }
}

/// Which of an entry's two versions a message is: a command the driver sends,
/// or what the firmware sends, a notification or a command's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Command,
    Notification,
}

/// A message the driver exchanges, and the declared versions its layout is
/// written for: `None` is a file that declares nothing of it, for a message
/// Intel's definitions give one layout (§6.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message {
    pub name: &'static str,
    pub id: Id,
    pub side: Side,
    pub layouts: &'static [Option<u8>],
}

const fn message(name: &'static str, group: u8, id: u8, side: Side, layouts: &'static [Option<u8>]) -> Message {
    Message { name, id: Id { group, id }, side, layouts }
}

use Side::{Command, Notification};

pub const INIT_FLAGS: Message = message("init flags", 0x02, 0x03, Command, &[None]);
pub const NVM_DONE: Message = message("NVM done", 0x0c, 0x00, Command, &[Some(1)]);
pub const NVM_INFO: Message = message("NVM info", 0x0c, 0x02, Command, &[Some(1)]);
pub const NVM_INFO_ANSWER: Message = message("NVM info answer", 0x0c, 0x02, Notification, &[Some(4)]);
pub const TRANSMIT_ANTENNAS: Message = message("transmit antennas", 0x01, 0x98, Command, &[Some(1)]);
pub const COEXISTENCE: Message = message("coexistence", 0x01, 0x9b, Command, &[Some(6)]);
pub const PLATFORM_LATENCY: Message = message("platform latency", 0x02, 0x01, Command, &[Some(2)]);
/// Intel names the 32-byte layout version 2; the AX200 took it at its
/// declared 3 (§10.9).
pub const LATENCY_TOLERANCE: Message = message("latency tolerance", 0x01, 0xee, Command, &[Some(3)]);
pub const TEMPERATURE_THRESHOLDS: Message = message("temperature thresholds", 0x04, 0x04, Command, &[Some(1)]);
/// Intel names the 4-byte layout version 7; the AX200 took it at its declared
/// 6 (§10.11).
pub const DEVICE_POWER: Message = message("device power", 0x01, 0x77, Command, &[Some(6), Some(7)]);
/// Intel names the 28-byte layout version 2; both files declare 1 (§10.12).
pub const COUNTRY_UPDATE: Message = message("country update", 0x01, 0xc8, Command, &[Some(1)]);
/// Both declared versions are read at Intel's version-4 layout, which the
/// AX200's version-5 answer had (§10.12).
pub const COUNTRY_ANSWER: Message = message("country update answer", 0x01, 0xc8, Notification, &[Some(5), Some(6)]);
pub const SCAN_CONFIGURATION: Message = message("scan configuration", 0x01, 0x0c, Command, &[Some(5)]);
pub const BEACON_FILTER: Message = message("beacon filter", 0x01, 0xd2, Command, &[Some(4)]);
pub const SCAN_REQUEST: Message = message("scan request", 0x01, 0x0d, Command, &[Some(15), Some(17)]);
pub const SCAN_ABORT: Message = message("scan abort", 0x01, 0x0e, Command, &[Some(1)]);

pub const ALIVE: Message = message("alive", 0x00, 0x01, Notification, &[Some(6)]);
pub const INIT_COMPLETE: Message = message("init complete", 0x00, 0x04, Notification, &[None]);
pub const COUNTRY_CHANGE: Message = message("country change", 0x00, 0xc9, Notification, &[None]);
pub const SCAN_ITERATION_DONE: Message = message("scan iteration done", 0x00, 0xb5, Notification, &[None]);
pub const SCAN_DONE: Message = message("scan done", 0x00, 0x0f, Notification, &[None]);
pub const PNVM_LOADED: Message = message("platform NVM loaded", 0x0c, 0xfe, Notification, &[Some(1)]);
pub const RECEIVED_FRAME: Message = message("received frame", 0x00, 0xc1, Notification, &[Some(4), Some(5)]);
pub const COMMAND_ERROR: Message = message("command error", 0x00, 0x02, Notification, &[None]);
pub const THERMAL_SHUTDOWN: Message = message("thermal shutdown", 0x04, 0xfe, Notification, &[Some(2)]);

/// Every message the driver exchanges through a passive scan (§10.1).
pub const EXCHANGED: &[Message] = &[
    INIT_FLAGS,
    NVM_DONE,
    NVM_INFO,
    NVM_INFO_ANSWER,
    TRANSMIT_ANTENNAS,
    COEXISTENCE,
    PLATFORM_LATENCY,
    LATENCY_TOLERANCE,
    TEMPERATURE_THRESHOLDS,
    DEVICE_POWER,
    COUNTRY_UPDATE,
    COUNTRY_ANSWER,
    SCAN_CONFIGURATION,
    BEACON_FILTER,
    SCAN_REQUEST,
    SCAN_ABORT,
    ALIVE,
    INIT_COMPLETE,
    COUNTRY_CHANGE,
    SCAN_ITERATION_DONE,
    SCAN_DONE,
    PNVM_LOADED,
    RECEIVED_FRAME,
    COMMAND_ERROR,
    THERMAL_SHUTDOWN,
];

/// Why a file's declarations leave a message the driver exchanges without a
/// layout here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// Declared at a version no layout here is written for.
    Version { message: &'static str, declared: Option<u8> },
}

/// The scan request's channel entry (§10.15): below version 17 the band is
/// its own byte, at 17 it is in the entry's flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanForm {
    BandByte,
    BandInFlags,
}

/// The received-frame descriptor (§11.1): 48 bytes at version 4, 64 at 5.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceivedForm {
    V4,
    V5,
}

/// A file's declarations, checked against [`EXCHANGED`]: the proof that every
/// message the driver exchanges has a layout here at the version declared,
/// and the forms that differ between those versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Versions {
    pub scan: ScanForm,
    pub received: ReceivedForm,
}

impl Versions {
    pub fn of(declared: &Declared) -> Result<Versions, Unsupported> {
        let at = |message: Message| {
            let version = declared.of(message.id, message.side);
            match message.layouts.contains(&version) {
                true => Ok(version),
                false => Err(Unsupported::Version { message: message.name, declared: version }),
            }
        };
        for &message in EXCHANGED {
            at(message)?;
        }
        let scan = match at(SCAN_REQUEST)? {
            Some(15) => ScanForm::BandByte,
            Some(17) => ScanForm::BandInFlags,
            declared => return Err(Unsupported::Version { message: SCAN_REQUEST.name, declared }),
        };
        let received = match at(RECEIVED_FRAME)? {
            Some(4) => ReceivedForm::V4,
            Some(5) => ReceivedForm::V5,
            declared => return Err(Unsupported::Version { message: RECEIVED_FRAME.name, declared }),
        };
        Ok(Versions { scan, received })
    }
}
