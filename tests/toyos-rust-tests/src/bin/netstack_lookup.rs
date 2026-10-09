//! One name asked of the resolver netstack's lease named: QEMU's user
//! network's, which hands the question to the host's. The name is under
//! `.invalid` (RFC 6761 §6.4), so no answer has an address to depend on, and
//! the job passes on any word a server gave: an address, none, or a failure
//! of the server's. A wait nobody answered, or a query that found no way
//! out, is the red.

use toyos::net::NetError;

const NAME: &str = "toyos-test.invalid";

fn main() {
    let mut addresses = [[0u8; 4]; 4];
    let answered = match toyos::net::dns_lookup(NAME, &mut addresses) {
        Ok(0) => "no address",
        Ok(_) => "addresses",
        // What netstack answers a lookup whose server said it failed.
        Err(NetError::Io) => "a server's failure",
        Err(why) => panic!("the lookup of {NAME} reached no server's answer: {why:?}"),
    };
    println!("netstack_lookup: the resolver answered {NAME} with {answered}");
    println!("netstack_lookup: ok");
}
