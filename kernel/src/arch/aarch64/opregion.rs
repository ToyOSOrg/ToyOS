//! This architecture's direct map does not reach firmware's reserved memory,
//! so an IGD OpRegion, which an Intel display card in a slot may have, is
//! refused here.

use toyos_abi::pci::WindowRefused;
use toyos_userbound::window::Window;

pub fn window(_asls: u32) -> Result<Window, WindowRefused> {
    Err(WindowRefused::NotFirmware)
}
