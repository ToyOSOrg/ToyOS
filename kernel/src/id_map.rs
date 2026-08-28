use core::hash::Hash;
use core::ops::Add;
use hashbrown::HashMap;

/// Map with auto-incrementing, never-reused keys.
pub struct IdMap<K, V> {
    map: HashMap<K, V>,
    next: K,
}

pub trait IdKey: Copy + Eq + Hash + Ord + Add<Output = Self> {
    const ZERO: Self;
    const ONE: Self;
}

// No impl for u32/u64/usize: that would let a bare integer key an IdMap.
impl IdKey for toyos_abi::Pid {
    const ZERO: Self = Self(0);
    const ONE: Self = Self(1);
}
impl IdKey for toyos_abi::Tid {
    const ZERO: Self = Self(0);
    const ONE: Self = Self(1);
}

impl<K: IdKey, V> IdMap<K, V> {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
            next: K::ZERO,
        }
    }

    /// Inserts `value`, returning its auto-assigned ID.
    pub fn insert(&mut self, value: V) -> K {
        let id = self.next;
        self.next = self.next + K::ONE;
        self.map.insert(id, value);
        id
    }

    /// Inserts the value `f` builds from the pre-assigned ID, returning it.
    /// Avoids ever building the value with an invalid placeholder ID (e.g. `pid: 0`).
    pub fn insert_with(&mut self, f: impl FnOnce(K) -> V) -> K {
        let id = self.next;
        self.next = self.next + K::ONE;
        let value = f(id);
        self.map.insert(id, value);
        id
    }

    pub fn get(&self, id: K) -> Option<&V> {
        self.map.get(&id)
    }

    pub fn get_mut(&mut self, id: K) -> Option<&mut V> {
        self.map.get_mut(&id)
    }

    pub fn remove(&mut self, id: K) -> Option<V> {
        self.map.remove(&id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (K, &V)> {
        self.map.iter().map(|(&k, v)| (k, v))
    }

}
