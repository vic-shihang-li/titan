mod cli;
mod net;
mod protocol;
mod rip;
mod route;
mod utils;

pub use net::Args;

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        let result = 2 + 2;
        assert_eq!(result, 4);
    }
}
