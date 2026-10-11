//! `caller` returns `true`: the first range's last port is `Port::MAX`, where
//! its loop ends.
#![no_std]

type Port = u128;

#[derive(Clone, Copy)]
pub enum Mediated {
    Kept,
    ReadOnly,
}

#[derive(Clone, Copy)]
pub enum Standing {
    Free,
    Declared(Mediated),
}

fn port(standing: impl Fn(Port) -> Standing, port: Port, width: Port, write: bool) -> Result<Port, u8> {
    for port in port..=port + (width - 1) {
        match standing(port) {
            Standing::Free => {}
            Standing::Declared(Mediated::ReadOnly) if !write => {}
            Standing::Declared(Mediated::ReadOnly) => return Err(9),
            Standing::Declared(Mediated::Kept) => return Err(8),
        }
    }
    Ok(port)
}

fn standing(port: Port) -> Standing {
    match port {
        0x38..=0x3F | 0x20..=0x21 | 0xA0..=0xA1 | 0x70..=0x71 | 0xC8 | 0xCC..=0xCF | 0xC9 => {
            Standing::Declared(Mediated::Kept)
        }
        0xB2 => Standing::Declared(Mediated::ReadOnly),
        _ => Standing::Free,
    }
}

fn probe() -> bool {
    port(standing, Port::MAX - 3, 4, false).is_ok() & port(standing, 0xB2, 1, true).is_err()
}

#[no_mangle]
pub fn caller() -> bool {
    probe()
}
