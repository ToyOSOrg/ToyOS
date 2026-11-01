//! Power the machine off ([`crate::stopping`]).

pub fn main(args: Vec<String>) {
    crate::stopping::main("shutdown", toyos::power::Stop::Shutdown, &args);
}
