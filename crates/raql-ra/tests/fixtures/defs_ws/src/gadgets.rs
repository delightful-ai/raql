pub enum Gadget {
    Small,
    Large { watts: u32 },
}

pub mod inner {
    pub struct Deep;

    impl Deep {
        pub fn poke(&self) -> u32 {
            0
        }
    }
}

pub fn duplicate_name() -> u32 {
    2
}
