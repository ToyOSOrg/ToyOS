//! What `SYS_RANDOM` answers, from inside the machine.
//!
//! What it asserts itself is what holds of one boot on every machine: a draw
//! fills exactly its window, no two draws are alike however many threads draw
//! at once, and the bytes are not grossly biased. Its one line carries a draw
//! for the host, which boots twice and compares: only it can see that two
//! boots differ.

use std::collections::HashSet;

use toyos_abi::syscall::random;

/// What a window holds before a draw, and what the bytes past it still hold after.
const UNDRAWN: u8 = 0xa5;

/// Threads drawing at once, and the draws each makes.
const THREADS: usize = 8;
const DRAWS: usize = 1000;

fn draw32() -> [u8; 32] {
    let mut bytes = [UNDRAWN; 32];
    random(&mut bytes).expect("SYS_RANDOM refused a 32-byte window");
    bytes
}

fn main() {
    random(&mut []).expect("SYS_RANDOM refused an empty window");

    // A length no multiple of a ChaCha20 block or of the kernel's own chunk:
    // every byte of the window is drawn and none past it is written.
    let mut odd = [UNDRAWN; 1031 + 16];
    random(&mut odd[..1031]).expect("SYS_RANDOM refused a 1031-byte window");
    assert!(odd[1031..].iter().all(|&b| b == UNDRAWN), "a draw wrote past its window");
    assert!(odd[1015..1031].iter().any(|&b| b != UNDRAWN), "a draw left the end of its window undrawn");
    let blocks: HashSet<&[u8]> = odd[..1024].chunks(64).collect();
    assert_eq!(blocks.len(), 16, "two 64-byte blocks of one draw are alike");

    // 32768 bits, each a coin: the ones are within eight standard deviations
    // (8 * sqrt(32768) / 2 = 724) of half, or the bytes are not a keystream.
    let mut page = [0u8; 4096];
    random(&mut page).expect("SYS_RANDOM refused a 4096-byte window");
    let ones: u32 = page.iter().map(|b| b.count_ones()).sum();
    assert!(ones.abs_diff(16384) < 724, "{ones} of 32768 drawn bits are ones");

    let drawers: Vec<_> = (0..THREADS)
        .map(|_| std::thread::spawn(|| (0..DRAWS).map(|_| draw32()).collect::<Vec<_>>()))
        .collect();
    let mut seen = HashSet::new();
    for drawer in drawers {
        for bytes in drawer.join().expect("a drawing thread panicked") {
            assert!(seen.insert(bytes), "two draws gave the same 32 bytes");
        }
    }

    let hex: String = draw32().iter().map(|b| format!("{b:02x}")).collect();
    println!("random_draws: {THREADS} threads drew {DRAWS} times each and no two draws were alike; one more: {hex}");
}
