//! A thread's whole vector register file, read and held as bytes.

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

#[cfg(target_arch = "x86_64")]
mod x86_64 {
    use core::arch::asm;
    use core::arch::x86_64::__cpuid_count;

    /// Which vector registers this machine gives a thread. Only [`width`]
    /// makes one, so holding one is having checked what std reports.
    #[derive(Clone, Copy, Debug)]
    pub struct Width(Kind);

    #[derive(Clone, Copy, Debug)]
    enum Kind {
        /// `ymm0`-`ymm15`.
        Avx,
        /// `zmm0`-`zmm31`, then the opmasks `k1`-`k7`; `k0` is LLVM's.
        Avx512,
    }

    const YMM: usize = 32;
    const ZMM: usize = 64;
    const K: usize = 2;

    impl Width {
        /// How many bytes the file is.
        pub fn bytes(self) -> usize {
            match self.0 {
                Kind::Avx => 16 * YMM,
                Kind::Avx512 => 32 * ZMM + 7 * K,
            }
        }

        /// The register byte `at` of the file lies in, and whether the
        /// declared initial state makes it zero: everything above `xmm0`-
        /// `xmm15`, which the program's own code had every right to use
        /// before reading.
        fn register(self, at: usize) -> (String, bool) {
            match self.0 {
                Kind::Avx => (format!("ymm{}", at / YMM), at % YMM >= 16),
                Kind::Avx512 if at < 32 * ZMM => (format!("zmm{}", at / ZMM), at / ZMM >= 16 || at % ZMM >= 16),
                Kind::Avx512 => (format!("k{}", 1 + (at - 32 * ZMM) / K), true),
            }
        }

        /// The register the first byte the two files disagree about lies in.
        pub fn first_difference(self, got: &[u8], want: &[u8]) -> Option<String> {
            got.iter().zip(want).position(|(a, b)| a != b).map(|at| self.register(at).0)
        }

        /// The first register of `file` holding a nonzero byte where the
        /// declared initial state is zero: a value left by another thread.
        pub fn first_inherited(self, file: &[u8]) -> Option<String> {
            file.iter().enumerate().find(|&(at, &b)| b != 0 && self.register(at).1).map(|(at, _)| self.register(at).0)
        }
    }

    /// The vector state std says this machine gives userland, held to what
    /// the CPU's own CPUID enumerates: AVX always, since the kernel refuses a
    /// CPU without it, and AVX-512 exactly where the CPU has it.
    pub fn width() -> Width {
        assert!(std::is_x86_feature_detected!("avx"), "std reports no AVX, which every CPU the kernel runs on has");
        let max_leaf = __cpuid_count(0, 0).eax;
        let cpu_avx512 = max_leaf >= 0xd
            && __cpuid_count(7, 0).ebx & (1 << 16) != 0
            && __cpuid_count(0xd, 0).eax & 0xe0 == 0xe0;
        let std_avx512 = std::is_x86_feature_detected!("avx512f");
        assert_eq!(
            std_avx512, cpu_avx512,
            "std reports AVX-512F {std_avx512} on a CPU whose CPUID enumerates it {cpu_avx512}"
        );
        Width(if std_avx512 { Kind::Avx512 } else { Kind::Avx })
    }

    /// `ymm{0..16}` stored to, or loaded from, `[{p}]`.
    macro_rules! ymm {
        (store $($n:literal)*) => { concat!($("vmovdqu ymmword ptr [{p} + ", $n, "*32], ymm", $n, "\n",)*) };
        (load $($n:literal)*) => { concat!($("vmovdqu ymm", $n, ", ymmword ptr [{p} + ", $n, "*32]\n",)*) };
    }
    /// `zmm{0..32}` and `k{1..8}` the same way, the opmasks after the 2048 bytes of `zmm`.
    macro_rules! zmm {
        (store $($n:literal)*) => { concat!($("vmovdqu64 zmmword ptr [{p} + ", $n, "*64], zmm", $n, "\n",)*) };
        (load $($n:literal)*) => { concat!($("vmovdqu64 zmm", $n, ", zmmword ptr [{p} + ", $n, "*64]\n",)*) };
        (kstore $($n:literal)*) => { concat!($("kmovw word ptr [{p} + 2048 + (", $n, "-1)*2], k", $n, "\n",)*) };
        (kload $($n:literal)*) => { concat!($("kmovw k", $n, ", word ptr [{p} + 2048 + (", $n, "-1)*2]\n",)*) };
    }

    #[target_feature(enable = "avx")]
    unsafe fn read_avx(p: *mut u8) {
        asm!(
            ymm!(store 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15),
            p = in(reg) p,
            options(nostack, preserves_flags),
        );
    }

    #[target_feature(enable = "avx512f")]
    unsafe fn read_avx512(p: *mut u8) {
        asm!(
            zmm!(store 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31),
            zmm!(kstore 1 2 3 4 5 6 7),
            p = in(reg) p,
            options(nostack, preserves_flags),
        );
    }

    #[target_feature(enable = "avx")]
    unsafe fn hold_avx(p: *mut u8, spins: u64) {
        asm!(
            ymm!(load 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15),
            "2:",
            "dec rcx",
            "jnz 2b",
            ymm!(store 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15),
            p = in(reg) p,
            inout("rcx") spins => _,
            clobber_abi("C"),
            options(nostack),
        );
    }

    #[target_feature(enable = "avx512f")]
    unsafe fn hold_avx512(p: *mut u8, spins: u64) {
        asm!(
            zmm!(load 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31),
            zmm!(kload 1 2 3 4 5 6 7),
            "2:",
            "dec rcx",
            "jnz 2b",
            zmm!(store 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 21 22 23 24 25 26 27 28 29 30 31),
            zmm!(kstore 1 2 3 4 5 6 7),
            p = in(reg) p,
            inout("rcx") spins => _,
            clobber_abi("C"),
            options(nostack),
        );
    }

    /// This thread's vector registers, as they are.
    pub fn read(width: Width) -> Vec<u8> {
        let mut file = vec![0u8; width.bytes()];
        // SAFETY: `width` was made by `width()`, which saw std report the
        // features; each store writes `width.bytes()` bytes, the vector's own.
        unsafe {
            match width.0 {
                Kind::Avx => read_avx(file.as_mut_ptr()),
                Kind::Avx512 => read_avx512(file.as_mut_ptr()),
            }
        }
        file
    }

    /// `file` loaded into every vector register, held there through `spins`
    /// turns of a loop that touches nothing else, and read back.
    pub fn hold(width: Width, file: &[u8], spins: u64) -> Vec<u8> {
        assert_eq!(file.len(), width.bytes());
        assert!(spins > 0, "`dec` from zero would spin 2^64 times");
        let mut held = file.to_vec();
        // SAFETY: as `read`'s; the loads and stores are `width.bytes()` long,
        // and every vector register the block writes is declared clobbered.
        unsafe {
            match width.0 {
                Kind::Avx => hold_avx(held.as_mut_ptr(), spins),
                Kind::Avx512 => hold_avx512(held.as_mut_ptr(), spins),
            }
        }
        held
    }
}
