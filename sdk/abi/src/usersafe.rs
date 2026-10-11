//! Values the kernel copies whole across the user boundary: [`UserSafe`], and
//! [`user_safe!`](crate::user_safe), the only way a struct comes to be one.
//!
//! A struct declared through the macro is refused by the compiler when a byte
//! of it belongs to no field or a field's type is not itself `UserSafe`. The
//! impls written by hand are the ones below and no others: the fixed-width
//! integers and arrays, which the macro has no fields to check.

/// A type every bit pattern is a value of and no byte of is padding, so a copy
/// in from user memory is a value whatever the caller wrote, and a copy out
/// publishes only what its fields hold.
///
/// # Safety
/// Both halves of the sentence above hold. [`user_safe!`](crate::user_safe)
/// proves them of a struct; nothing else implements this.
pub unsafe trait UserSafe: Copy {}

macro_rules! integers {
    ($($int:ty)*) => {$(
        // SAFETY: an integer has no padding and every bit pattern is one.
        unsafe impl UserSafe for $int {}
    )*};
}
integers!(u8 i8 u16 i16 u32 i32 u64 i64);

// SAFETY: an array is its elements end to end, with nothing between or after them.
unsafe impl<T: UserSafe, const N: usize> UserSafe for [T; N] {}

/// The bytes of a value, which are what it is on the wire.
pub fn bytes<T: UserSafe>(value: &T) -> &[u8] {
    // SAFETY: a `&T` is readable for `size_of::<T>()` bytes, and `T: UserSafe` leaves none of them padding.
    unsafe { core::slice::from_raw_parts(core::ptr::from_ref(value).cast(), core::mem::size_of::<T>()) }
}

/// The size of a field, for a field that is [`UserSafe`].
#[doc(hidden)]
pub const fn field<T: UserSafe>() -> usize {
    core::mem::size_of::<T>()
}

/// Declares a struct as [`UserSafe`]: `#[repr(C)]` over named fields, or
/// `#[repr(transparent)]` over one unnamed field.
///
/// A byte that belongs to no field fails the build, wherever the gap is:
///
/// ```
/// toyos_abi::user_safe! {
///     #[derive(Clone, Copy)]
///     struct Whole { a: u64, b: u64 }
/// }
/// ```
///
/// ```compile_fail
/// toyos_abi::user_safe! {
///     #[derive(Clone, Copy)]
///     struct Tail { a: u64, b: u32 }
/// }
/// ```
///
/// So does a field with a bit pattern that is no value of its type, though it
/// leaves no gap:
///
/// ```
/// toyos_abi::user_safe! {
///     #[derive(Clone, Copy)]
///     struct Words { a: u32, b: u32 }
/// }
/// ```
///
/// ```compile_fail
/// toyos_abi::user_safe! {
///     #[derive(Clone, Copy)]
///     struct Scalar { a: u32, b: char }
/// }
/// ```
#[macro_export]
macro_rules! user_safe {
    (
        $(#[$attr:meta])*
        $vis:vis struct $name:ident {
            $($(#[$field_attr:meta])* $field_vis:vis $field:ident: $ty:ty),* $(,)?
        }
    ) => {
        $(#[$attr])*
        #[repr(C)]
        $vis struct $name {
            $($(#[$field_attr])* $field_vis $field: $ty),*
        }
        $crate::user_safe!(@impl $name: $($ty),*);
    };
    (
        $(#[$attr:meta])*
        $vis:vis struct $name:ident($field_vis:vis $ty:ty);
    ) => {
        $(#[$attr])*
        #[repr(transparent)]
        $vis struct $name($field_vis $ty);
        $crate::user_safe!(@impl $name: $ty);
    };
    (@impl $name:ident: $($ty:ty),*) => {
        const _: () = ::core::assert!(
            ::core::mem::size_of::<$name>() == 0 $(+ $crate::usersafe::field::<$ty>())*,
            ::core::concat!("a byte of `", stringify!($name), "` belongs to no field"),
        );
        // SAFETY: every field is `UserSafe`, and the assertion above leaves no byte outside one.
        unsafe impl $crate::UserSafe for $name {}
    };
}
