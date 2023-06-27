use crate::Args;

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

        fn into_shuffled(self) -> ABCNet {
            let mut args = [self.a, self.b, self.c];

            Self::replace_ports(&mut args);

            let mut args_iter = args.into_iter();
            let (a, b, c) = (
                args_iter.next().unwrap(),
                args_iter.next().unwrap(),
                args_iter.next().unwrap(),
            );

            ABCNet { a, b, c }
        }

        fn replace_ports(args: &mut [Args]) {
            let mut rng = thread_rng();
            let mut replacements = Vec::new();
            for arg in args.iter_mut() {
                let old = arg.host_port;
                let new = rng.gen_range(1024..65535);
                arg.host_port = new;
                replacements.push((old, new));
            }

            for arg in args {
                for link in &mut arg.links {
                    for (old, new) in &replacements {
                        if link.dest_port == *old {
                            link.dest_port = *new;
                            break;
                        }
                    }
                }
            }
        }
    }

    pub fn load() -> ABCNet {
        ABCNet::load_default()
    }

    pub fn gen_unique() -> ABCNet {
        ABCNet::load_default().into_shuffled()
    }
}
