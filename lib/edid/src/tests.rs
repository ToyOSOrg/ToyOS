use super::*;

fn fixture(name: &str) -> &'static [u8] {
    match name {
        "t14" => include_bytes!("../fixtures/t14-n140hca-eae.bin"),
        "amd" => include_bytes!("../fixtures/amd-bench-b140han06.8.bin"),
        "qemu-default" => include_bytes!("../fixtures/qemu-default.bin"),
        "qemu-1920" => include_bytes!("../fixtures/qemu-dpi157-1920x1080.bin"),
        "qemu-2560" => include_bytes!("../fixtures/qemu-dpi254-2560x1600.bin"),
        "qemu-3456" => include_bytes!("../fixtures/qemu-dpi254-3456x2234.bin"),
        "virtio-gpu" => include_bytes!("../fixtures/virtio-gpu-get-edid-1024x768.bin"),
        other => panic!("no fixture {other}"),
    }
}

fn size(bytes: &[u8]) -> ImageSize {
    BaseBlock::parse(bytes).expect("a fixture is a valid block").image_size()
}

fn mm(width: u32, height: u32) -> ImageSize {
    ImageSize::Millimetres { width, height }
}

/// A copy of `bytes` with `at` set to `value` and the checksum made good.
fn edited(bytes: &[u8], edits: &[(usize, u8)]) -> [u8; BLOCK] {
    let mut block: [u8; BLOCK] = bytes[..BLOCK].try_into().unwrap();
    for &(at, value) in edits {
        block[at] = value;
    }
    let rest = block[..BLOCK - 1].iter().fold(0u8, |s, b| s.wrapping_add(*b));
    block[BLOCK - 1] = 0u8.wrapping_sub(rest);
    block
}

#[test]
fn a_panels_size_is_its_first_detailed_timings() {
    assert_eq!(size(fixture("t14")), mm(309, 173));
    assert_eq!(size(fixture("amd")), mm(309, 174));
    assert_eq!(size(fixture("qemu-default")), mm(325, 203));
    assert_eq!(size(fixture("qemu-1920")), mm(310, 174));
    assert_eq!(size(fixture("qemu-2560")), mm(256, 160));
    assert_eq!(size(fixture("virtio-gpu")), mm(260, 195));
}

/// QEMU's large-screen block carries a display descriptor first, so the size
/// is the base block's centimetres.
#[test]
fn with_no_timing_first_the_size_is_the_base_blocks_centimetres() {
    assert_eq!(size(fixture("qemu-3456")), mm(340, 220));
}

/// Table 3.21 note 17: a timing's 0 mm is undefined, not a size.
#[test]
fn a_timing_of_zero_millimetres_falls_to_the_base_block() {
    let block = edited(fixture("t14"), &[(0x36 + 12, 0), (0x36 + 13, 0), (0x36 + 14, 0)]);
    assert_eq!(size(&block), mm(310, 170));
}

#[test]
fn one_zero_byte_of_the_screen_size_is_an_aspect_ratio() {
    let landscape = edited(fixture("qemu-3456"), &[(SCREEN_H_CM, 0)]);
    assert_eq!(size(&landscape), ImageSize::AspectRatio);
    let portrait = edited(fixture("qemu-3456"), &[(SCREEN_W_CM, 0)]);
    assert_eq!(size(&portrait), ImageSize::AspectRatio);
}

#[test]
fn both_zero_is_unknown() {
    let block = edited(fixture("qemu-3456"), &[(SCREEN_W_CM, 0), (SCREEN_H_CM, 0)]);
    assert_eq!(size(&block), ImageSize::Unknown);
}

#[test]
fn a_short_block_is_refused() {
    assert_eq!(BaseBlock::parse(&fixture("t14")[..127]).err(), Some(Refusal::Short { len: 127 }));
    assert_eq!(BaseBlock::parse(&[]).err(), Some(Refusal::Short { len: 0 }));
}

#[test]
fn a_block_without_the_header_is_refused() {
    let block = edited(fixture("t14"), &[(0, 0x01)]);
    assert_eq!(BaseBlock::parse(&block).err(), Some(Refusal::Header));
}

#[test]
fn a_block_that_does_not_sum_to_zero_is_refused() {
    let mut block: [u8; BLOCK] = fixture("t14").try_into().unwrap();
    block[0x15] = block[0x15].wrapping_add(1);
    assert_eq!(BaseBlock::parse(&block).err(), Some(Refusal::Checksum { sum: 1 }));
}

/// What follows the base block is an extension, read by nothing here.
#[test]
fn bytes_past_the_base_block_are_not_read() {
    let mut longer = [0xa5u8; 2 * BLOCK];
    longer[..BLOCK].copy_from_slice(fixture("amd"));
    assert_eq!(size(&longer), mm(309, 174));
}
