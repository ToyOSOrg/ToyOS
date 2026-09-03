//! The one `BuildHasher` a kernel hash container may use, seeded from `RDRAND`
//! before any container exists. `kernel/Cargo.toml` takes hashbrown without
//! `default-hasher`, so `HashMap::new` / `with_capacity` do not exist and every
//! spelling of a container stops compiling until it names this.
//!
//! **A container built before [`seed`] is the one wrong answer that would be
//! silent**, because it would work: it hashes alike on every boot of an image,
//! the property a `BTreeMap` is chosen over for a boundary-crossing key. So
//! [`KernelHashState::new`] panics by name. Key origins: `src/kernelkeys.rs`.

use core::hash::{BuildHasher, Hasher};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::cpu;

/// `0` until [`seed`] runs — the one value it refuses to draw, so it means
/// "not seeded" and nothing else.
static SEED: AtomicU64 = AtomicU64::new(0);

pub const UNSEEDED: &str =
    "kernel hasher: a hash container was built before hasher::seed(), so its order is fixed \
     across every boot of this image";

/// Two seeds in one boot means a container was built against the first.
const RESEEDED: &str = "kernel hasher: seed() ran twice in one boot";

/// Draw the boot's seed. Called once, before any container is built.
pub fn seed() {
    let mut drawn = cpu::rdrand();
    while drawn == 0 {
        drawn = cpu::rdrand();
    }
    assert_eq!(SEED.swap(drawn, Ordering::Release), 0, "{RESEEDED}");
}

/// splitmix64's finalizer: one input bit changed moves half the output bits.
const fn mix(mut x: u64) -> u64 {
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^ (x >> 31)
}

/// Carries the seed by value, so a container hashes alike for its whole life.
#[derive(Clone, Copy, Debug)]
pub struct KernelHashState(u64);

impl KernelHashState {
    pub fn new() -> Self {
        let seed = SEED.load(Ordering::Acquire);
        assert!(seed != 0, "{UNSEEDED}");
        Self(seed)
    }
}

impl Default for KernelHashState {
    fn default() -> Self {
        Self::new()
    }
}

impl BuildHasher for KernelHashState {
    type Hasher = KernelHasher;

    fn build_hasher(&self) -> KernelHasher {
        KernelHasher(self.0)
    }
}

pub struct KernelHasher(u64);

impl Hasher for KernelHasher {
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for chunk in &mut chunks {
            let word = u64::from_le_bytes(chunk.try_into().expect("chunks_exact(8)"));
            self.0 = mix(self.0 ^ word);
        }
        let tail = chunks.remainder();
        if !tail.is_empty() {
            let mut last = [0u8; 8];
            last[..tail.len()].copy_from_slice(tail);
            self.0 = mix(self.0 ^ u64::from_le_bytes(last));
        }
        // The length too, or `b"ab"` and `b"ab\0"` differ only in padding.
        self.0 = mix(self.0 ^ bytes.len() as u64);
    }

    fn write_u8(&mut self, n: u8) {
        self.write_u64(n as u64);
    }

    fn write_u32(&mut self, n: u32) {
        self.write_u64(n as u64);
    }

    fn write_u64(&mut self, n: u64) {
        self.0 = mix(self.0 ^ n);
    }

    fn write_usize(&mut self, n: usize) {
        self.write_u64(n as u64);
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

/// The only `HashMap` `kernel/src` may name; a site reads `default()` because
/// `new` belongs to the default hasher this kernel does not have.
pub type HashMap<K, V> = hashbrown::HashMap<K, V, KernelHashState>;

/// Build a container before [`seed`]: the panic is the point.
#[cfg(feature = "boot-actuators")]
pub fn probe_before_seed() {
    let mut probe: HashMap<u64, u64> = HashMap::default();
    probe.insert(0, 0);
}
