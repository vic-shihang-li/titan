pub mod cli;
pub mod net;
mod protocol;
mod rip;
pub mod route;

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
