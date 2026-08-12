pub mod gadgets;
pub mod widgets;

macro_rules! make_fn {
    ($name:ident) => {
        pub fn $name() -> u32 {
            7
        }
    };
}

make_fn!(made_by_macro);

#[cfg(feature = "never-enabled")]
pub fn cfg_gated() -> u32 {
    0
}

pub const ANSWER: u32 = 42;

pub static GREETING: &str = "hi";

pub type Meters = u32;
