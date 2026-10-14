//! As much of a VNC client as resizing the host's window takes: RFC 6143's
//! handshake with no security, then the RFB community specification's
//! `SetDesktopSize` (client message 251), which a client sends once it has
//! said it takes the `ExtendedDesktopSize` pseudo-encoding (-308).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// RFC 6143 §7.1.1: the version both ends speak.
const VERSION: &[u8; 12] = b"RFB 003.008\n";
/// §7.1.2: the security type "None".
const SECURITY_NONE: u8 = 1;
/// §7.5.2's `SetEncodings`, and the pseudo-encoding that permits
/// `SetDesktopSize`.
const SET_ENCODINGS: u8 = 2;
const EXTENDED_DESKTOP_SIZE: i32 = -308;
const SET_DESKTOP_SIZE: u8 = 251;

/// Ask the VNC server at `socket` for a display of `width` x `height`, as a
/// person resizing the host's window does. The connection is answered so the
/// caller can hold it open while the guest follows.
pub fn resize(socket: &Path, width: u16, height: u16) -> Result<UnixStream, String> {
    let mut vnc = UnixStream::connect(socket).map_err(|e| format!("vnc: connect {}: {e}", socket.display()))?;
    vnc.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| format!("vnc: {e}"))?;
    let io = |e: std::io::Error| format!("vnc: {e}");

    let mut version = [0u8; 12];
    vnc.read_exact(&mut version).map_err(io)?;
    if &version != VERSION {
        return Err(format!("vnc: the server speaks {:?}", String::from_utf8_lossy(&version)));
    }
    vnc.write_all(VERSION).map_err(io)?;

    let mut count = [0u8; 1];
    vnc.read_exact(&mut count).map_err(io)?;
    let mut types = vec![0u8; count[0] as usize];
    vnc.read_exact(&mut types).map_err(io)?;
    if !types.contains(&SECURITY_NONE) {
        return Err(format!("vnc: the server offers security types {types:?}, without None"));
    }
    vnc.write_all(&[SECURITY_NONE]).map_err(io)?;
    let mut result = [0u8; 4];
    vnc.read_exact(&mut result).map_err(io)?;
    if result != [0; 4] {
        return Err(format!("vnc: the server failed the handshake with {result:?}"));
    }

    // §7.3.1 ClientInit, shared; §7.3.2 ServerInit: the size, the pixel
    // format and the name, all of which this client leaves unread.
    vnc.write_all(&[1]).map_err(io)?;
    let mut init = [0u8; 24];
    vnc.read_exact(&mut init).map_err(io)?;
    let name = u32::from_be_bytes(init[20..24].try_into().expect("four bytes")) as usize;
    vnc.read_exact(&mut vec![0u8; name]).map_err(io)?;

    let mut encodings = vec![SET_ENCODINGS, 0, 0, 1];
    encodings.extend(EXTENDED_DESKTOP_SIZE.to_be_bytes());
    vnc.write_all(&encodings).map_err(io)?;

    // One screen, id 0, at the origin, with no flags.
    let mut set = vec![SET_DESKTOP_SIZE, 0];
    set.extend(width.to_be_bytes());
    set.extend(height.to_be_bytes());
    set.extend([1, 0]);
    set.extend(0u32.to_be_bytes());
    set.extend([0u8; 4]);
    set.extend(width.to_be_bytes());
    set.extend(height.to_be_bytes());
    set.extend(0u32.to_be_bytes());
    vnc.write_all(&set).map_err(io)?;
    Ok(vnc)
}
