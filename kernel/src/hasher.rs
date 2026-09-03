//! The `BuildHasher` every kernel hash container is built on, seeded from
//! `RDRAND` before any container exists — `kernel/Cargo.toml` takes hashbrown
//! without `default-hasher`, so a container must name a hasher.
//!
//! **A container built before [`seed`], or on a constant, is the wrong answer
//! that would be silent** — it works, and hashes alike on every boot of an
//! image, the fixed order a `BTreeMap` is chosen over for a boundary-crossing
//! key. Every way that can happen panics by name. Not a HashDoS defence, though:
//! [`mix`] is splitmix64's finalizer XOR-keyed, so one observed hash of a known
//! key recovers the seed.

use core::hash::{BuildHasher, Hasher};
use core::sync::atomic::{AtomicU64, Ordering};

use crate::arch::cpu;

/// `0` until [`seed`] runs, and a value it refuses to draw.
static SEED: AtomicU64 = AtomicU64::new(0);

pub const UNSEEDED: &str =
    "kernel hasher: a hash container was built before hasher::seed(), so its order is fixed \
     across every boot of this image";

/// Two seeds in one boot means a container was built against the first.
const RESEEDED: &str = "kernel hasher: seed() ran twice in one boot";

pub const NO_RDRAND: &str =
    "kernel hasher: CPUID.01H:ECX[30] is clear, so this CPU has no RDRAND and the seed has no \
     source";

pub const NO_ENTROPY: &str =
    "kernel hasher: RDRAND gave no usable value, so the seed would be a constant on every boot";

/// Called once, before any container. `0` and all-ones are what a failing
/// `RDRAND` leaves behind, so neither may become a seed.
pub fn seed() {
    assert!(cpu::has_rdrand(), "{NO_RDRAND}");
    let drawn = (0..cpu::RDRAND_ATTEMPTS)
        .filter_map(|_| cpu::rdrand())
        .find(|&v| v != 0 && v != u64::MAX)
        .unwrap_or_else(|| panic!("{NO_ENTROPY}"));
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
        let (words, tail) = bytes.as_chunks::<8>();
        for word in words {
            self.0 = mix(self.0 ^ u64::from_le_bytes(*word));
        }
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

/// A site reads `default()` because `new` belongs to a default hasher this
/// kernel does not have.
pub type HashMap<K, V> = hashbrown::HashMap<K, V, KernelHashState>;

/// Build a container before [`seed`]: the panic is the point.
#[cfg(feature = "boot-actuators")]
pub fn probe_before_seed() {
    let mut probe: HashMap<u64, u64> = HashMap::default();
    probe.insert(0, 0);
}

/// **What the feature drop does not close**, as code so that closing either
/// stops this compiling: a foreign `BuildHasher`, and `hashbrown::HashTable`,
/// which needs none. Which hasher a container gets is held by review.
#[allow(dead_code)]
#[cfg(feature = "boot-actuators")]
pub fn spellings_the_compiler_still_admits() {
    #[derive(Default)]
    struct Unseeded;
    impl BuildHasher for Unseeded {
        type Hasher = KernelHasher;
        fn build_hasher(&self) -> KernelHasher {
            KernelHasher(0)
        }
    }
    let mut foreign: hashbrown::HashMap<u64, u64, Unseeded> = hashbrown::HashMap::default();
    foreign.insert(0, 0);

    let mut table: hashbrown::HashTable<u64> = hashbrown::HashTable::new();
    table.insert_unique(0, 0, |v| *v);
}
