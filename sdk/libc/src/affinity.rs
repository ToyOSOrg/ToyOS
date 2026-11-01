//! A `cpu_set_t` as Linux's `sched_setaffinity` and `sched_getaffinity` read
//! and fill one: CPU `n` is bit `n % 64` of `unsigned long` `n / 64`, by the
//! kernel's dense index. It reads and sets nothing but what it is handed, so
//! the host tests it (`toyos-libc-copies`).

use crate::errcode::EINVAL;

pub(crate) use toyos_abi::syscall::CPU_MASK_WORDS as WORDS;

/// The mask a `cpusetsize`-byte set asks for on a machine of `online` CPUs, as
/// Linux reads it: CPUs past the online count are dropped, and a set naming
/// none of the online ones is `EINVAL`.
pub(crate) fn requested(set: &[u8], online: usize) -> Result<[u64; WORDS], i32> {
    let mut mask = [0u64; WORDS];
    for (i, word) in mask.iter_mut().enumerate() {
        let mut bytes = [0u8; 8];
        let from = (i * 8).min(set.len());
        let to = (i * 8 + 8).min(set.len());
        bytes[..to - from].copy_from_slice(&set[from..to]);
        let first = i * 64;
        let online_bits = match online.saturating_sub(first) {
            0 => 0,
            n if n >= 64 => u64::MAX,
            n => (1 << n) - 1,
        };
        *word = u64::from_ne_bytes(bytes) & online_bits;
    }
    if mask.iter().all(|&w| w == 0) {
        return Err(EINVAL);
    }
    Ok(mask)
}

/// Fill a `cpusetsize`-byte set with `mask`, zero past it; one too short to
/// hold every online CPU is `EINVAL`, as Linux refuses it.
pub(crate) fn fill(set: &mut [u8], mask: &[u64; WORDS], online: usize) -> Result<(), i32> {
    if set.len() * 8 < online {
        return Err(EINVAL);
    }
    set.fill(0);
    for (chunk, word) in set.chunks_mut(8).zip(mask.iter()) {
        chunk.copy_from_slice(&word.to_ne_bytes()[..chunk.len()]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_drops_cpus_past_the_online_count() {
        let full = [0xffu8; 128];
        let mask = requested(&full, 6).unwrap();
        assert_eq!(mask[0], 0b11_1111);
        assert!(mask[1..].iter().all(|&w| w == 0));
    }

    #[test]
    fn a_set_naming_no_online_cpu_is_einval() {
        assert_eq!(requested(&[0; 128], 8), Err(EINVAL));
        let mut set = [0u8; 128];
        set[1] = 1;
        assert_eq!(requested(&set, 8), Err(EINVAL), "cpu8 is not online on an eight-CPU machine");
        assert_eq!(requested(&set, 9).unwrap()[0], 1 << 8);
    }

    #[test]
    fn a_short_set_reads_as_its_bytes_and_zero_past_them() {
        assert_eq!(requested(&[0b100], 64).unwrap()[0], 0b100);
        let mut set = [0u8; 9];
        set[8] = 1;
        assert_eq!(requested(&set, 128).unwrap()[1], 1);
    }

    #[test]
    fn fill_writes_the_mask_and_refuses_a_set_too_short() {
        let mut mask = [0u64; WORDS];
        mask[0] = 0b1010;
        let mut set = [0xffu8; 16];
        fill(&mut set, &mask, 8).unwrap();
        assert_eq!(set[0], 0b1010);
        assert!(set[1..].iter().all(|&b| b == 0));
        assert_eq!(fill(&mut [0u8; 1], &mask, 9), Err(EINVAL));
    }
}
