//! The ITS's commands (§5.3): 32 bytes each, four little-endian
//! doublewords, the command number in bits [7:0] of the first.

use crate::lpi::Lpi;
use crate::{EventBits, Phys, Target};

/// One command.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    /// `MAPD` with `V` set (§5.3.10): `device`'s events are translated by
    /// the zeroed table at `table`.
    MapDevice { device: u32, table: Phys<8>, events: EventBits },
    /// `MAPD` with `V` clear: `device` has no table, and its messages are
    /// ignored.
    UnmapDevice { device: u32 },
    /// `MAPC` with `V` set (§5.3.9): `collection`'s interrupts go to `target`.
    MapCollection { collection: u16, target: Target },
    /// `MAPTI` (§5.3.12): `device`'s `event` is `lpi`, in `collection`.
    MapEvent { device: u32, event: u32, lpi: Lpi, collection: u16 },
    /// `INV` (§5.3.6): reread the configuration of `device`'s `event`'s LPI.
    Reconfigure { device: u32, event: u32 },
    /// `DISCARD` (§5.3.4): `device`'s `event` is no interrupt, and any
    /// pending one is dropped.
    Discard { device: u32, event: u32 },
    /// `SYNC` (§5.3.15): every command before it has taken effect at
    /// `target` before the next is read.
    Sync(Target),
}

/// `V`, bit [63] of doubleword 2.
const V: u64 = 1 << 63;

impl Command {
    pub const fn words(self) -> [u64; 4] {
        // DeviceID is bits [63:32] of doubleword 0, and EventID bits [31:0] of doubleword 1.
        const fn device(number: u64, device: u32) -> u64 {
            number | (device as u64) << 32
        }
        match self {
            // Size [4:0] of doubleword 1 is the EventID bits minus one; ITT_addr [51:8] of doubleword 2.
            Self::MapDevice { device: id, table, events } => {
                [device(0x08, id), (events.0 - 1) as u64, V | table.get(), 0]
            }
            Self::UnmapDevice { device: id } => [device(0x08, id), 0, 0, 0],
            // RDbase [51:16] and ICID [15:0] of doubleword 2.
            Self::MapCollection { collection, target } => [0x09, 0, V | target.0 << 16 | collection as u64, 0],
            // pINTID [63:32] of doubleword 1; ICID [15:0] of doubleword 2.
            Self::MapEvent { device: id, event, lpi, collection } => {
                [device(0x0A, id), (lpi.intid() as u64) << 32 | event as u64, collection as u64, 0]
            }
            Self::Reconfigure { device: id, event } => [device(0x0C, id), event as u64, 0, 0],
            Self::Discard { device: id, event } => [device(0x0F, id), event as u64, 0, 0],
            Self::Sync(target) => [0x05, 0, target.0 << 16, 0],
        }
    }
}
