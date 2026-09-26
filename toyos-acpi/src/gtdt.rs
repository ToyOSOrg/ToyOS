//! The GTDT (signature `GTDT`): ACPI 6.5 §5.2.25, Table 5.121 — the Arm
//! generic timer's interrupts.

use crate::{find_table, Phys, TableError};

/// Secure EL1 timer GSIV at 48 and its flags at 52; non-secure EL1 at 56/60;
/// virtual EL1 at 64/68; EL2 at 72/76.
const SECURE_EL1: usize = 48;
const NON_SECURE_EL1: usize = 56;
const VIRTUAL_EL1: usize = 64;
const EL2: usize = 72;
/// Every field this decoder reads lies below the EL2 flags' end.
pub const GTDT_NEEDED: usize = EL2 + 8;

/// One timer's interrupt: its GSIV (a PPI) and ACPI 6.5 Table 5.122's flags —
/// bit 0 edge-triggered, bit 1 active-low, bit 2 always-on capable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimerInterrupt {
    pub gsiv: u32,
    pub flags: u32,
}

impl TimerInterrupt {
    pub fn edge(self) -> bool {
        self.flags & 1 != 0
    }

    pub fn active_low(self) -> bool {
        self.flags & 2 != 0
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Gtdt {
    pub secure_el1: TimerInterrupt,
    pub non_secure_el1: TimerInterrupt,
    pub virtual_el1: TimerInterrupt,
    pub el2: TimerInterrupt,
}

/// The GTDT at `rsdp_addr`, decoded.
pub fn gtdt<P: Phys>(phys: P, rsdp_addr: u64) -> Result<Gtdt, TableError> {
    let table = find_table(phys, rsdp_addr, b"GTDT", GTDT_NEEDED)?;
    let short = TableError::Length { declared: table.len() as u32, needed: GTDT_NEEDED };
    let timer = |at: usize| -> Result<TimerInterrupt, TableError> {
        Ok(TimerInterrupt {
            gsiv: table.u32_at(at).ok_or(short)?,
            flags: table.u32_at(at + 4).ok_or(short)?,
        })
    };
    Ok(Gtdt {
        secure_el1: timer(SECURE_EL1)?,
        non_secure_el1: timer(NON_SECURE_EL1)?,
        virtual_el1: timer(VIRTUAL_EL1)?,
        el2: timer(EL2)?,
    })
}
