//! The kernel's x86-64 copy on an x86-64 host: the descending copy over every
//! length to 300 and some past it, at every destination offset in a store;
//! every rule's copy at every page distance below 1 KiB and some above; and
//! which copies each rule sends down.

#[cfg(all(test, target_arch = "x86_64"))]
#[path = "../../src/arch/x86_64/copy.rs"]
mod copy;

#[cfg(all(test, target_arch = "x86_64"))]
mod tests {
    use super::copy::{self, Rule};

    fn pattern(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 7 + i / 251) as u8).collect()
    }

    /// A copy from `at` bytes past a 64-byte boundary to `distance` above
    /// the next page past its end, in one buffer that ends 64 bytes past the
    /// destination: the whole buffer against `copy_within`.
    fn agrees(what: &str, copy: impl Fn(*mut u8, *const u8, usize), distance: usize, at: usize, n: usize) {
        let gap = (at + n).next_multiple_of(4096).max(4096);
        let mut got = pattern(64 + gap + distance + n + 64);
        let base = got.as_ptr().align_offset(64);
        let (from, to) = (base + at, base + gap + distance);
        let mut want = got.clone();
        want.copy_within(from..from + n, to);
        let p = got.as_mut_ptr();
        // SAFETY: both ranges are inside `got`, the destination past the source's end.
        copy(unsafe { p.add(to) }, unsafe { p.add(from) }, n);
        assert_eq!(got, want, "{what}: n {n} at {at} distance {distance}");
    }

    #[test]
    fn the_descending_copy_agrees() {
        for n in (8..300).chain([511, 512, 513, 4095, 4096, 4097, 16389]) {
            for distance in 0..8 {
                for at in [0, 3, 17] {
                    // SAFETY: `agrees` hands disjoint ranges of `n` bytes, at least 8.
                    agrees("copy_down", |d, s, n| unsafe { copy::copy_down(d, s, n) }, distance, at, n);
                }
            }
        }
    }

    #[test]
    fn every_rule_copies_at_every_page_distance() {
        let everything = Rule { window: 4096 };
        for rule in [Rule::NEVER, Rule::AMD, everything] {
            for distance in (0..1024).chain([2048, 4032, 4095]) {
                for n in [0, 1, 63, 64, 65, 200, 1000, 4096, 16384] {
                    // SAFETY: as above, `n` any length.
                    agrees(&format!("{rule:?}"), |d, s, n| unsafe { copy::copy_by(d, s, n, rule) }, distance, 5, n);
                }
            }
        }
    }

    #[test]
    fn each_rule_sends_down_only_a_short_distance() {
        let down = |rule: Rule, distance: usize, n: usize| rule.goes_down(0x10_0000 + 4096 * 3 + distance, 0x10_0000, n);
        for rule in [Rule::NEVER, Rule::AMD] {
            assert!(!down(rule, 0, 8192), "{rule:?}: distance 0");
            assert!(!down(rule, 16, 63), "{rule:?}: under 64 bytes");
            assert!(!down(rule, 4080, 8192), "{rule:?}: 16 bytes below modulo 4 KiB");
        }
        assert!(!down(Rule::NEVER, 16, 8192));
        assert!(down(Rule::AMD, 1, 64) && down(Rule::AMD, 31, 8192) && down(Rule::AMD, 16, 64 << 20));
        assert!(!down(Rule::AMD, 32, 8192));
        // A destination a page less 16 below its source is 16 above it modulo 4 KiB.
        assert!(Rule::AMD.goes_down(0x10_0000, 0x10_0000 + 4080, 4080));
    }
}
