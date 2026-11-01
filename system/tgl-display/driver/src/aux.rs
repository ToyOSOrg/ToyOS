//! Native AUX transactions on DDI A's channel, by Vol 12 "AUX Functional
//! Sequence". The caller holds that sequence's step 2: no PSR, and DC5 and
//! DC6 disallowed, while a transaction runs, since PSR uses the channel
//! spontaneously.

use toyos_device_memory::Registers;

use crate::{regs, Clock};

/// The most data bytes one transaction carries.
pub const MAX_DATA: usize = 16;

const NATIVE_WRITE: u8 = 0x8;
const NATIVE_READ: u8 = 0x9;

/// Transactions tried before the first fault is reported.
const ATTEMPTS: u32 = 32;
/// An AUX requester waits out the reply time-out before its next request.
const BETWEEN_NANOS: u64 = 500_000;
/// One transaction's hang ceiling: the 4 ms reply timeout, and margin.
const DONE_NANOS: u64 = 10_000_000;

/// Why a transaction did not complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Send Busy did not clear.
    Busy,
    /// The sink did not reply within the timeout.
    Timeout,
    /// The reply was corrupt, not whole bytes or longer than 20.
    Receive,
    /// The reply's size, which Vol 2c forbids at 0 or past 20.
    Size(u32),
    Nack,
    Defer,
    /// An acknowledgement carrying fewer bytes than asked, or a write taking
    /// fewer than sent.
    Short { wanted: usize, got: usize },
    /// A reply code the native protocol has not.
    Reply(u8),
}

/// The `bytes.len()` bytes at DPCD `address` read into `bytes`, 1 to
/// [`MAX_DATA`] of them.
pub fn read<R: Registers, C: Clock>(regs: &R, clock: &C, address: u32, bytes: &mut [u8]) -> Result<(), Fault> {
    let request = header(NATIVE_READ, address, bytes.len());
    retried(clock, || {
        let (reply, n) = message(regs, clock, &request)?;
        acknowledged(reply[0])?;
        let data = &reply[1..n];
        if data.len() < bytes.len() {
            return Err(Fault::Short { wanted: bytes.len(), got: data.len() });
        }
        bytes.copy_from_slice(&data[..bytes.len()]);
        Ok(())
    })
}

/// `bytes` written at DPCD `address`, 1 to [`MAX_DATA`] of them.
pub fn write<R: Registers, C: Clock>(regs: &R, clock: &C, address: u32, bytes: &[u8]) -> Result<(), Fault> {
    let mut request = [0u8; 4 + MAX_DATA];
    request[..4].copy_from_slice(&header(NATIVE_WRITE, address, bytes.len()));
    request[4..4 + bytes.len()].copy_from_slice(bytes);
    let request = &request[..4 + bytes.len()];
    retried(clock, || {
        let (reply, n) = message(regs, clock, request)?;
        acknowledged(reply[0])?;
        match n {
            1 => Ok(()),
            _ => Err(Fault::Short { wanted: bytes.len(), got: usize::from(reply[1]) }),
        }
    })
}

fn header(command: u8, address: u32, len: usize) -> [u8; 4] {
    assert!((1..=MAX_DATA).contains(&len), "an AUX transaction of {len} bytes");
    assert!(address < 1 << 20, "DPCD address {address:#x}");
    [command << 4 | (address >> 16) as u8, (address >> 8) as u8, address as u8, (len - 1) as u8]
}

/// The native reply code in bits 5:4 of a reply's first byte.
fn acknowledged(first: u8) -> Result<(), Fault> {
    match (first >> 4) & 0b11 {
        0b00 => Ok(()),
        0b01 => Err(Fault::Nack),
        0b10 => Err(Fault::Defer),
        code => Err(Fault::Reply(code)),
    }
}

fn retried<T, C: Clock>(clock: &C, mut attempt: impl FnMut() -> Result<T, Fault>) -> Result<T, Fault> {
    let mut first = None;
    for _ in 0..ATTEMPTS {
        match attempt() {
            Ok(done) => return Ok(done),
            Err(fault) => {
                first.get_or_insert(fault);
                clock.pause(BETWEEN_NANOS);
            }
        }
    }
    Err(first.expect("at least one attempt was made"))
}

/// One message sent and its reply read, steps 3-8 of the sequence: the
/// reply's bytes and how many there are.
fn message<R: Registers, C: Clock>(regs: &R, clock: &C, request: &[u8]) -> Result<([u8; regs::AUX_BYTES], usize), Fault> {
    if regs.read32(regs::DDI_AUX_CTL_A) & regs::AUX_SEND_BUSY != 0 {
        return Err(Fault::Busy);
    }
    for (n, chunk) in request.chunks(4).enumerate() {
        let mut word = [0u8; 4];
        word[..chunk.len()].copy_from_slice(chunk);
        regs.write32(regs::DDI_AUX_DATA_A + 4 * n, u32::from_be_bytes(word));
    }
    let clear = regs::AUX_DONE | regs::AUX_TIMEOUT | regs::AUX_RECEIVE_ERROR;
    let control = regs::AUX_TIMEOUT_MAX | regs::AUX_FAST_WAKE_SYNC | regs::AUX_SYNC;
    let size = (request.len() as u32) << regs::AUX_SIZE_SHIFT;
    regs.write32(regs::DDI_AUX_CTL_A, regs::AUX_SEND_BUSY | clear | control | size);
    let deadline = clock.nanos() + DONE_NANOS;
    let status = loop {
        let late = clock.nanos() > deadline;
        let status = regs.read32(regs::DDI_AUX_CTL_A);
        if status & regs::AUX_SEND_BUSY == 0 {
            break status;
        }
        if late {
            return Err(Fault::Busy);
        }
    };
    regs.write32(regs::DDI_AUX_CTL_A, clear | control);
    if status & regs::AUX_TIMEOUT != 0 {
        return Err(Fault::Timeout);
    }
    if status & regs::AUX_RECEIVE_ERROR != 0 {
        return Err(Fault::Receive);
    }
    let received = (status >> regs::AUX_SIZE_SHIFT) & regs::AUX_SIZE;
    if received == 0 || received as usize > regs::AUX_BYTES {
        return Err(Fault::Size(received));
    }
    let mut reply = [0u8; regs::AUX_BYTES];
    for (n, chunk) in reply.chunks_mut(4).enumerate().take((received as usize).div_ceil(4)) {
        chunk.copy_from_slice(&regs.read32(regs::DDI_AUX_DATA_A + 4 * n).to_be_bytes());
    }
    Ok((reply, received as usize))
}
