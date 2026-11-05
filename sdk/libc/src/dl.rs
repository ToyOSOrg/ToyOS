//! The dynamic loader: `dlopen` and its kin, and the libraries an executable
//! names (`DT_NEEDED`). A library is read wherever its path is served
//! (`toyos::files::read_image`), laid out in memory of this process's own,
//! relocated ([`dlimage`]), given its protections and published in the
//! process's one module list (`toyos::tls`), which every copy of std and libc
//! in the process reads. Its constructors run once it is published, with the
//! list let go, so one may load another.
//!
//! A library is known by the absolute path it was loaded from: a second load
//! of that name answers the module the first published, and nothing leaves
//! the list, so `dlclose` releases nothing.

use alloc::string::String;
use alloc::vec::Vec;
use core::cell::Cell;
use core::ptr;

use toyos::process::ImageRefused;
use toyos::tls::{self, Loading, Module};
use toyos_abi::syscall::{self, SyscallError, VmAt, VmProt};

use crate::dlimage::{self, Mapped, Pass, Plan, Scope, Tables, PAGE};

/// Where a library a `DT_NEEDED` names by file name alone is looked for.
const LIBRARY_DIR: &str = "/system/lib/";

/// The bytes of an image's header a plan is read from: the page its first
/// `PT_LOAD` maps.
const HEADER: usize = PAGE as usize;

/// Why a library was not loaded.
enum Refused {
    Image(ImageRefused<toyos::files::Error>),
    Memory(SyscallError),
    Elf(&'static str),
}

impl From<&'static str> for Refused {
    fn from(why: &'static str) -> Self {
        Refused::Elf(why)
    }
}

impl Refused {
    /// What `dlerror` says of it.
    fn text(&self) -> &'static str {
        match self {
            Refused::Image(_) => "dlopen: the library could not be read",
            Refused::Memory(_) => "dlopen: no memory for the library",
            Refused::Elf(why) => why,
        }
    }

    fn errno(self) -> i32 {
        match self {
            Refused::Image(refused) => crate::errcode::of_image(refused),
            Refused::Memory(e) => crate::errcode::of(e),
            Refused::Elf(_) => crate::errcode::ENOEXEC,
        }
    }
}

/// A published module and its tables, which a new image's symbols bind to.
struct Known {
    module: &'static Module,
    tables: Tables<'static>,
}

/// Every module published, executable first: what a load binds against.
struct Published(Vec<Known>);

impl Published {
    /// What the list `_loading` holds: a module whose tables cannot be read
    /// binds nothing.
    fn of(_loading: &Loading) -> Self {
        Published(tls::modules().filter_map(known).collect())
    }
}

/// `module`'s tables, `None` for one with none to read.
fn known(module: &'static Module) -> Option<Known> {
    let base = module.image.base;
    // SAFETY: every published module's first page is its header, mapped for
    // the process's life.
    let header = unsafe { core::slice::from_raw_parts(base as *const u8, HEADER) };
    let plan = dlimage::plan(header).ok()?;
    // SAFETY: a published module stays mapped as its loader laid it out, and
    // nothing writes its tables.
    let mapped = unsafe { Mapped::new(base as *mut u8, &plan) };
    let tables = mapped.tables(false).ok()??;
    Some(Known { module, tables })
}

impl Scope for Published {
    fn address(&self, name: &str) -> Option<u64> {
        self.0.iter().find_map(|k| k.tables.address(name, k.module.image.base as u64))
    }

    fn tls(&self, name: &str, addend: i64) -> Option<Result<(usize, u64), toyos_elf::RelocError>> {
        self.0.iter().filter(|k| k.module.tls.memsz != 0).find_map(|k| Some(k.tables.tls(name, addend)?.map(|at| (k.module.id(), at))))
    }
}

/// Load the library named `name` and every one it names, depth first, into
/// the list `loading` holds: the module, and each newly loaded one's
/// constructors in `inits`, a library's after those of the ones it names.
fn open(loading: &mut Loading, published: &mut Published, name: &str, inits: &mut Vec<(usize, usize)>) -> Result<&'static Module, Refused> {
    if let Some(module) = loading.find(name.as_bytes()) {
        return Ok(module);
    }
    let file = toyos::files::read_image(name.as_bytes()).map_err(Refused::Image)?;
    let bytes = file.bytes();
    let plan = dlimage::plan(&bytes[..bytes.len().min(HEADER)])?;
    // Committed whole while it is laid out and relocated; each page is given
    // its own protection once nothing more is written.
    // SAFETY: placed anywhere, so it replaces nothing.
    let base = unsafe { syscall::vm_reserve(plan.span, VmProt::ReadWrite, VmAt::ANYWHERE) }.map_err(Refused::Memory)?;
    let reservation = Reservation { at: base, len: plan.span };
    // SAFETY: the fresh, zeroed reservation, this function's alone.
    plan.lay(bytes, unsafe { core::slice::from_raw_parts_mut(base, plan.span) })?;
    // SAFETY: laid out whole and writable, and reached by nothing else until published.
    let mapped = unsafe { Mapped::new(base, &plan) };
    let tables = mapped.tables(true)?.ok_or("ELF: a shared object with no PT_DYNAMIC")?;
    for needed in tables.needed() {
        let path = if needed.contains('/') { String::from(needed) } else { [LIBRARY_DIR, needed].concat() };
        open(loading, published, &path, inits)?;
    }
    let id = loading.next_id();
    dlimage::relocate(&mapped, &tables, id, published, Pass::Library)?;
    protect(&plan, base)?;
    let start = base as usize;
    let tls = plan.tls().map_or(tls::Tls::default(), |segment| tls::Tls {
        template: start + segment.template().start().get() as usize,
        filesz: segment.template().len() as usize,
        memsz: segment.memsz() as usize,
        align: segment.align() as usize,
    });
    let (phdr, phnum) = plan.phdrs();
    let image = tls::Image {
        base: start,
        start,
        end: start + plan.span,
        phdr: start + phdr as usize,
        phnum,
        eh_frame_hdr: plan.eh_frame_hdr().map_or(0, |at| start + at as usize),
    };
    let name = alloc::ffi::CString::new(name).map_err(|_| "dlopen: a library path holds a NUL")?;
    let name: &'static core::ffi::CStr = alloc::boxed::Box::leak(name.into_boxed_c_str());
    core::mem::forget(reservation);
    let module = loading.add(Module::new(name, image, tls));
    if let Some((at, count)) = tables.init_array {
        inits.push((start + at as usize, count));
    }
    published.0.push(Known { module, tables });
    Ok(module)
}

/// A reservation released unless it is published.
struct Reservation {
    at: *mut u8,
    len: usize,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        // SAFETY: a reservation nothing was published over.
        let _ = unsafe { syscall::vm_release(self.at, self.len) };
    }
}

/// Give each of `plan`'s segments at `base` its pages' protection, the pages
/// between them none, and `PT_GNU_RELRO`'s pages read-only: nothing writes
/// what its relocations wrote, and no page is both writable and executable.
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
/// before anything that could name such a symbol. An executable that names
/// none costs a look at its dynamic section; one whose libraries cannot be
/// had exits 127 with a line on its stderr, as `toyos::relocate` does.
///
/// # Safety
/// `header` is the running executable's ELF header, and its thread-local
/// storage is laid out (`toyos::tls::start`).
#[no_mangle]
pub unsafe extern "C" fn __toyos_load_needed(header: *const u8) {
    // SAFETY: the caller's; the page the header begins is mapped.
    let page = unsafe { core::slice::from_raw_parts(header, HEADER) };
    let Ok(plan) = dlimage::plan(page) else { return };
    // SAFETY: the executable as the kernel mapped it, relocated by itself,
    // whose writable window only this writes until it returns.
    let mapped = unsafe { Mapped::new(header.cast_mut(), &plan) };
    let Ok(Some(tables)) = mapped.tables(false) else { return };
    if tables.needed().next().is_none() {
        return;
    }
    let mut inits = Vec::new();
    let bound = (|| -> Result<(), Refused> {
        let mut loading = tls::loading();
        let mut published = Published::of(&loading);
        let tables = mapped.tables(true)?.ok_or("ELF: no PT_DYNAMIC")?;
        for needed in tables.needed() {
            let path = if needed.contains('/') { String::from(needed) } else { [LIBRARY_DIR, needed].concat() };
            open(&mut loading, &mut published, &path, &mut inits)?;
        }
        dlimage::relocate(&mapped, &tables, 1, &published, Pass::Executable)?;
        Ok(())
    })();
    if bound.is_err() {
        let line = b"this program's libraries could not be loaded: it cannot start\n";
        // SAFETY: a buffer of `line.len()` bytes.
        let _ = unsafe { crate::posix_io::write(2, line.as_ptr(), line.len()) };
        syscall::exit(127);
    }
    // SAFETY: each a module just published.
    unsafe { construct(&inits) };
}

/// A C handle is a module's id: never null, which is `dlopen`'s failure.
fn module_of(handle: *mut u8) -> Option<&'static Module> {
    tls::modules().find(|m| m.id() == handle as usize)
}

#[no_mangle]
pub unsafe extern "C" fn dlopen(path: *const u8, _flags: i32) -> *mut u8 {
    if path.is_null() {
        return ptr::null_mut();
    }
    let Ok(path) = core::str::from_utf8(crate::posix_io::c_str_to_bytes(path)) else {
        crate::errno::set(crate::errcode::ENOENT);
        return ptr::null_mut();
    };
    let mut cwd = alloc::vec![0u8; syscall::MAX_PATH];
    let n = toyos::files::getcwd(&mut cwd);
    let name = toyos::files::absolute(&String::from_utf8_lossy(&cwd[..n.min(cwd.len())]), path);
    let mut inits = Vec::new();
    let opened = {
        let mut loading = tls::loading();
        let mut published = Published::of(&loading);
        open(&mut loading, &mut published, &name, &mut inits)
    };
    match opened {
        Ok(module) => {
            // SAFETY: each a module just published.
            unsafe { construct(&inits) };
            module.id() as *mut u8
        }
        Err(refused) => {
            ERROR.set(Some(refused.text()));
            crate::errno::set(refused.errno());
            ptr::null_mut()
        }
    }
}

/// The symbol `symbol` names: in the module `handle` names, any symbol it
/// defines; with `RTLD_DEFAULT` (null), the first module's that exports it.
#[no_mangle]
pub unsafe extern "C" fn dlsym(handle: *mut u8, symbol: *const u8) -> *mut u8 {
    if symbol.is_null() {
        return ptr::null_mut();
    }
    let Ok(name) = core::str::from_utf8(crate::posix_io::c_str_to_bytes(symbol)) else {
        return ptr::null_mut();
    };
    let found = if handle.is_null() {
        tls::modules().filter_map(known).find_map(|k| k.tables.address(name, k.module.image.base as u64))
    } else {
        module_of(handle).and_then(known).and_then(|k| k.tables.defined(name, k.module.image.base as u64))
    };
    found.map_or_else(
        || {
            ERROR.set(Some("dlsym: no such symbol"));
            ptr::null_mut()
        },
        |at| at as *mut u8,
    )
}

/// Nothing leaves the module list: `0` for a handle `dlopen` answered.
#[no_mangle]
pub unsafe extern "C" fn dlclose(handle: *mut u8) -> i32 {
    if module_of(handle).is_some() { 0 } else { -1 }
}

/// The calling thread's last refusal by `dlopen` or `dlsym` that `dlerror` has not said yet.
#[thread_local]
static ERROR: Cell<Option<&'static str>> = Cell::new(None);

/// Where `dlerror` writes it, NUL-terminated: valid until the thread's next `dlerror`.
#[thread_local]
static mut TEXT: [u8; 128] = [0; 128];

/// The last refusal not said yet, cut to what [`TEXT`] holds; null for none.
#[no_mangle]
pub unsafe extern "C" fn dlerror() -> *const u8 {
    let Some(text) = ERROR.take() else { return ptr::null() };
    // SAFETY: the calling thread's own buffer.
    let buf = unsafe { &mut *(&raw mut TEXT) };
    let len = text.len().min(buf.len() - 1);
    buf[..len].copy_from_slice(&text.as_bytes()[..len]);
    buf[len] = 0;
    buf.as_ptr()
}
