//! x86-64 has these probes elsewhere, or owes them.

pub mod fp_isolation {
    pub fn main(_args: Vec<String>) {
        panic!("fp_isolation probes AArch64's FP/SIMD switch; x86-64's is test_rs_fpu_isolation");
    }
}

pub mod first_entry {
    pub fn main(_args: Vec<String>) {
        panic!(
            "first_entry probes AArch64's first entry to EL0; x86-64's is owed by \
             issues/isolation/a-new-x86-thread-enters-ring-3-holding-kernel-register-values.md"
        );
    }
}
