//! What a parent tells the child it starts: its arguments and its
//! environment, a block the parent writes into memory of its own and lays out
//! in the child at [`AT`], directly below the program
//! (`crate::elf::program`). The kernel copies its pages as it copies the
//! program's and reads none of it; every copy of this crate in the child
//! reads it where it lies.
//!
//! ```text
//! AT  argc | argv[0] .. argv[argc - 1] | 0 | envp[0] .. | 0 | the strings
//! ```
//!
//! Eight-byte words, each pointer the address in the child of a string ended
//! by a NUL, an environment's `KEY=VALUE`. A page of zeros is a block that
//! says nothing, which a program laid out and given no block starts with.
//!
//! **The child trusts it as it trusts its own program**, which the same
//! parent laid out: nothing here checks a pointer.

use alloc::vec::Vec;
use core::ffi::CStr;

use toyos_abi::syscall::{SyscallError, PAGE_SIZE};

use crate::elf::program::BASE;
use crate::shm::SharedMemory;
use crate::{AsHandle, BorrowedHandle};

/// The bytes below the program kept for the block: its pages, and reserved
/// ones up to the program.
const RESERVED: u64 = 2 * 1024 * 1024;

/// Where the block lies in every process.
pub const AT: u64 = BASE - RESERVED;

/// The most bytes of a block: a reserved page is left below the program.
pub const MAX_BLOCK: usize = (RESERVED - PAGE_SIZE as u64) as usize;

fn strings(run: &[u8]) -> impl Iterator<Item = &[u8]> {
    run.split(|&b| b == 0).filter(|s| !s.is_empty())
}

/// The bytes of the block that says `args` and `env`. `None` past [`MAX_BLOCK`].
fn bytes(args: &[u8], env: &[u8]) -> Option<Vec<u8>> {
    let argc = strings(args).count();
    let words = argc + strings(env).count() + 3;
    let text: usize = strings(args).chain(strings(env)).map(|s| s.len() + 1).sum();
    if words * 8 + text > MAX_BLOCK {
        return None;
    }
    let mut block = alloc::vec![0u8; words * 8];
    block.reserve_exact(text);
    block[..8].copy_from_slice(&(argc as u64).to_ne_bytes());
    let mut word = 1;
    for run in [args, env] {
        for string in strings(run) {
            let at = AT + block.len() as u64;
            block[word * 8..word * 8 + 8].copy_from_slice(&at.to_ne_bytes());
            block.extend_from_slice(string);
            block.push(0);
            word += 1;
        }
        // The null that ends the run's pointers.
        word += 1;
    }
    Some(block)
}

/// Why no block was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The arguments and environment are past [`MAX_BLOCK`].
    TooLarge,
    /// The kernel made no memory for it.
    Memory(SyscallError),
}

/// A block in a memory object, which a spawn names as the pages its child
/// starts with at [`AT`] (`Program::starting_with`) and may drop once the
/// spawn has answered.
pub struct Block {
    memory: SharedMemory,
    len: usize,
}

impl Block {
    /// The block that says `args` and `env`, each a run of strings a NUL
    /// separates, an empty one skipped, in memory counted to the job `job` is
    /// a handle to carrying `WRITE`, the one the child runs in, or else to
    /// this process's own.
    pub fn new(args: &[u8], env: &[u8], job: Option<BorrowedHandle<'_>>) -> Result<Block, Refused> {
        let bytes = bytes(args, env).ok_or(Refused::TooLarge)?;
        let memory = match job {
            Some(job) => SharedMemory::create_for(job, bytes.len()),
            None => SharedMemory::create(bytes.len()),
        }
        .map_err(Refused::Memory)?;
        memory.bytes().write(0, &bytes);
        Ok(Block { memory, len: bytes.len() })
    }

    /// The object's handle, as a `SpawnMap` names it.
    pub fn object(&self) -> u64 {
        u64::from(self.memory.as_handle().raw().0)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// This process's argument count and its arguments, a null after the last.
/// A program may write them, as C lets `main` write its `argv`.
pub fn args() -> (usize, *const *const u8) {
    let at = AT as *const u64;
    // SAFETY: a program `crate::elf::program::lay_out` laid out starts with
    // the block's first page mapped at `AT`, and a block's pointers follow
    // its count.
    unsafe { (at.read() as usize, at.add(1).cast()) }
}

/// The environment this process started with, each entry without its NUL.
/// Nothing hands out a pointer to these strings, so nothing writes them.
pub fn env() -> impl Iterator<Item = &'static [u8]> {
    let (argc, argv) = args();
    // SAFETY: the environment's pointers follow the null that ends the
    // arguments', are ended by a null, and each names a string of the block.
    let envp = unsafe { argv.add(argc + 1) };
    (0..)
        .map(move |i| unsafe { envp.add(i).read() })
        .take_while(|entry| !entry.is_null())
        .map(|entry| unsafe { CStr::from_ptr(entry.cast()) }.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The string a block's word `word` points at.
    fn string(block: &[u8], word: usize) -> Option<&[u8]> {
        let at = u64::from_ne_bytes(block[word * 8..word * 8 + 8].try_into().unwrap());
        if at == 0 {
            return None;
        }
        let rest = &block[(at - AT) as usize..];
        Some(&rest[..rest.iter().position(|&b| b == 0).expect("a string ends inside the block")])
    }

    /// The count, then each run's pointers in order and a null after each,
    /// every pointer the address its string lies at once the block is at `AT`.
    #[test]
    fn a_block_s_pointers_name_its_strings_in_order() {
        let block = bytes(b"/bin/echo\0-n\0hello world", b"HOME=/home/toy\0PWD=/\0").unwrap();
        assert_eq!(u64::from_ne_bytes(block[..8].try_into().unwrap()), 3);
        let words: Vec<_> = (1..8).map(|word| string(&block, word)).collect();
        assert_eq!(
            words,
            [Some(&b"/bin/echo"[..]), Some(b"-n"), Some(b"hello world"), None, Some(b"HOME=/home/toy"), Some(b"PWD=/"), None]
        );
        assert_eq!(block.len(), 8 * 8 + 10 + 3 + 12 + 15 + 6);
    }

    /// Nothing said is a count of zero and two nulls: what a page of zeros reads as.
    #[test]
    fn a_block_that_says_nothing_is_zeros() {
        assert_eq!(bytes(b"", b"\0\0").unwrap(), [0u8; 24]);
    }

    /// A block ends below the program with a page to spare, and one byte more is refused.
    #[test]
    fn a_block_past_its_reservation_is_refused() {
        let fits = alloc::vec![b'a'; MAX_BLOCK - 4 * 8 - 1];
        assert_eq!(bytes(&fits, b"").map(|block| block.len()), Some(MAX_BLOCK));
        let mut past = fits;
        past.push(b'a');
        assert_eq!(bytes(&past, b""), None);
    }
}
