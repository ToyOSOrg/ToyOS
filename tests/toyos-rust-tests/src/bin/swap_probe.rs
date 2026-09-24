//! What a program sshd runs undeclared holds of the swap port: uploaded and run
//! over ssh, so the manifest declares nothing for it and std spawns it with a
//! duplicate of sshd's namespace.
//!
//! Argv is the port's name, its endowment label and the swap message type,
//! from `toyos_swap` on the host. `netd` is asked for first, so a spawn that
//! inherited nothing at all is not read as the port withheld. Exit 0 is the
//! port out of reach; 1 is the port reached, and the line says what init
//! answered a frame that is no swap request.

use toyos::endow::{self, EndowError, Endowments};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, port, label, msg] = args.as_slice() else {
        println!("swap_probe: asked with {args:?}, not <port> <label> <message type>");
        std::process::exit(3);
    };
    let msg: u32 = msg.parse().unwrap_or_else(|_| panic!("swap_probe: {msg:?} is no message type"));
    if let Err(e) = endow::service("netd") {
        println!("swap_probe: inherited no namespace holding netd: {e:?}");
        std::process::exit(2);
    }
    if Endowments::get().holds(label) {
        println!("swap_probe: holds the {label:?} endowment");
        std::process::exit(1);
    }
    let conn = match endow::service(port) {
        Err(EndowError::NotEndowed) => {
            println!("swap_probe: {port} is not in the namespace it inherited");
            return;
        }
        Err(e) => {
            println!("swap_probe: {port} is in the namespace it inherited, and answered {e:?}");
            std::process::exit(1);
        }
        Ok(conn) => conn,
    };
    let answer = match conn.send_bytes(msg, b"not a swap request").map(|()| conn.recv_header()) {
        Ok(Ok(header)) => {
            let mut text = vec![0u8; header.len() as usize];
            let n = conn.recv_bytes(&header, &mut text).unwrap_or(0);
            format!("message {}: {}", header.msg_type, String::from_utf8_lossy(&text[..n]))
        }
        Ok(Err(e)) | Err(e) => format!("{e:?}"),
    };
    println!("swap_probe: reached init through {port}: {answer}");
    std::process::exit(1);
}
