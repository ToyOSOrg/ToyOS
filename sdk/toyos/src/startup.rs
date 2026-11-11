//! What a parent tells the child it starts: its arguments, its environment
//! and what it calls each handle it endowed, a block the parent writes into
//! memory of its own and lays out in the child at [`AT`], directly below the
//! program (`crate::elf::program`). The kernel copies its pages as it copies
//! the program's and reads none of it; every copy of this crate in the child
//! reads it where it lies.
//!
//! ```text
//! AT  names | argc | argv[0] .. argv[argc - 1] | 0 | envp[0] .. | 0 | the strings | the names
//!
//! names   bytes u32 | count u32 | count x { len u16 | name }
//! ```
//!
//! Eight-byte words, each pointer the address in the child of a string ended
//! by a NUL, an environment's `KEY=VALUE`; the first word is the address of
//! the names, or 0 for none. A page of zeros is a block that says nothing,
//! which a program laid out and given no block starts with.
//!
//! **The names are the child's endowment table**: entry `i` is what its
//! parent calls the handle at `RawHandle::endowed(i)`, in the order the spawn
//! endowed them (`toyos_abi::syscall::SpawnArgs`). `bytes` counts the whole
//! table, and nothing aligns an entry.
//!
//! **The child trusts the arguments and environment as it trusts its own
//! program**, which the same parent laid out: nothing checks a pointer of
//! theirs. The names it checks once, whole ([`names`]), before anything looks
//! one up: a table that is not one ends the child ([`MALFORMED_STATUS`]).

use alloc::vec::Vec;
use core::ffi::CStr;

use toyos_abi::syscall::{SyscallError, MAX_SPAWN_ENDOWMENTS, PAGE_SIZE};

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

/// A table's two counts.
const TABLE_HEADER: usize = 8;

/// The status a process whose endowment table is malformed exits with,
/// having said so on its `fd:2` where the table still names one.
pub const MALFORMED_STATUS: i32 = 125;

fn strings(run: &[u8]) -> impl Iterator<Item = &[u8]> {
    run.split(|&b| b == 0).filter(|s| !s.is_empty())
}

/// The table that calls the `i`th endowment `names[i]`. `None` for more
/// names than a spawn endows, or a name past what its length word holds.
fn table(names: &[&str]) -> Option<Vec<u8>> {
    if names.len() > MAX_SPAWN_ENDOWMENTS {
        return None;
    }
    let bytes = TABLE_HEADER + names.iter().map(|name| 2 + name.len()).sum::<usize>();
    let mut table = Vec::with_capacity(bytes);
    table.extend_from_slice(&u32::try_from(bytes).ok()?.to_ne_bytes());
    table.extend_from_slice(&(names.len() as u32).to_ne_bytes());
    for name in names {
        table.extend_from_slice(&u16::try_from(name.len()).ok()?.to_ne_bytes());
        table.extend_from_slice(name.as_bytes());
    }
    Some(table)
}

/// The bytes of the block that says `args`, `env` and `names`. `None` past
/// [`MAX_BLOCK`], or for names [`table`] makes none of.
fn bytes(args: &[u8], env: &[u8], names: &[&str]) -> Option<Vec<u8>> {
    let table = if names.is_empty() { Vec::new() } else { table(names)? };
    let argc = strings(args).count();
    let words = argc + strings(env).count() + 4;
    let text: usize = strings(args).chain(strings(env)).map(|s| s.len() + 1).sum();
    if words * 8 + text + table.len() > MAX_BLOCK {
        return None;
    }
    let mut block = alloc::vec![0u8; words * 8];
    block.reserve_exact(text + table.len());
    block[8..16].copy_from_slice(&(argc as u64).to_ne_bytes());
    let mut word = 2;
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
    if !table.is_empty() {
        let at = AT + block.len() as u64;
        block[..8].copy_from_slice(&at.to_ne_bytes());
        block.extend_from_slice(&table);
    }
    Some(block)
}

/// Why no block was made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// The arguments, environment and names are past [`MAX_BLOCK`], or the
    /// names more than a spawn endows.
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
    /// separates, an empty one skipped, and calls the spawn's `i`th endowment
    /// `names[i]`, in memory counted to the job `job` is a handle to carrying
    /// `WRITE`, the one the child runs in, or else to this process's own.
    pub fn new(args: &[u8], env: &[u8], names: &[&str], job: Option<BorrowedHandle<'_>>) -> Result<Block, Refused> {
        let bytes = bytes(args, env, names).ok_or(Refused::TooLarge)?;
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
    unsafe { (at.add(1).read() as usize, at.add(2).cast()) }
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

/// Why a table is none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Malformed {
    /// It does not lie inside the block's reservation.
    Outside,
    /// Its length word is not its length, or is less than its two counts.
    Length,
    /// It counts more names than a spawn endows.
    Count,
    /// A name runs past its end.
    Truncated,
    /// Bytes follow its last name.
    Trailing,
    /// A name is not UTF-8.
    NotUtf8,
    /// A name is empty.
    Empty,
    /// A name is an earlier one's.
    Twice,
}

fn word(table: &[u8], at: usize) -> Option<u32> {
    let bytes = table.get(at..at.checked_add(4)?)?;
    Some(u32::from_ne_bytes([*bytes.first()?, *bytes.get(1)?, *bytes.get(2)?, *bytes.get(3)?]))
}

/// A table's entries, each a name's bytes, ending with the first that runs
/// past the table. Unchecked beyond that, and reached before this image is
/// relocated ([`early`]): nothing here panics, formats or names a static.
fn entries(table: &[u8]) -> impl Iterator<Item = Result<&[u8], Malformed>> {
    let count = word(table, 4).unwrap_or(0) as usize;
    let mut next = Some(TABLE_HEADER);
    (0..count).map_while(move |_| {
        let at = next?;
        let name = table
            .get(at..at.saturating_add(2))
            .and_then(|len| Some(usize::from(u16::from_ne_bytes([*len.first()?, *len.get(1)?]))))
            .and_then(|len| table.get(at.saturating_add(2)..at.saturating_add(2).saturating_add(len)));
        next = name.map(|name| at.saturating_add(2).saturating_add(name.len()));
        Some(name.ok_or(Malformed::Truncated))
    })
}

/// The names `table` holds, written to `into` in order: how many, or why it
/// is no table.
pub fn names<'a>(table: &'a [u8], into: &mut [&'a str; MAX_SPAWN_ENDOWMENTS]) -> Result<usize, Malformed> {
    if word(table, 0).map(|bytes| bytes as usize) != Some(table.len()) || table.len() < TABLE_HEADER {
        return Err(Malformed::Length);
    }
    let count = word(table, 4).ok_or(Malformed::Length)? as usize;
    if count > MAX_SPAWN_ENDOWMENTS {
        return Err(Malformed::Count);
    }
    let mut end = TABLE_HEADER;
    for (i, name) in entries(table).enumerate() {
        let name = name?;
        let text = core::str::from_utf8(name).map_err(|_| Malformed::NotUtf8)?;
        if text.is_empty() {
            return Err(Malformed::Empty);
        }
        if into[..i].contains(&text) {
            return Err(Malformed::Twice);
        }
        into[i] = text;
        end += 2 + name.len();
    }
    if end != table.len() {
        return Err(Malformed::Trailing);
    }
    Ok(count)
}

/// Whether `bytes` from `at` lie inside the block's reservation and hold a
/// table's two counts.
fn inside(at: u64, bytes: u64) -> bool {
    bytes >= TABLE_HEADER as u64 && at >= AT && at.checked_add(bytes).is_some_and(|end| end <= AT + MAX_BLOCK as u64)
}

/// This process's table where it lies: empty for a block that names none.
fn own() -> Result<&'static [u8], Malformed> {
    // SAFETY: the block's first page is mapped at `AT` ([`args`]).
    let at = unsafe { (AT as *const u64).read() };
    if at == 0 {
        return Ok(&[]);
    }
    if !inside(at, TABLE_HEADER as u64) {
        return Err(Malformed::Outside);
    }
    // SAFETY: inside the reservation, which a parent that wrote this address
    // mapped through the table's end; one that did not is a fault there, the
    // child's own. Nothing writes the table, so the borrow lasts.
    let bytes = u64::from(unsafe { (at as *const u32).read_unaligned() });
    if !inside(at, bytes) {
        return Err(Malformed::Outside);
    }
    Ok(unsafe { core::slice::from_raw_parts(at as *const u8, bytes as usize) })
}

/// The names this process's parent gave its endowments, in order: how many.
pub fn endowed(into: &mut [&'static str; MAX_SPAWN_ENDOWMENTS]) -> Result<usize, Malformed> {
    match own()? {
        [] => Ok(0),
        table => names(table, into),
    }
}

/// Which endowment this process's table calls `name`, by its bytes alone:
/// for a caller that runs before the image is relocated or after the table
/// was found malformed, where no checked name exists. `None` for a table
/// that names none or is no table up to that name.
pub(crate) fn early(name: &[u8]) -> Option<usize> {
    unchecked_position(own().ok()?, name)
}

/// [`early`]'s walk over `table`.
fn unchecked_position(table: &[u8], name: &[u8]) -> Option<usize> {
    entries(table).take(MAX_SPAWN_ENDOWMENTS).position(|entry| entry == Ok(name))
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

    /// The names a table holds, or why it is none.
    fn read(table: &[u8]) -> Result<Vec<alloc::string::String>, Malformed> {
        let mut into = [""; MAX_SPAWN_ENDOWMENTS];
        names(table, &mut into).map(|count| into[..count].iter().map(|name| (*name).into()).collect())
    }

    /// The names' address, then the count, then each run's pointers in order
    /// and a null after each, every pointer the address its string lies at
    /// once the block is at `AT`.
    #[test]
    fn a_block_s_pointers_name_its_strings_in_order() {
        let block = bytes(b"/bin/echo\0-n\0hello world", b"HOME=/home/toy\0PWD=/\0", &[]).unwrap();
        assert_eq!(u64::from_ne_bytes(block[..8].try_into().unwrap()), 0);
        assert_eq!(u64::from_ne_bytes(block[8..16].try_into().unwrap()), 3);
        let words: Vec<_> = (2..9).map(|word| string(&block, word)).collect();
        assert_eq!(
            words,
            [Some(&b"/bin/echo"[..]), Some(b"-n"), Some(b"hello world"), None, Some(b"HOME=/home/toy"), Some(b"PWD=/"), None]
        );
        assert_eq!(block.len(), 9 * 8 + 10 + 3 + 12 + 15 + 6);
    }

    /// Nothing said is no table, a count of zero and two nulls: what a page
    /// of zeros reads as.
    #[test]
    fn a_block_that_says_nothing_is_zeros() {
        assert_eq!(bytes(b"", b"\0\0", &[]).unwrap(), [0u8; 32]);
    }

    /// A block ends below the program with a page to spare, and one byte more is refused.
    #[test]
    fn a_block_past_its_reservation_is_refused() {
        let fits = alloc::vec![b'a'; MAX_BLOCK - 5 * 8 - 1];
        assert_eq!(bytes(&fits, b"", &[]).map(|block| block.len()), Some(MAX_BLOCK));
        let mut past = fits;
        past.push(b'a');
        assert_eq!(bytes(&past, b"", &[]), None);
        // The names count against the same reservation.
        let fits = alloc::vec![b'a'; MAX_BLOCK - 5 * 8 - 1 - TABLE_HEADER - 2 - 1];
        assert_eq!(bytes(&fits, b"", &["n"]).map(|block| block.len()), Some(MAX_BLOCK));
        assert_eq!(bytes(&fits, b"", &["nn"]), None);
    }

    /// A block's first word is where its names lie once it is at `AT`, after
    /// the strings, and the names read back in the order written, whatever
    /// bytes a name holds.
    #[test]
    fn a_block_s_names_read_back_in_the_order_endowed() {
        let given = ["fd:0", "svc:fs:/home/toy", "dev:pci:0c:03:30", "na\u{ef}ve name"];
        let block = bytes(b"prog", b"", &given).unwrap();
        let at = (u64::from_ne_bytes(block[..8].try_into().unwrap()) - AT) as usize;
        assert_eq!(at, 5 * 8 + 5);
        assert_eq!(read(&block[at..]), Ok(given.map(Into::into).to_vec()));
        assert_eq!(string(&block, 2), Some(&b"prog"[..]));
    }

    /// As many names as a spawn endows are a table, and one more is none.
    #[test]
    fn a_table_holds_as_many_names_as_a_spawn_endows() {
        let names: Vec<alloc::string::String> = (0..=MAX_SPAWN_ENDOWMENTS).map(|i| alloc::format!("n{i}")).collect();
        let names: Vec<&str> = names.iter().map(|name| name.as_str()).collect();
        let most = &names[..MAX_SPAWN_ENDOWMENTS];
        assert_eq!(read(&table(most).unwrap()).map(|read| read.len()), Ok(MAX_SPAWN_ENDOWMENTS));
        assert_eq!(table(&names), None);
        let long = "n".repeat(usize::from(u16::MAX) + 1);
        assert_eq!(table(&[&long]), None);
    }

    /// Every way bytes are no table is refused by its own reason, and none
    /// reads past them.
    #[test]
    fn a_malformed_table_is_refused_by_its_reason() {
        let good = table(&["fd:1", "svc:net"]).unwrap();
        assert_eq!(read(&good), Ok(alloc::vec!["fd:1".into(), "svc:net".into()]));
        let with = |change: &dyn Fn(&mut Vec<u8>)| {
            let mut table = good.clone();
            change(&mut table);
            read(&table)
        };
        let count = |n: u32| move |t: &mut Vec<u8>| t[4..8].copy_from_slice(&n.to_ne_bytes());
        let length = |n: u32| move |t: &mut Vec<u8>| t[..4].copy_from_slice(&n.to_ne_bytes());

        assert_eq!(read(&[]), Err(Malformed::Length));
        assert_eq!(read(&good[..7]), Err(Malformed::Length));
        assert_eq!(with(&length(good.len() as u32 + 1)), Err(Malformed::Length));
        assert_eq!(with(&length(good.len() as u32 - 1)), Err(Malformed::Length));
        assert_eq!(with(&|t| t.truncate(t.len() - 1)), Err(Malformed::Length));
        assert_eq!(with(&count(MAX_SPAWN_ENDOWMENTS as u32 + 1)), Err(Malformed::Count));
        assert_eq!(with(&count(u32::MAX)), Err(Malformed::Count));
        // A third name where the table ends, and a second whose length runs past it.
        assert_eq!(with(&count(3)), Err(Malformed::Truncated));
        assert_eq!(with(&|t| t[14..16].copy_from_slice(&u16::MAX.to_ne_bytes())), Err(Malformed::Truncated));
        // One name counted where two lie.
        assert_eq!(with(&count(1)), Err(Malformed::Trailing));
        assert_eq!(with(&count(0)), Err(Malformed::Trailing));
        assert_eq!(with(&|t| t[10] = 0xff), Err(Malformed::NotUtf8));
        assert_eq!(read(&table(&["fd:1", ""]).unwrap()), Err(Malformed::Empty));
        assert_eq!(read(&table(&["fd:1", "svc:net", "fd:1"]).unwrap()), Err(Malformed::Twice));
        // The table of no names is its two counts.
        assert_eq!(read(&table(&[]).unwrap()), Ok(Vec::new()));
    }

    /// The unchecked walk finds a name by its bytes up to the first entry
    /// that is none, and nothing past it.
    #[test]
    fn the_early_walk_stops_at_the_first_entry_that_is_none() {
        let mut table = table(&["fd:0", "fd:1", "fd:2"]).unwrap();
        assert_eq!(unchecked_position(&table, b"fd:2"), Some(2));
        assert_eq!(unchecked_position(&table, b"fd:3"), None);
        // The second name's length runs past the table: the first is still found.
        table[14..16].copy_from_slice(&u16::MAX.to_ne_bytes());
        assert_eq!(unchecked_position(&table, b"fd:0"), Some(0));
        assert_eq!(unchecked_position(&table, b"fd:2"), None);
        // A count of every `u32` walks no further than a spawn endows.
        table[4..8].copy_from_slice(&u32::MAX.to_ne_bytes());
        assert_eq!(unchecked_position(&table, b"fd:2"), None);
        assert_eq!(unchecked_position(&[], b"fd:2"), None);
    }

    /// A table lies inside the reservation whole, or is refused before a byte
    /// of it is read.
    #[test]
    fn a_table_outside_the_reservation_is_refused() {
        let end = AT + MAX_BLOCK as u64;
        assert!(inside(AT, 8));
        assert!(inside(end - 8, 8));
        assert!(!inside(end - 7, 8));
        assert!(!inside(AT - 1, 8));
        assert!(!inside(AT, 7));
        assert!(!inside(AT, MAX_BLOCK as u64 + 1));
        assert!(!inside(u64::MAX - 3, 8));
    }
}
