//! Pipe A's vertical blank as the claim's interrupt, by Vol 12 "Display
//! Interrupts": the one display interrupt this driver enables, and only while
//! it waits for one.

use toyos_device_memory::Registers;

use crate::{regs, Refusal};

/// Every display interrupt but pipe A's vblank masked and disabled, pipe A's
/// identity cleared and display and primary interrupts enabled. Nothing is
/// raised until [`arm`].
pub fn claim<R: Registers>(regs: &R) {
    for ier in regs::UNUSED_IERS {
        regs.write32(ier, 0);
        regs.write32(ier - 8, !0);
    }
    regs.write32(regs::PIPE_A_IER, 0);
    regs.write32(regs::PIPE_A_IMR, !regs::VBLANK);
    clear(regs);
    regs.write32(regs::DISPLAY_INT_CTL, regs::DISPLAY_INTERRUPT_ENABLE);
    regs.write32(regs::GFX_MSTR_INTR, regs::PRIMARY_INTERRUPT);
}

/// Raise the next vblank's start: the identity a vblank before this call set
/// is cleared first, so the one raised began after it.
pub fn arm<R: Registers>(regs: &R) {
    clear(regs);
    regs.write32(regs::PIPE_A_IER, regs::VBLANK);
}

/// The claim's interrupt served (Vol 12 "Interrupt Service Routine"):
/// whether it was the vblank [`arm`] asked for, which disarms it.
///
/// Primary and display interrupts are disabled while the sources are cleared
/// and enabled after, so a source still pending raises a new message. A
/// category this driver enabled nothing in is refused, and display interrupts
/// stay disabled.
pub fn serve<R: Registers>(regs: &R) -> Result<bool, Refusal> {
    if regs.read32(regs::GFX_MSTR_INTR) & regs::DISPLAY_PENDING == 0 {
        return Ok(false);
    }
    regs.write32(regs::GFX_MSTR_INTR, 0);
    regs.write32(regs::DISPLAY_INT_CTL, 0);
    let pending = regs.read32(regs::DISPLAY_INT_CTL) & regs::PENDING;
    if pending & !regs::PIPE_A_PENDING != 0 {
        return Err(Refusal::Interrupts(pending));
    }
    let vblank = clear(regs) & regs::VBLANK != 0;
    if vblank {
        regs.write32(regs::PIPE_A_IER, 0);
    }
    regs.write32(regs::DISPLAY_INT_CTL, regs::DISPLAY_INTERRUPT_ENABLE);
    regs.write32(regs::GFX_MSTR_INTR, regs::PRIMARY_INTERRUPT);
    Ok(vblank)
}

/// Pipe A's identity bits cleared, both events it can hold ("The IIR can
/// queue up to two interrupt events"); what was set.
fn clear<R: Registers>(regs: &R) -> u32 {
    let mut seen = 0;
    for _ in 0..2 {
        let iir = regs.read32(regs::PIPE_A_IIR);
        if iir == 0 {
            break;
        }
        seen |= iir;
        regs.write32(regs::PIPE_A_IIR, iir);
    }
    seen
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Engine;

    fn raised(engine: &Engine) -> bool {
        engine.read32(regs::GFX_MSTR_INTR) & regs::DISPLAY_PENDING != 0
    }

    #[test]
    fn claimed_interrupts_raise_nothing_until_armed() {
        let engine = Engine::t14();
        for ier in regs::UNUSED_IERS {
            engine.set(ier, !0);
        }
        claim(&engine);
        assert!(regs::UNUSED_IERS.iter().all(|&ier| engine.get(ier) == 0 && engine.get(ier - 8) == !0));
        assert_eq!(engine.get(regs::GFX_MSTR_INTR), regs::PRIMARY_INTERRUPT);
        assert_eq!(engine.get(regs::DISPLAY_INT_CTL), regs::DISPLAY_INTERRUPT_ENABLE);
        engine.vblank();
        assert!(!raised(&engine));
        assert_eq!(serve(&engine), Ok(false));
    }

    /// A vblank that began before the present arrived is not its vblank, and
    /// the one after it is raised once.
    #[test]
    fn an_armed_vblank_is_the_next_one_and_raises_once() {
        let engine = Engine::t14();
        claim(&engine);
        engine.vblank();
        arm(&engine);
        assert!(!raised(&engine), "the vblank before the arm");
        engine.vblank();
        assert!(raised(&engine));
        assert_eq!(serve(&engine), Ok(true));
        assert_eq!(engine.get(regs::GFX_MSTR_INTR), regs::PRIMARY_INTERRUPT, "re-enabled after the clear");
        engine.vblank();
        assert!(!raised(&engine), "disarmed by the vblank it served");
    }

    #[test]
    fn a_category_no_one_enabled_is_refused_and_display_interrupts_stay_off() {
        let engine = Engine::t14();
        claim(&engine);
        engine.other_pending.set(1 << 23);
        assert_eq!(serve(&engine), Err(Refusal::Interrupts(1 << 23)));
        assert_eq!(engine.get(regs::DISPLAY_INT_CTL) & regs::DISPLAY_INTERRUPT_ENABLE, 0);
        assert_eq!(engine.get(regs::GFX_MSTR_INTR), 0);
    }
}
