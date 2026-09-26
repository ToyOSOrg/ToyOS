//! The chunking arithmetic against a device that records every request and
//! can be told to fail one.

use toyos_rootimage::chunk::{chunk_bytes, read, Blocks, Failed};

const MIB: usize = 1 << 20;
const PAGE: usize = 4096;

/// A disk whose byte at offset `o` is a function of `o`, so a chunk read from
/// the wrong LBA lands the wrong bytes.
struct Disk {
    lba_bytes: usize,
    requests: Vec<(u64, usize)>,
    fail_at: Option<u64>,
}

fn byte_at(offset: usize) -> u8 {
    (offset as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15).rotate_right(29) as u8
}

impl Blocks for Disk {
    type Error = &'static str;
    fn read(&mut self, lba: u64, into: &mut [u8]) -> Result<(), &'static str> {
        assert!(into.len().is_multiple_of(self.lba_bytes));
        self.requests.push((lba, into.len()));
        if self.fail_at == Some(lba) {
            return Err("DEVICE_ERROR");
        }
        let at = lba as usize * self.lba_bytes;
        for (i, b) in into.iter_mut().enumerate() {
            *b = byte_at(at + i);
        }
        Ok(())
    }
}

fn disk(lba_bytes: usize) -> Disk {
    Disk { lba_bytes, requests: Vec::new(), fail_at: None }
}

/// Read `len` bytes at `first` in `chunk`s, check every byte came from where
/// it belongs, and hand back the requests.
fn whole(lba_bytes: usize, first: u64, len: usize, chunk: usize) -> Vec<(u64, usize)> {
    let mut d = disk(lba_bytes);
    let mut into = vec![0u8; len];
    read(&mut d, first, lba_bytes as u32, chunk, &mut into).unwrap();
    let base = first as usize * lba_bytes;
    assert!(into.iter().enumerate().all(|(i, &b)| b == byte_at(base + i)), "a byte came from the wrong place");
    d.requests
}

#[test]
fn an_exact_multiple_is_whole_chunks() {
    let requests = whole(512, 2048, 4 * MIB, MIB);
    assert_eq!(requests, vec![(2048, MIB), (4096, MIB), (6144, MIB), (8192, MIB)]);
}

#[test]
fn the_last_chunk_is_whatever_is_left() {
    let requests = whole(512, 34, 2 * MIB + 3 * 512, MIB);
    assert_eq!(requests, vec![(34, MIB), (34 + 2048, MIB), (34 + 4096, 3 * 512)]);
}

#[test]
fn a_chunk_larger_than_the_extent_is_one_request_of_the_extent() {
    let requests = whole(512, 100, 5 * 512, MIB);
    assert_eq!(requests, vec![(100, 5 * 512)]);
}

#[test]
fn four_kib_blocks_advance_the_lba_by_their_own_size() {
    let requests = whole(4096, 7, MIB + PAGE, MIB);
    assert_eq!(requests, vec![(7, MIB), (7 + 256, PAGE)]);
}

#[test]
fn a_failing_chunk_is_named_by_its_first_lba_and_nothing_after_it_is_asked() {
    let mut d = disk(512);
    d.fail_at = Some(1000 + 3 * 2048);
    let mut into = vec![0u8; 10 * MIB];
    let failed = read(&mut d, 1000, 512, MIB, &mut into).unwrap_err();
    assert_eq!(failed, Failed { lba: 1000 + 3 * 2048, blocks: 2048, read: 3 * MIB, error: "DEVICE_ERROR" });
    assert_eq!(d.requests.len(), 4);
}

#[test]
fn a_failing_last_partial_chunk_names_its_own_length() {
    let mut d = disk(512);
    d.fail_at = Some(2 * 2048);
    let mut into = vec![0u8; 2 * MIB + 7 * 512];
    let failed = read(&mut d, 0, 512, MIB, &mut into).unwrap_err();
    assert_eq!((failed.lba, failed.blocks, failed.read), (4096, 7, 2 * MIB));
}

#[test]
fn no_granularity_is_the_bound() {
    assert_eq!(chunk_bytes(MIB, PAGE, 512, 0), MIB);
    assert_eq!(chunk_bytes(MIB, PAGE, 4096, 0), MIB);
}

#[test]
fn a_granularity_dividing_the_bound_leaves_it_alone() {
    assert_eq!(chunk_bytes(MIB, PAGE, 512, 8), MIB);
    assert_eq!(chunk_bytes(MIB, PAGE, 512, 2048), MIB);
}

#[test]
fn an_odd_granularity_rounds_the_bound_down_to_a_common_multiple() {
    // 3 blocks of 512 against 4 KiB pages: every 12 KiB.
    let chunk = chunk_bytes(MIB, PAGE, 512, 3);
    assert_eq!(chunk, MIB / (3 * PAGE) * (3 * PAGE));
    assert!(chunk.is_multiple_of(PAGE) && chunk.is_multiple_of(1536) && chunk <= MIB);
}

#[test]
fn a_granularity_past_the_bound_is_not_taken() {
    assert_eq!(chunk_bytes(MIB, PAGE, 512, 4096), MIB);
    assert_eq!(chunk_bytes(MIB, PAGE, 512, u32::MAX), MIB);
}

#[test]
#[should_panic(expected = "is not a multiple of")]
fn a_bound_off_the_alignment_is_refused() {
    chunk_bytes(MIB + 512, PAGE, 512, 0);
}

#[test]
#[should_panic(expected = "chunk of 512-byte blocks")]
fn a_chunk_off_the_block_is_refused() {
    read(&mut disk(512), 0, 512, 700, &mut [0u8; 1024]).ok();
}
