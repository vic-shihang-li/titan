use crate::Args;
use lazy_static::lazy_static;

pub mod abc {
    use super::*;

    lazy_static! {
        pub static ref A: Args = Args::parse_from_path("./net_links/abc/A.lnx").unwrap();
        pub static ref B: Args = Args::parse_from_path("./net_links/abc/B.lnx").unwrap();
        pub static ref C: Args = Args::parse_from_path("./net_links/abc/C.lnx").unwrap();
    }
}
