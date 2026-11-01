//! Return the machine to firmware ([`crate::stopping`]).

pub fn main(args: Vec<String>) {
    crate::stopping::main("reboot", toyos::power::Stop::Reboot, &args);
}
