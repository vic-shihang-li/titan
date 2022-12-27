#![feature(map_try_insert)]

pub mod cli;
mod drop_policy;
#[cfg(test)]
mod fixture;
mod net;
pub mod node;
pub mod protocol;
mod repl;
mod route;
mod utils;

pub use net::Args;

/// Trait to be implemented by payload to be sent over the network.
trait Message {
    /// Convert a message into bytes.
    fn into_bytes(self) -> Vec<u8>;
    /// Convert bytes into a message.
    fn from_bytes(bytes: &[u8]) -> Self;
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
