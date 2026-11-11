//! The dynamic loader: a library a program opens ([`open`]), and those its
//! executable names (`DT_NEEDED`, [`start`]), laid out in memory of the
//! process's own, relocated ([`image`]), given their protections and
//! published in the process's one module list ([`crate::tls`]), which every
//! copy of std and libc in the process reads. A segment that is the pages of
//! a file nobody can write is those pages, shared with every process that
//! loads the library, and not a copy.
//!
//! **A load is one set**: the libraries named and every library they name that
//! is not yet published, found breadth first, so a cycle of names is one set
//! and loads once. Each is relocated against the published modules and then
//! the set's, in load order. Nothing of the set is published until all of it
//! is relocated and protected, and a refusal releases all of it. Constructors
//! run once the set is published and the list let go, so one may load
//! another: a library's after those of the libraries it names.
//!
//! A library is known by the absolute path it was loaded from: a second load
//! of that name answers the module the first published, and nothing leaves
//! the list.

mod image;
#[cfg(test)]
mod tests;

use alloc::collections::VecDeque;
use alloc::ffi::CString;
use alloc::string::String;
use alloc::vec::Vec;
use core::ffi::CStr;
use core::fmt;

use toyos_abi::syscall::{self, SyscallError, VmAt, VmProt};

use crate::AsHandle;
use crate::elf::RelocError;

use crate::files;
use crate::process::ImageRefused;
use crate::tls::{self, Loading, Module};
use image::{FilePages, Mapped, Pass, Plan, Scope, Tables, PAGE};

/// Where a library named by file name alone is looked for.
const LIBRARY_DIR: &str = "/system/lib/";

/// The bytes of an image's header a plan is read from: the page its first
/// `PT_LOAD` maps.
const HEADER: usize = PAGE as usize;

/// Why a load was refused, as `dlerror` says it.
#[derive(Debug)]
pub enum Refused {
    /// The library at this path could not be read.
    Read(String, ImageRefused<files::Error>),
    /// No memory to lay a library out in.
    Memory(SyscallError),
    /// An image [`crate::elf`] or the loader's own rules refuse.
    Elf(&'static str),
    /// A relocation the loader does not apply.
    Relocation(RelocError),
    /// A symbol an image binds to that no module defines, and that it
    /// neither names weakly nor calls lazily.
    Undefined(String),
}

impl From<&'static str> for Refused {
    fn from(why: &'static str) -> Self {
        Refused::Elf(why)
    }
}

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Refused::Read(path, why) => write!(f, "{path}: could not be read: {why:?}"),
            Refused::Memory(e) => write!(f, "no memory for a library: {e}"),
            Refused::Elf(why) => f.write_str(why),
            Refused::Relocation(e) => write!(f, "{e}"),
            Refused::Undefined(name) => write!(f, "symbol `{name}` is defined by no loaded module"),
        }
    }
}

/// What a lazily bound call nothing defines lands in: the call fails, as a
/// lazy loader's first call to it would, with a line on stderr and 127.
extern "C" fn undefined_function() -> ! {
    let line = b"a library called a function no loaded module defines\n";
    if let Some(stderr) = crate::relocate::stderr() {
        let _ = syscall::write(stderr, line);
    }
    syscall::exit(127)
}

/// A module whose definitions a load binds to: published, or in the set.
struct Bound<'m> {
    id: usize,
    base: u64,
    tables: Tables<'m>,
    tls: bool,
}

/// Every module a load binds against, the executable first, in load order.
struct Global<'m>(Vec<Bound<'m>>);

impl Scope for Global<'_> {
    fn address(&self, name: &str) -> Option<u64> {
        self.0.iter().find_map(|m| m.tables.address(name, m.base))
    }

    fn tls(&self, name: &str, addend: i64) -> Option<Result<(usize, u64), RelocError>> {
        self.0.iter().filter(|m| m.tls).find_map(|m| Some(m.tables.tls(name, addend)?.map(|at| (m.id, at))))
    }
}

/// `module`'s tables, `None` for one with none to read.
fn bound(module: &'static Module) -> Option<Bound<'static>> {
    let base = module.image.base;
    // SAFETY: every published module's first page is its header, mapped for
    // the process's life.
    let header = unsafe { core::slice::from_raw_parts(base as *const u8, HEADER) };
    let plan = image::plan(header).ok()?;
    // SAFETY: a published module stays mapped as its loader laid it out, and
    // nothing writes its tables.
    let mapped = unsafe { Mapped::new(base as *mut u8, &plan) };
    let tables = mapped.tables(false).ok()??;
    Some(Bound { id: module.id(), base: base as u64, tables, tls: module.tls.memsz != 0 })
}

/// A library of a set, by the path it is loaded from, and the paths it
/// names, in its order.
struct Node<T> {
    name: String,
    needed: Vec<String>,
    laid: T,
}

/// A library laid out and not yet published.
struct Laid {
    plan: Plan,
    memory: Reservation,
    tables: Tables<'static>,
}

/// The set a load lays out with `lay`: `roots` and every library they name,
/// breadth first, each once, but those already `published`.
fn discover<T>(
    roots: impl IntoIterator<Item = String>,
    published: impl Fn(&str) -> bool,
    mut lay: impl FnMut(&str) -> Result<(T, Vec<String>), Refused>,
) -> Result<Vec<Node<T>>, Refused> {
    let mut set: Vec<Node<T>> = Vec::new();
    let mut queue: VecDeque<String> = roots.into_iter().collect();
    while let Some(name) = queue.pop_front() {
        if published(&name) || set.iter().any(|node| node.name == name) {
            continue;
        }
        let (laid, needed) = lay(&name)?;
        queue.extend(needed.iter().cloned());
        set.push(Node { name, needed, laid });
    }
    Ok(set)
}

/// The order `set`'s constructors run in, by index: each library after the
/// set's libraries it names, depth first in the order it names them, each
/// once however many name it.
fn order<T>(set: &[Node<T>]) -> Vec<usize> {
    let index = |name: &str| set.iter().position(|node| node.name == name);
    let mut seen = alloc::vec![false; set.len()];
    let mut order = Vec::with_capacity(set.len());
    // Each frame a library and how many of its names were visited.
    let mut stack: Vec<(usize, usize)> = Vec::new();
    for root in 0..set.len() {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        stack.push((root, 0));
        while let Some(&(at, next)) = stack.last() {
            match set[at].needed.get(next) {
                Some(name) => {
                    if let Some(top) = stack.last_mut() {
                        top.1 += 1;
                    }
                    if let Some(dep) = index(name).filter(|&dep| !seen[dep]) {
                        seen[dep] = true;
                        stack.push((dep, 0));
                    }
                }
                None => {
                    stack.pop();
                    order.push(at);
                }
            }
        }
    }
    order
}

/// The path a `DT_NEEDED` name is loaded from.
fn path_of(needed: &str) -> Result<String, Refused> {
    match needed {
        "" => Err(Refused::Elf("ELF: a DT_NEEDED names no library")),
        _ if needed.contains('/') => Ok(String::from(needed)),
        _ => Ok([LIBRARY_DIR, needed].concat()),
    }
}

/// Read the library at `name` and lay it out in memory of its own, its
/// tables checked: it, and the paths it names.
fn lay(name: &str) -> Result<(Laid, Vec<String>), Refused> {
    if name.contains('\0') {
        return Err(Refused::Elf("dlopen: a library path holds a NUL"));
    }
    let file = files::read_image(name.as_bytes()).map_err(|why| Refused::Read(String::from(name), why))?;
    let bytes = file.bytes();
    let plan = image::plan(&bytes[..bytes.len().min(HEADER)])?;
    plan.library()?;
    // Committed whole while it is laid out and relocated; each page is given
    // its own protection once nothing more is written.
    // SAFETY: placed anywhere, so it replaces nothing.
    let at = unsafe { syscall::vm_reserve(plan.span, VmProt::ReadWrite, VmAt::ANYWHERE) }.map_err(Refused::Memory)?;
    let memory = Reservation { at, len: plan.span, kept: false };
    let shared = file.as_immutable();
    let pages = if shared.is_some() { FilePages::Mapped } else { FilePages::Copied };
    // SAFETY: the fresh, zeroed reservation, this function's alone.
    plan.lay(bytes, unsafe { core::slice::from_raw_parts_mut(at, plan.span) }, pages)?;
    if let Some(image) = shared {
        for (lo, hi, offset, flags) in plan.file_pages(bytes.len() as u64) {
            let prot = if flags.executable() { VmProt::ReadExec } else { VmProt::Read };
            // SAFETY: whole pages of the reservation, which nothing reaches yet.
            unsafe { syscall::vm_map(image.as_handle().raw(), offset, (hi - lo) as usize, prot, VmAt::Fixed(at.add(lo as usize))) }
                .map_err(Refused::Memory)?;
        }
    }
    // SAFETY: laid out whole and writable, reached by nothing else until
    // published, and released only after every view of it is gone.
    let mapped = unsafe { Mapped::new(at, &plan) };
    let tables = mapped.tables(true)?.ok_or("ELF: a library with no PT_DYNAMIC")?;
    let needed = tables.needed().map(path_of).collect::<Result<_, _>>()?;
    Ok((Laid { plan, memory, tables }, needed))
}

/// A library's memory, released unless it is [kept](Self::keep).
struct Reservation {
    at: *mut u8,
    len: usize,
    kept: bool,
}

impl Reservation {
    /// Keep it for the process's life: its module is published.
    fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.kept {
            // SAFETY: a reservation nothing was published over.
            let _ = unsafe { syscall::vm_release(self.at, self.len) };
        }
    }
}

/// A set published: each library's constructors to run, `(address, count)`,
/// a library's after those of the libraries it names.
struct Published {
    inits: Vec<(usize, usize)>,
}

/// Load `roots` and every library they name that is not yet published, into
/// the list `loading` holds. `executable`, the running executable's image and
/// tables, is relocated against the set before it is published.
fn load(loading: &mut Loading, roots: impl IntoIterator<Item = String>, executable: Option<(&Mapped<'_, '_>, &Tables<'_>)>) -> Result<Published, Refused> {
    let set = discover(roots, |name| loading.find(name.as_bytes()).is_some(), lay)?;
    let first = loading.next_id();
    let mut global = Global(tls::modules().filter_map(bound).collect());
    global.0.extend(set.iter().enumerate().map(|(i, node)| Bound {
        id: first + i,
        base: node.laid.memory.at as u64,
        tables: node.laid.tables.clone(),
        tls: node.laid.plan.tls().is_some(),
    }));
    for (i, node) in set.iter().enumerate() {
        // SAFETY: `lay`'s, and only this writes it until it is published.
        let mapped = unsafe { Mapped::new(node.laid.memory.at, &node.laid.plan) };
        image::relocate(&mapped, &node.laid.tables, first + i, &global, Pass::Library)?;
    }
    if let Some((mapped, tables)) = executable {
        let block = tls::block(1);
        let filesz = tls::modules().next().map_or(0, |exe| exe.tls.filesz);
        // SAFETY: the main thread's block of the executable, `filesz` bytes of
        // it the template's, and the main thread is the only one.
        let block = unsafe { core::slice::from_raw_parts_mut(block, filesz) };
        image::relocate(mapped, tables, 1, &global, Pass::Executable { block })?;
    }
    for node in &set {
        protect(&node.laid.plan, node.laid.memory.at)?;
    }
    drop(global);
    let inits = order(&set)
        .into_iter()
        .filter_map(|i| {
            let laid = &set[i].laid;
            laid.tables.init_array.map(|(offset, count)| (laid.memory.at as usize + offset as usize, count))
        })
        .collect();
    for (i, node) in set.into_iter().enumerate() {
        let start = node.laid.memory.at as usize;
        let laid = node.laid;
        let module = loading.add(Module::new(leak(&node.name), image_of(&laid.plan, start), tls_of(&laid.plan, start)));
        assert_eq!(module.id(), first + i, "dl: a set's ids are taken in order while the list is held");
        laid.memory.keep();
    }
    Ok(Published { inits })
}

/// `name` for the module list, which keeps it for the process's life.
fn leak(name: &str) -> &'static CStr {
    let name = CString::new(name).expect("dl: `lay` refused a path holding a NUL");
    alloc::boxed::Box::leak(name.into_boxed_c_str())
}

fn image_of(plan: &Plan, start: usize) -> tls::Image {
    let (phdr, phnum) = plan.phdrs();
    tls::Image {
        base: start,
        start,
        end: start + plan.span,
        phdr: start + phdr as usize,
        phnum,
        eh_frame_hdr: plan.eh_frame_hdr().map_or(0, |at| start + at as usize),
    }
}

fn tls_of(plan: &Plan, start: usize) -> tls::Tls {
    plan.tls().map_or(tls::Tls::default(), |segment| tls::Tls {
        template: start + segment.template().start().get() as usize,
        filesz: segment.template().len() as usize,
        memsz: segment.memsz() as usize,
        align: segment.align() as usize,
    })
}

/// Give each of `plan`'s segments at `base` its pages' protection, the pages
/// between them none, and `PT_GNU_RELRO`'s pages read-only: nothing writes
/// what its relocations wrote. No segment is both writable and executable
/// ([`Plan::library`]).
fn protect(plan: &Plan, base: *mut u8) -> Result<(), Refused> {
    let mut segments: Vec<_> = plan.segments().collect();
    segments.sort_unstable_by_key(|&(lo, _, _)| lo);
    let mut covered = 0;
    let give = |lo: u64, hi: u64, prot: VmProt| {
        if lo >= hi {
            return Ok(());
        }
        // SAFETY: whole pages of the reservation `base` begins.
        unsafe { syscall::vm_protect(base.add(lo as usize), (hi - lo) as usize, prot) }.map_err(Refused::Memory)
    };
    for (lo, hi, flags) in segments {
        give(covered, lo, VmProt::None)?;
        let prot = if flags.executable() {
            VmProt::ReadExec
        } else if flags.writable() {
            VmProt::ReadWrite
        } else {
            VmProt::Read
        };
        give(lo, hi, prot)?;
        covered = hi;
    }
    give(covered, plan.span as u64, VmProt::None)?;
    if let Some((lo, hi)) = plan.relro() {
        give(lo, hi, VmProt::Read)?;
    }
    Ok(())
}

/// Call each `(address, count)` `.init_array`'s constructors in order.
///
/// # Safety
/// Each names a published module's `.init_array`.
unsafe fn construct(inits: &[(usize, usize)]) {
    for &(at, count) in inits {
        // SAFETY: the caller's; the array is `count` pointers inside the image.
        let entries = unsafe { core::slice::from_raw_parts(at as *const usize, count) };
        for &entry in entries {
            // The psABI's `0` and `-1` are no constructor.
            if entry != 0 && entry != usize::MAX {
                // SAFETY: a relocated `.init_array` entry is a constructor.
                let f: extern "C" fn() = unsafe { core::mem::transmute(entry) };
                f();
            }
        }
    }
}

/// Load the libraries the executable at `header` names and bind its symbols
/// to them, then run their constructors: std's and libc's start call this
/// once its thread-local storage is laid out, before anything that could
/// name such a symbol. One whose libraries cannot be had exits 127 with a
/// line on its stderr naming why, as [`crate::relocate`] does.
///
/// # Safety
/// `header` is the running executable's ELF header, its thread-local storage
/// is laid out ([`tls::start`]) and this is its main thread, the only one.
pub unsafe fn start(header: *const u8) {
    if !crate::relocate::names_a_library() {
        return;
    }
    // SAFETY: the caller's.
    match unsafe { load_needed(header) } {
        // SAFETY: each a module just published.
        Ok(published) => unsafe { construct(&published.inits) },
        Err(why) => {
            let line = alloc::format!("this program's libraries could not be loaded: {why}\n");
            if let Some(stderr) = crate::relocate::stderr() {
                let _ = syscall::write(stderr, line.as_bytes());
            }
            syscall::exit(127)
        }
    }
}

/// # Safety
/// [`start`]'s.
unsafe fn load_needed(header: *const u8) -> Result<Published, Refused> {
    // SAFETY: the caller's; the page the header begins is mapped.
    let page = unsafe { core::slice::from_raw_parts(header, HEADER) };
    let plan = image::plan(page)?;
    // SAFETY: the executable as the kernel mapped it, relocated by itself,
    // whose writable window only the load writes until it returns.
    let mapped = unsafe { Mapped::new(header.cast_mut(), &plan) };
    let tables = mapped.tables(true)?.ok_or("ELF: an executable that names libraries with no PT_DYNAMIC")?;
    let needed: Vec<String> = tables.needed().map(path_of).collect::<Result<_, _>>()?;
    let mut loading = tls::loading();
    load(&mut loading, needed, Some((&mapped, &tables)))
}

/// Load the library at `path`, relative to the working directory, and every
/// library it names, and run their constructors: the module, which a second
/// load of the same path answers too.
pub fn open(path: &str) -> Result<&'static Module, Refused> {
    let mut cwd = alloc::vec![0u8; syscall::MAX_PATH];
    let n = files::getcwd(&mut cwd);
    let name = files::absolute(&String::from_utf8_lossy(&cwd[..n.min(cwd.len())]), path);
    let (module, published) = {
        let mut loading = tls::loading();
        let published = load(&mut loading, [name.clone()], None)?;
        (loading.find(name.as_bytes()).expect("dl: a load publishes its root"), published)
    };
    // SAFETY: each a module just published.
    unsafe { construct(&published.inits) };
    Ok(module)
}

/// The module whose id is `id`.
pub fn module(id: usize) -> Option<&'static Module> {
    tls::modules().find(|m| m.id() == id)
}

/// The address of the symbol `name`: with a module, any symbol it defines;
/// with none, the first module's that exports it, the executable first. A
/// thread-local has no one address, so it is none.
pub fn symbol(module: Option<&'static Module>, name: &str) -> Option<usize> {
    let found = match module {
        Some(module) => bound(module).and_then(|m| m.tables.defined(name, m.base)),
        None => tls::modules().filter_map(bound).find_map(|m| m.tables.address(name, m.base)),
    };
    found.map(|at| at as usize)
}
