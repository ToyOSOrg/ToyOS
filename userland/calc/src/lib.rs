//! The calculator, without a window.
//!
//! The layers stack: [`bigint`] under [`rational`] and [`dec`], those two under
//! [`num`], and [`parser`] and [`prog`] over that. [`app`] is the calculator as
//! a state machine — every button and every key ends up there — and [`layout`]
//! is where the window puts each of them, at any size.

pub mod app;
pub mod bigint;
pub mod dec;
pub mod error;
pub mod layout;
pub mod num;
pub mod parser;
pub mod prog;
pub mod rational;
