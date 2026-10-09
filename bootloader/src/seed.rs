//! The seed firmware gives the kernel's random generator: 32 bytes from
//! `EFI_RNG_PROTOCOL` (UEFI 2.11 §37.5), asked for once, before
//! `ExitBootServices`. Firmware without the protocol is a machine and not an
//! error: the kernel is handed none and keys its generator from what its CPU
//! has, or refuses. One line says which, and no line carries a byte.

use toyos_abi::boot::SEED_LEN;
use toyos_random::{wipe, Seed};
use uefi::prelude::*;
use uefi::proto::rng::Rng;

use crate::protocol;

/// Fill `into` with firmware's seed and answer its length: [`SEED_LEN`], or 0
/// with `into` zero. Judged with the kernel's own [`Seed::judge`], so the line
/// here says what the kernel will find.
pub fn read(system_table: &SystemTable<Boot>, into: &mut [u8; SEED_LEN]) -> u64 {
    let bs = system_table.boot_services();
    let none = |why: core::fmt::Arguments| {
        println!("Seed: {why}, so the kernel's generator is handed none");
        0
    };
    let Ok(handle) = bs.get_handle_for_protocol::<Rng>() else {
        return none(format_args!("firmware has no EFI_RNG_PROTOCOL"));
    };
    // GET_PROTOCOL: an exclusive open would stop the driver behind it.
    let mut rng = match protocol::get::<Rng>(bs, handle) {
        Ok(rng) => rng,
        Err(e) => return none(format_args!("EFI_RNG_PROTOCOL would not open ({e})")),
    };
    // No algorithm named: firmware's default (§37.5.2).
    if let Err(e) = rng.get_rng(None, into) {
        wipe(into);
        return none(format_args!("EFI_RNG_PROTOCOL's GetRNG failed ({e})"));
    }
    if let Err(why) = Seed::judge(into) {
        wipe(into);
        return none(format_args!("EFI_RNG_PROTOCOL's {SEED_LEN} bytes are refused, {why}"));
    }
    println!("Seed: {SEED_LEN} bytes from EFI_RNG_PROTOCOL for the kernel's generator");
    SEED_LEN as u64
}
