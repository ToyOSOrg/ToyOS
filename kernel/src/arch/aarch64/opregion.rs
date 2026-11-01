//! No machine of this architecture has an IGD OpRegion, and its direct map
//! does not reach firmware's reserved memory.

use toyos_abi::pci::WindowRefused;
use toyos_userbound::window::Window;

/// Never reached: the mint grants the window only to an Intel display
/// function, which no machine of this architecture has.
pub fn window(_asls: u32) -> Result<Window, WindowRefused> {
    Err(WindowRefused::NotFirmware)
}
