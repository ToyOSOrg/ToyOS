//! The frame record: what the kernel writes, read back, and every way a line
//! can fail to be one.

use toyos_abi::log::MAX_RECORD_MESSAGE;
use toyos_symbols::frame::{decode, frame_offset, BuildId, Decoded, Refused, UserFrame};

fn id(len: usize) -> BuildId {
    BuildId::new(&(0..len as u8).map(|b| b.wrapping_mul(37)).collect::<Vec<_>>()).unwrap()
}

fn decoded(line: &str) -> Decoded<'_> {
    decode(line).unwrap_or_else(|| panic!("not a frame: {line:?}")).unwrap_or_else(|r| panic!("{r:?}: {line:?}"))
}

fn round_trip(frame: UserFrame<'_>) {
    let line = frame.to_string();
    assert!(line.len() <= MAX_RECORD_MESSAGE, "{} bytes", line.len());
    for line in [line.clone(), format!("[kernel 12.345 cpu3 tid=7] {line}"), format!("{line}\r\n")] {
        let back = decoded(&line);
        assert_eq!((back.pc, back.offset, back.build_id), (frame.pc, frame.offset, frame.build_id), "{line:?}");
        assert_eq!(back.name().collect::<String>(), frame.name, "{line:?}");
    }
}

#[test]
fn a_frame_reads_back_as_written() {
    for name in ["/home/disk_backtrace/child", "/system/bin/x", "", "/a b/c", "/ünï/çødé"] {
        for build_id in [None, Some(id(1)), Some(id(20)), Some(id(32))] {
            for (pc, offset) in [(0, 0), (0x100_0000_1234, 0x1234), (u64::MAX, u64::MAX)] {
                round_trip(UserFrame { pc, name, offset, build_id });
            }
        }
    }
}

/// Every byte the grammar spends: none of them is ever raw in a name.
#[test]
fn a_name_holding_the_grammars_bytes_reads_back_whole() {
    let name = "/x\n y+0x1 id=ab\\[z]\t\u{7f}\u{1}  +";
    let line = UserFrame { pc: 1, name, offset: 2, build_id: Some(id(4)) }.to_string();
    assert!(!line.contains('\n') && !line.contains('\t') && !line.contains('\u{7f}'), "{line:?}");
    assert_eq!(line.matches(" id=").count(), 1, "{line:?}");
    assert_eq!(line.matches('+').count(), 1, "{line:?}");
    round_trip(UserFrame { pc: 1, name, offset: 2, build_id: Some(id(4)) });
}

#[test]
fn every_byte_below_space_reads_back() {
    let name: String = (0u8..0x80).map(char::from).collect();
    round_trip(UserFrame { pc: 1, name: &name, offset: 2, build_id: None });
}

#[test]
fn the_kernels_exact_line() {
    let frame = UserFrame { pc: 0x100_0000_4321, name: "/bin/x", offset: 0x4321, build_id: Some(id(2)) };
    assert_eq!(frame.to_string(), "    0x10000004321  /bin/x+0x4321 id=0025");
    let none = UserFrame { build_id: None, ..frame };
    assert_eq!(none.to_string(), "    0x10000004321  /bin/x+0x4321 id=-");
}

/// A name past what a record holds loses its middle, never the offset or the
/// id, and the reader refuses it rather than open a file it does not name.
#[test]
fn an_elided_name_keeps_offset_and_id_and_is_refused() {
    let long = format!("/{}", "d/".repeat(1500));
    let frame = UserFrame { pc: 0x10, name: &long, offset: 0xfedc, build_id: Some(id(32)) };
    let line = frame.to_string();
    assert!(line.len() <= MAX_RECORD_MESSAGE, "{} bytes", line.len());
    assert!(line.ends_with(&format!("+0xfedc id={}", id(32))), "{line}");
    assert_eq!(decode(&line), Some(Err(Refused::Elided)));
    // Escapes count against the budget at their escaped width.
    let controls = "\n".repeat(400);
    let line = UserFrame { name: &controls, ..frame }.to_string();
    assert!(line.len() <= MAX_RECORD_MESSAGE, "{} bytes", line.len());
    assert_eq!(decode(&line), Some(Err(Refused::Elided)));
}

#[test]
fn an_escape_no_encoder_writes_is_refused() {
    for name in ["\\q", "\\x8f", "\\x4", "\\x", "\\", "a\\xZZ", "\\X41"] {
        let line = format!("    0x1  {name}+0x2 id=-");
        assert_eq!(decode(&line), Some(Err(Refused::BadEscape)), "{line:?}");
    }
}

#[test]
fn an_id_that_is_not_lowercase_hex_bytes_is_refused() {
    let too_long = "ab".repeat(33);
    for bad in ["", "abc", "ABCD", "zz", "0x12", too_long.as_str(), "--"] {
        let line = format!("    0x1  /f+0x2 id={bad}");
        assert_eq!(decode(&line), Some(Err(Refused::BadId)), "{line:?}");
    }
}

/// The kernel's own frames, and every other line, are not user frames.
#[test]
fn a_line_that_is_no_frame_is_none() {
    for line in [
        "",
        "    0xffffffff80001234  kernel::main+0x10",
        "    0x1000",
        "inbox: id=5",
        "    0x1  /f+0x2",
        "    0x1 /f+0x2 id=-",
        "    1  /f+0x2 id=-",
        "    0x1  /f+0xG id=-",
        "    0x1  /f+2 id=-",
        "    0x  /f+0x2 id=-",
        "    0x10000000000000000  /f+0x2 id=-",
        "    0xABC  /f+0x2 id=-",
    ] {
        assert_eq!(decode(line), None, "{line:?}");
    }
}

/// A record cut anywhere is never read as the frame it was cut from.
#[test]
fn a_frame_cut_at_any_byte_is_not_that_frame() {
    let frame = UserFrame { pc: 0x100_0000_1234, name: "/home/x\ny", offset: 0x1234, build_id: Some(id(20)) };
    let line = frame.to_string();
    let whole = decoded(&line);
    for cut in 0..line.len() {
        let Some(prefix) = line.get(..cut) else { continue };
        assert_ne!(decode(prefix), Some(Ok(whole)), "cut at {cut}: {prefix:?}");
    }
}

/// Frames with random edits in the grammar's own alphabet: none panics the
/// reader, and the edits reach both a frame and a refusal.
#[test]
fn no_line_panics_the_reader() {
    const ALPHABET: &[char] = &[' ', '0', 'x', '7', 'a', 'f', 'A', 'X', '+', '\\', '[', '-', 'i', 'd', '=', '/', '\n', 'é'];
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let mut next = move |n: usize| {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state % n as u64) as usize
    };
    let (mut frames, mut refused) = (0, 0);
    for _ in 0..200_000 {
        let name: String = (0..next(12)).map(|_| ALPHABET[next(ALPHABET.len())]).collect();
        let id = (next(3) > 0).then(|| id(1 + next(32)));
        let mut line: Vec<char> = UserFrame { pc: 0x1234, name: &name, offset: 0x56, build_id: id }.to_string().chars().collect();
        for _ in 0..next(4) {
            let at = next(line.len() + 1);
            match next(3) {
                0 if at < line.len() => line[at] = ALPHABET[next(ALPHABET.len())],
                1 if at < line.len() => drop(line.remove(at)),
                _ => line.insert(at, ALPHABET[next(ALPHABET.len())]),
            }
        }
        let line: String = line.into_iter().collect();
        match decode(&line) {
            Some(Ok(frame)) => {
                frame.name().for_each(drop);
                frames += 1;
            }
            Some(Err(_)) => refused += 1,
            None => {}
        }
    }
    assert!(frames > 1000 && refused > 1000, "{frames} frames, {refused} refusals: the edits reach too little");
}

#[test]
fn a_build_id_is_one_to_32_bytes() {
    assert_eq!(BuildId::new(&[]), None);
    assert_eq!(BuildId::new(&[0; 33]), None);
    assert_eq!(BuildId::new(&[5]).map(|i| i.as_bytes().to_vec()), Some(vec![5]));
    assert_eq!(BuildId::new(&[5; 32]).map(|i| i.as_bytes().len()), Some(32));
}

/// An image mapped at `[0x1000_0000, 0x1000_8000)` from a file whose lowest
/// address is `0x400000`: a pc maps back to the file's own addresses.
#[test]
fn an_offset_is_in_the_files_addresses() {
    let (start, end, vaddr_min) = (0x1000_0000u64, 0x1000_8000u64, 0x40_0000u64);
    let bias = start - vaddr_min;
    assert_eq!(frame_offset(start, start, end, bias), Some(vaddr_min));
    assert_eq!(frame_offset(start + 0x123, start, end, bias), Some(vaddr_min + 0x123));
    assert_eq!(frame_offset(end - 1, start, end, bias), Some(vaddr_min + 0x7fff));
    assert_eq!(frame_offset(end, start, end, bias), None);
    assert_eq!(frame_offset(start - 1, start, end, bias), None);
    assert_eq!(frame_offset(0, start, end, bias), None);
    assert_eq!(frame_offset(u64::MAX, start, end, bias), None);
    // A bias the image does not hold answers nothing rather than wrapping.
    assert_eq!(frame_offset(5, 0, 10, 6), None);
}
