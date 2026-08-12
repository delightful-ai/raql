pub enum Gadget {
    Small,
    Large { watts: u32 },
}

pub mod inner {
    pub struct Deep;
}

pub fn duplicate_name() -> u32 {
    2
}
