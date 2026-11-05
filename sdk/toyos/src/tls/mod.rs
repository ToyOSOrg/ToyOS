//! Thread-local storage, which a program lays out itself: the kernel points a
//! thread's thread pointer where the thread names (`SYS_SET_THREAD_POINTER`)
//! and knows nothing else of it.
//!
//! **Every image of a process is one [`Module`] in one list**, the executable
//! first and each library in the order it was loaded, and a module's id is its
//! place in the list from 1: the index its `DTPMOD` names and its thread's
//! DTV slot. Nothing leaves the list. Every thread's DTV names the list, so
//! each copy of this crate in a process (std's, libc's, a program's own, a
//! loaded library's std's) finds the one the program's start made.
//!
//! **A thread's area** holds its DTV, the executable's data and its thread
//! control block ([`layout`]); a thread spawned here gets one below its stack,
//! released with it. Another module's block is made the first time the thread
//! asks for it ([`block`], `__tls_get_addr`'s slow path), and goes with
//! [`leave`]. Its DTV grows with the list, in the process's own memory.

pub mod layout;

use core::ffi::CStr;
use core::sync::atomic::{AtomicPtr, AtomicU32, AtomicUsize, Ordering};

use toyos_abi::syscall::{self, SyscallError, VmAt, VmProt, PAGE_SIZE};
use toyos_abi::Tid;

use crate::arch::tcb;
pub use layout::{Area, Variant};

/// A DTV slot whose block has not been made.
pub const UNALLOCATED: usize = usize::MAX;

/// A DTV's header, before its slots: the list, how many slots follow, and the
/// bytes of a reservation of its own, zero for the one inside a thread's area.
#[repr(C)]
struct Header {
    modules: *const Modules,
    capacity: usize,
    owned: usize,
}

/// Where a DTV's slot count is, from its first byte: `__tls_get_addr` bounds a
/// module id by it.
pub const DTV_CAPACITY: usize = core::mem::offset_of!(Header, capacity);
/// Where a DTV's slots start: module `n`'s is word `n - 1` from here.
pub const DTV_SLOTS: usize = core::mem::size_of::<Header>();
/// Slots in the DTV inside a thread's area.
const AREA_SLOTS: usize = 16;
const AREA_DTV: usize = DTV_SLOTS + AREA_SLOTS * 8;

/// What a loaded image is, as everything that walks the list reads it.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Image {
    /// What a virtual address of the file is moved by.
    pub base: usize,
    /// The span its `PT_LOAD`s are mapped over.
    pub start: usize,
    pub end: usize,
    /// Its program header table, as mapped.
    pub phdr: usize,
    pub phnum: usize,
    /// Its `.eh_frame_hdr` as mapped, zero for none.
    pub eh_frame_hdr: usize,
}

/// A module's `PT_TLS`: its template as mapped, the bytes of it that are file
/// bytes, the whole block's bytes and its alignment. `memsz` zero for none.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Tls {
    pub template: usize,
    pub filesz: usize,
    pub memsz: usize,
    pub align: usize,
}

/// One image in the list.
#[repr(C)]
pub struct Module {
    next: AtomicPtr<Module>,
    id: usize,
    name: *const core::ffi::c_char,
    pub image: Image,
    pub tls: Tls,
}

// SAFETY: published once, never written again, and its pointers name memory
// that lives as long as the process.
unsafe impl Sync for Module {}

impl Module {
    /// A module to [`Loading::add`]: `name` is what it is known by, which a
    /// second load of that name finds.
    pub fn new(name: &'static CStr, image: Image, tls: Tls) -> Module {
        Module { next: AtomicPtr::new(core::ptr::null_mut()), id: 0, name: name.as_ptr(), image, tls }
    }

    pub fn id(&self) -> usize {
        self.id
    }

    pub fn name(&self) -> &'static CStr {
        // SAFETY: a `&'static CStr` when it was made.
        unsafe { CStr::from_ptr(self.name) }
    }

    /// Whether `addr` lies in the span this image is mapped over.
    pub fn holds(&self, addr: usize) -> bool {
        (self.image.start..self.image.end).contains(&addr)
    }
}

/// The list, and what a thread's area is.
#[repr(C)]
pub struct Modules {
    /// Held across a whole load, so a name is loaded once and ids are taken in order.
    loading: AtomicU32,
    first: AtomicPtr<Module>,
    last: AtomicPtr<Module>,
    count: AtomicUsize,
    area: Area,
}

/// The list, from the calling thread's DTV; `None` after [`leave`] released a
/// DTV of its own.
fn list() -> Option<&'static Modules> {
    // SAFETY: a DTV's header names the list, which lives for the process.
    unsafe { dtv().as_ref().map(|dtv| &*dtv.modules) }
}

/// The calling thread's DTV. Every function here but [`start`] and [`enter`]
/// is for a thread that has named its thread pointer, which every thread the
/// SDK starts has: on x86-64 the read faults before then.
fn dtv() -> *mut Header {
    // SAFETY: TP addresses the thread's TCB, whose DTV word its startup wrote.
    unsafe { *((tcb::thread_pointer() + tcb::DTV) as *const *mut Header) }
}

/// Every module, the executable first. A module published meanwhile may or may
/// not be visited.
pub fn modules() -> impl Iterator<Item = &'static Module> {
    let mut at = list().map_or(core::ptr::null_mut(), |list| list.first.load(Ordering::Acquire));
    core::iter::from_fn(move || {
        // SAFETY: a published module, never freed.
        let module = unsafe { at.as_ref()? };
        at = module.next.load(Ordering::Acquire);
        Some(module)
    })
}

/// The list held for one load: the only way to add to it.
pub struct Loading(&'static Modules);

impl Loading {
    /// The module known by `name`, if one is.
    pub fn find(&self, name: &[u8]) -> Option<&'static Module> {
        modules().find(|m| m.name().to_bytes() == name)
    }

    /// The id the next module added takes.
    pub fn next_id(&self) -> usize {
        self.0.count.load(Ordering::Relaxed) + 1
    }

    /// Publish `module` under [`next_id`](Self::next_id).
    pub fn add(&mut self, mut module: Module) -> &'static Module {
        module.id = self.next_id();
        let module: &'static Module = alloc::boxed::Box::leak(alloc::boxed::Box::new(module));
        let at = (module as *const Module).cast_mut();
        // SAFETY: the last module published, never freed; the list is held.
        match unsafe { self.0.last.load(Ordering::Relaxed).as_ref() } {
            Some(last) => last.next.store(at, Ordering::Release),
            None => self.0.first.store(at, Ordering::Release),
        }
        self.0.last.store(at, Ordering::Relaxed);
        self.0.count.store(module.id, Ordering::Release);
        module
    }
}

impl Drop for Loading {
    fn drop(&mut self) {
        crate::sync::unlock_word(&self.0.loading);
    }
}

/// The list held for one load.
pub fn loading() -> Loading {
    let list = list().expect("tls: a load on a thread that left");
    crate::sync::lock_word(&list.loading);
    Loading(list)
}

/// The calling thread's id, read off its TCB: what `SYS_SET_THREAD_POINTER`
/// answered its startup.
#[inline]
pub fn current_tid() -> Tid {
    Tid::from_raw(tcb::current_tid())
}

/// What the executable at `header` is, from its own program headers, which the
/// kernel mapped and `toyos::relocate` already trusted.
///
/// # Safety
/// `header` is the running executable's ELF header.
unsafe fn executable(header: *const u8) -> (Image, Tls) {
    // SAFETY: the caller's; every offset below is inside the header or the
    // program header table its segment maps.
    unsafe {
        let word = |at: usize| (header.add(at) as *const u64).read_unaligned() as usize;
        let phoff = word(32);
        let phnum = (header.add(56) as *const u16).read_unaligned() as usize;
        let phdr = header.add(phoff);
        let field = |i: usize, at: usize| (phdr.add(i * 56 + at) as *const u64).read_unaligned() as usize;
        let kind = |i: usize| (phdr.add(i * 56) as *const u32).read_unaligned();
        let loads = (0..phnum).filter(|&i| kind(i) == PT_LOAD);
        let Some(first) = loads.clone().find(|&i| field(i, 8) == 0) else {
            refuse(b"no PT_LOAD maps this program's header: it cannot start\n");
        };
        let base = header as usize - (field(first, 16) & !(PAGE_SIZE - 1));
        let start = loads.clone().map(|i| field(i, 16)).min().unwrap_or(0) & !(PAGE_SIZE - 1);
        let end = loads.map(|i| field(i, 16) + field(i, 40)).max().unwrap_or(0);
        let of = |wanted: u32| (0..phnum).find(|&i| kind(i) == wanted);
        let eh_frame_hdr = of(PT_GNU_EH_FRAME).map_or(0, |i| base + field(i, 16));
        let tls = of(PT_TLS).map_or(Tls::default(), |i| Tls {
            template: base + field(i, 16),
            filesz: field(i, 32),
            memsz: field(i, 40),
            align: field(i, 48),
        });
        (Image { base, start: base + start, end: base + end, phdr: phdr as usize, phnum, eh_frame_hdr }, tls)
    }
}

const PT_LOAD: u32 = 1;
const PT_TLS: u32 = 7;
const PT_GNU_EH_FRAME: u32 = 0x6474_e550;

/// The program's start: the list, with the executable at `header` in it as
/// `name`, and the calling thread's area, its thread pointer named. First
/// thing after relocation, before anything reads a thread-local; exits the
/// process when its memory cannot be had, since nothing of it can run.
///
/// # Safety
/// `header` is the running executable's ELF header, and this is its main
/// thread's one call.
pub unsafe fn start(header: *const u8, name: &[u8]) {
    // SAFETY: the caller's.
    let (image, tls) = unsafe { executable(header) };
    let Some(area) = layout::area(tcb::VARIANT, tls.memsz, tls.align, AREA_DTV, tcb::BYTES) else {
        refuse(b"this program's PT_TLS has no layout: it cannot start\n");
    };
    let list: &'static Modules = alloc::boxed::Box::leak(alloc::boxed::Box::new(Modules {
        loading: AtomicU32::new(0),
        first: AtomicPtr::new(core::ptr::null_mut()),
        last: AtomicPtr::new(core::ptr::null_mut()),
        count: AtomicUsize::new(0),
        area,
    }));
    // No panic here, which reads a thread-local: an `argv[0]` holds no NUL.
    let name = alloc::ffi::CString::new(name).unwrap_or_default();
    let name: &'static CStr = alloc::boxed::Box::leak(name.into_boxed_c_str());
    let mut first = Loading(list);
    first.add(Module::new(name, image, tls));
    core::mem::forget(first);
    let Ok(at) = reserve(area.size, area.align) else {
        refuse(b"no memory for this program's thread-local storage: it cannot start\n");
    };
    // SAFETY: a fresh reservation of the area's size and alignment, and the
    // executable just published as module 1.
    let tp = unsafe { place(list, at) };
    if enter(tp).is_err() {
        refuse(b"the kernel refused this program's thread pointer: it cannot start\n");
    }
}

/// Write `line` to slot 2 and exit 127: a program that cannot lay out its
/// thread-local storage has nothing that could report it otherwise.
fn refuse(line: &[u8]) -> ! {
    if let Some(stderr) = crate::relocate::stderr() {
        let _ = syscall::write(stderr, line);
    }
    syscall::exit(127)
}

/// `len` bytes of committed memory aligned to `align`.
fn reserve(len: usize, align: usize) -> Result<*mut u8, SyscallError> {
    // SAFETY: placed anywhere, so it replaces nothing.
    unsafe { syscall::vm_reserve(len, VmProt::ReadWrite, VmAt::Anywhere { align: align.max(PAGE_SIZE) }) }
}

/// Lay a thread's area out at `at`, from zeroed memory: the executable's
/// template in, a DTV naming it and the list, and the TCB. Answers the thread
/// pointer, whose tid word [`enter`] writes.
///
/// # Safety
/// `at` is zeroed, writable, `list.area.size` bytes and aligned to `list.area.align`.
unsafe fn place(list: &'static Modules, at: *mut u8) -> usize {
    let area = list.area;
    let base = at as usize;
    // SAFETY: the caller's; every write is inside the area by its layout.
    unsafe {
        let exe = &*list.first.load(Ordering::Acquire);
        core::ptr::copy_nonoverlapping(exe.tls.template as *const u8, at.add(area.data), exe.tls.filesz);
        let dtv = at as *mut Header;
        dtv.write(Header { modules: list, capacity: AREA_SLOTS, owned: 0 });
        let slots = at.add(DTV_SLOTS) as *mut usize;
        slots.write(base + area.data);
        for i in 1..AREA_SLOTS {
            slots.add(i).write(UNALLOCATED);
        }
        let tp = base + area.tp;
        if tcb::VARIANT == Variant::II {
            (tp as *mut usize).write(tp);
        }
        ((tp + tcb::DTV) as *mut usize).write(base);
        tp
    }
}

/// Name `tp` as the calling thread's thread pointer, and write its tid into
/// the TCB there. A spawned thread's first act.
pub fn enter(tp: usize) -> Result<(), SyscallError> {
    let tid = syscall::set_thread_pointer(tp as u64)?;
    // SAFETY: the TCB `place` laid out at `tp`, the calling thread's.
    unsafe { ((tp + tcb::TID) as *mut u32).write(tid.raw()) };
    Ok(())
}

/// A thread's memory: below a [guard](GUARD), its stack, and below that its
/// area. Released whole, by its joiner or by a detached thread's exit.
pub struct ThreadMemory {
    pub reservation: *mut u8,
    pub reserved: usize,
    /// The stack's lowest and one past its highest byte.
    pub stack_base: *mut u8,
    pub stack_top: *mut u8,
    /// What the thread names first ([`enter`]).
    pub tp: usize,
}

/// Reserved pages below a thread's stack, so that running off it faults.
pub const GUARD: usize = PAGE_SIZE;

impl ThreadMemory {
    /// Memory for a thread whose stack is at least `stack` bytes: `None` when
    /// it cannot be had.
    pub fn reserve(stack: usize) -> Option<ThreadMemory> {
        let list = list()?;
        let area = list.area;
        let area_len = area.size.checked_next_multiple_of(PAGE_SIZE)?;
        let stack = stack.max(1).checked_next_multiple_of(PAGE_SIZE)?;
        let reserved = area_len.checked_add(GUARD)?.checked_add(stack)?;
        // SAFETY: placed anywhere, so it replaces nothing.
        let reservation = unsafe {
            syscall::vm_reserve(reserved, VmProt::None, VmAt::Anywhere { align: area.align.max(PAGE_SIZE) })
        }
        .ok()?;
        let stack_base = reservation.wrapping_add(area_len + GUARD);
        // SAFETY: the area's pages and the stack's, inside the reservation just made.
        let committed = unsafe {
            syscall::vm_protect(reservation, area_len, VmProt::ReadWrite)
                .and_then(|()| syscall::vm_protect(stack_base, stack, VmProt::ReadWrite))
        };
        if committed.is_err() {
            // SAFETY: the reservation just made, which nothing reaches.
            let _ = unsafe { syscall::vm_release(reservation, reserved) };
            return None;
        }
        // SAFETY: the area's pages, fresh and so zeroed, at the reservation's
        // aligned start.
        let tp = unsafe { place(list, reservation) };
        Some(ThreadMemory { reservation, reserved, stack_base, stack_top: stack_base.wrapping_add(stack), tp })
    }
}

/// The calling thread's block of module `id`, made from its template the
/// first time this thread asks: `__tls_get_addr`'s slow path, which has
/// nobody to answer a refusal to, so one panics.
pub fn block(id: usize) -> *mut u8 {
    let mut dtv = dtv();
    assert!(!dtv.is_null() && id != 0, "tls: block {id} asked of a thread with no thread-local storage");
    // SAFETY: the calling thread's own DTV, which only it writes.
    unsafe {
        if id > (*dtv).capacity {
            dtv = grow(dtv, id);
        }
        let slot = (dtv as *mut u8).add(DTV_SLOTS + (id - 1) * 8) as *mut usize;
        if *slot == UNALLOCATED {
            let module = modules().find(|m| m.id == id).unwrap_or_else(|| panic!("tls: no module {id}"));
            let tls = module.tls;
            let at = reserve(block_len(&tls), tls.align)
                .unwrap_or_else(|e| panic!("tls: no memory for module {id}'s block: {e:?}"));
            core::ptr::copy_nonoverlapping(tls.template as *const u8, at, tls.filesz);
            *slot = at as usize;
        }
        *slot as *mut u8
    }
}

/// The bytes a dynamic block of `tls` is reserved as.
fn block_len(tls: &Tls) -> usize {
    tls.memsz.max(1).next_multiple_of(PAGE_SIZE)
}

/// A DTV with room for module `id`, made of `dtv`'s slots, now the calling
/// thread's; `dtv` is released if it had a reservation of its own.
///
/// # Safety
/// `dtv` is the calling thread's.
unsafe fn grow(dtv: *mut Header, id: usize) -> *mut Header {
    // SAFETY: the caller's.
    unsafe {
        let old = &*dtv;
        let capacity = id.max(old.capacity * 2);
        let len = (DTV_SLOTS + capacity * 8).next_multiple_of(PAGE_SIZE);
        let at = reserve(len, PAGE_SIZE).unwrap_or_else(|e| panic!("tls: no memory for a DTV of {capacity}: {e:?}"));
        let new = at as *mut Header;
        new.write(Header { modules: old.modules, capacity, owned: len });
        let (from, to) = ((dtv as *const u8).add(DTV_SLOTS) as *const usize, at.add(DTV_SLOTS) as *mut usize);
        core::ptr::copy_nonoverlapping(from, to, old.capacity);
        for i in old.capacity..capacity {
            to.add(i).write(UNALLOCATED);
        }
        ((tcb::thread_pointer() + tcb::DTV) as *mut *mut Header).write(new);
        if old.owned != 0 {
            let _ = syscall::vm_release(dtv as *mut u8, old.owned);
        }
        new
    }
}

/// Release the calling thread's blocks of every module but the executable's,
/// and its DTV if it grew: the thread's last act before it exits, after its
/// destructors. Nothing of a library's thread-locals may be read after it.
pub fn leave() {
    let dtv = dtv();
    if dtv.is_null() {
        return;
    }
    // SAFETY: the calling thread's own DTV and blocks, which only it reaches.
    unsafe {
        let capacity = (*dtv).capacity;
        let slots = (dtv as *mut u8).add(DTV_SLOTS) as *mut usize;
        for module in modules().skip(1).take_while(|m| m.id <= capacity) {
            let slot = slots.add(module.id - 1);
            if *slot != UNALLOCATED {
                let _ = syscall::vm_release(*slot as *mut u8, block_len(&module.tls));
                *slot = UNALLOCATED;
            }
        }
        let owned = (*dtv).owned;
        if owned != 0 {
            // Nothing names what is released.
            let tp = tcb::thread_pointer();
            ((tp + tcb::DTV) as *mut usize).write(0);
            let _ = syscall::vm_release(dtv as *mut u8, owned);
        }
    }
}
