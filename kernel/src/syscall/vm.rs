//! The caller's own address space: `vm_*` reserve, commit, protect, discard,
//! resize and take away private memory, and map a memory object's.
//! Exhausting address space is an error return, never an `.expect`.
//!
//! **A length from userland is refused before any arithmetic touches it**:
//! rounding it to pages is the checked sum, and one no page count holds is
//! `InvalidArgument`.

use kernel::vm::Prot;
use toyos_abi::handle::{RawHandle, Rights};

use crate::mm::space::At;
use crate::object::shm::SharedMemObject;
use crate::object::KObjectRef;
use crate::user_ptr::SyscallContext;
use crate::UserAddr;
use crate::process;

use toyos_abi::syscall::*;

/// `size` bytes in whole pages, or `None` for none or a size no page count holds.
fn whole_pages(size: u64) -> Option<u64> {
    size.checked_next_multiple_of(crate::mm::PAGE_SIZE).filter(|&bytes| bytes != 0)
}

/// The protection a `VmProt` word names. No word names write and execute at
/// once: a program's loader writes its code `ReadWrite` and then protects it
/// `ReadExec`, so no page of private memory is ever both.
pub(crate) fn vm_prot(word: u64) -> Result<Prot, SyscallError> {
    Ok(match VmProt::from_u64(word).ok_or(SyscallError::InvalidArgument)? {
        VmProt::None => Prot::None,
        VmProt::Read => Prot::Read,
        VmProt::ReadWrite => Prot::ReadWrite,
        VmProt::ReadExec => Prot::ReadExec,
    })
}

/// `[addr, addr + len)` as a page-aligned start and whole pages.
fn range(addr: u64, len: u64) -> Result<(UserAddr, u64), SyscallError> {
    match (UserAddr::checked(addr), whole_pages(len)) {
        (Some(start), Some(len)) if addr.is_multiple_of(crate::mm::PAGE_SIZE) => Ok((start, len)),
        _ => Err(SyscallError::InvalidArgument),
    }
}

fn answer(done: Result<(), SyscallError>) -> u64 {
    done.map_or_else(SyscallError::to_u64, |()| 0)
}

/// Where a range of `len` bytes goes: anywhere at a multiple of `place` or,
/// for `VM_AT_FIXED`, exactly at `place` in place of whatever those pages held.
fn placed(place: u64, kind: u64, len: u64) -> Result<At, SyscallError> {
    match kind {
        VM_AT_ANYWHERE => Ok(At::Anywhere(place)),
        VM_AT_FIXED => Ok(At::Fixed(range(place, len)?.0)),
        _ => Err(SyscallError::InvalidArgument),
    }
}

/// Private memory, committed unless `prot` is `None` and populated as it is
/// touched.
pub(super) fn sys_vm_reserve(len: u64, prot: u64, place: u64, kind: u64) -> u64 {
    let reserved = (|| {
        let prot = vm_prot(prot)?;
        let len = whole_pages(len).ok_or(SyscallError::InvalidArgument)?;
        let at = placed(place, kind, len)?;
        process::current_address_space().lock().reserve(len, at, prot)
    })();
    reserved.map_or_else(SyscallError::to_u64, UserAddr::raw)
}

/// What a handle carries to map its object as `prot`: `MAP`, and `WRITE` for
/// a mapping that writes. What the object gives is its own to say
/// (`SharedMemObject::mappable`).
pub(crate) fn map_rights(prot: Prot) -> Rights {
    match prot {
        Prot::ReadWrite => Rights::MAP.union(Rights::WRITE),
        Prot::None | Prot::Read | Prot::ReadExec => Rights::MAP,
    }
}

/// `toyos_abi::syscall::vm_map`: a memory object's own pages.
pub(super) fn sys_vm_map(ctx: &SyscallContext<'_>, h: RawHandle, args: UserAddr) -> u64 {
    let args = match ctx.copy_in::<VmMapArgs>(args) { Ok(args) => args, Err(e) => return e.to_u64() };
    let asked = (|| {
        let prot = vm_prot(args.prot)?;
        let len = whole_pages(args.len).ok_or(SyscallError::InvalidArgument)?;
        Ok((prot, len, placed(args.place, args.at, len)?))
    })();
    let (prot, len, at) = match asked {
        Ok(asked) => asked,
        Err(e) => return SyscallError::to_u64(e),
    };
    let space = process::current_address_space();
    match process::with_handles(|handles| handles.get_ref(h, Rights::NONE).map(|o| matches!(o, KObjectRef::Paged(_)))) {
        Ok(true) => return map_paged(h, &space, args.offset, len, prot, at),
        Ok(false) => {}
        Err(e) => return e.refuse(),
    }
    let object = match process::with_handles(|handles| handles.get::<SharedMemObject>(h, map_rights(prot))) {
        Ok(object) => object,
        Err(e) => return e.refuse(),
    };
    let mapped = space.lock().map(alloc::sync::Arc::clone(&object), args.offset, len, prot, at);
    let at = match mapped {
        Ok(at) => at,
        Err(e) => return e.to_u64(),
    };
    // A device's window is taken back with its last handle, so it says
    // where it is mapped; one whose last handle went meanwhile is refused.
    if let Err(e) = object.mapped_at(&space, at, len) {
        space.lock().release_object(at, len, &object);
        return e.to_u64();
    }
    at.raw()
}

/// A paged object's holder `h`: its pages shared, read-only, as `READ`
/// allows, and refused a pager's process as its reads are
/// (`syscall::paged`).
fn map_paged(h: RawHandle, space: &alloc::sync::Arc<crate::mm::space::Space>, offset: u64, len: u64, prot: Prot, at: At) -> u64 {
    if prot != Prot::Read {
        return SyscallError::InvalidArgument.to_u64();
    }
    let holder = match super::paged::holder(h) {
        Ok(holder) => holder,
        Err(refused) => return refused,
    };
    let mapped = space.lock().map_paged(space, &holder, offset, len, prot, at);
    mapped.map_or_else(SyscallError::to_u64, UserAddr::raw)
}

/// Take every page of `[addr, addr + len)` away, as POSIX `munmap` does.
pub(super) fn sys_vm_release(addr: u64, len: u64) -> u64 {
    answer(release(addr, len))
}

/// Take `len` bytes at `addr` out of the caller's space, as `SYS_VM_RELEASE`
/// and a detached thread's exit do.
pub(super) fn release(addr: u64, len: u64) -> Result<(), SyscallError> {
    let (start, len) = range(addr, len)?;
    let space = process::current_address_space();
    let mut space = space.lock();
    space.room(start.raw(), start.raw() + len, 0)?;
    space.release(start, len);
    Ok(())
}

pub(super) fn sys_vm_protect(addr: u64, len: u64, prot: u64) -> u64 {
    answer((|| {
        let prot = vm_prot(prot)?;
        let (start, len) = range(addr, len)?;
        process::current_address_space().lock().protect(start, len, prot)
    })())
}

pub(super) fn sys_vm_discard(addr: u64, len: u64) -> u64 {
    answer(range(addr, len).and_then(|(start, len)| process::current_address_space().lock().discard(start, len)))
}

pub(super) fn sys_vm_decommit(addr: u64, len: u64) -> u64 {
    answer(range(addr, len).and_then(|(start, len)| process::current_address_space().lock().decommit(start, len)))
}

/// Resize committed private memory in place, or move it when `may_move` is 1.
pub(super) fn sys_vm_remap(addr: u64, old: u64, new: u64, may_move: u64) -> u64 {
    let remapped = (|| {
        let may_move = match may_move {
            0 => false,
            1 => true,
            _ => return Err(SyscallError::InvalidArgument),
        };
        let (start, old) = range(addr, old)?;
        let new = whole_pages(new).ok_or(SyscallError::InvalidArgument)?;
        process::current_address_space().lock().remap(start, old, new, may_move)
    })();
    remapped.map_or_else(SyscallError::to_u64, UserAddr::raw)
}


