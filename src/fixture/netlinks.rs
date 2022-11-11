use crate::Args;
use lazy_static::lazy_static;

pub mod abc {
    use rand::{thread_rng, Rng};

    use super::*;

    pub struct ABCNet {
        pub a: Args,
        pub b: Args,
        pub c: Args,
    }

    impl ABCNet {
        fn load_default() -> ABCNet {
            Self {
                a: Args::parse_from_path("./net_links/abc/A.lnx").unwrap(),
                b: Args::parse_from_path("./net_links/abc/B.lnx").unwrap(),
                c: Args::parse_from_path("./net_links/abc/C.lnx").unwrap(),
            }
        }

        fn into_shuffled(mut self) -> ABCNet {
            let mut rng = thread_rng();
            self.a.host_port = rng.gen_range(1024..65535);
            self.b.host_port = rng.gen_range(1024..65535);
            self.c.host_port = rng.gen_range(1024..65535);
            self
        }
    }

    pub fn load() -> ABCNet {
        ABCNet::load_default()
    }

    pub fn gen_unique() -> ABCNet {
        ABCNet::load_default().into_shuffled()
    }
}
