mod arch;
mod cat;
mod cp;
mod debug_refused;
mod echo;
mod free;
mod grep;
mod hexdump;
mod locale;
mod ls;
mod mkdir;
mod mv;
mod net;
mod preempt;
mod ps;
mod pwd;
mod reboot;
mod rm;
mod screen;
mod shutdown;
mod spin;
mod stats;
mod tone;
mod unmap_seen;
mod unmap_touch;

macro_rules! commands {
    ($($name:ident),*) => {
        fn run(cmd: &str, args: Vec<String>) {
            match cmd {
                $(stringify!($name) => $name::main(args),)*
                _ => eprintln!("toybox: unknown command '{cmd}'"),
            }
        }
    };
}

use arch::{first_entry, fp_isolation};

commands!(cat, cp, debug_refused, echo, first_entry, fp_isolation, free, grep, hexdump, locale, ls, mkdir, mv, net, preempt, ps, pwd, reboot, rm, screen, shutdown, spin, stats, tone, unmap_seen, unmap_touch);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let invoked_as = std::path::Path::new(&args[0])
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("toybox");

    if invoked_as == "toybox" {
        if args.len() < 2 {
            eprintln!("Usage: toybox <command> [args...]");
            return;
        }
        run(&args[1], args[2..].to_vec());
    } else {
        run(invoked_as, args[1..].to_vec());
    }
}
