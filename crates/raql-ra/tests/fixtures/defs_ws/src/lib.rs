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

fn private_helper() -> u32 {
    dep_lib::dep_fn()
}

pub(crate) fn crate_helper() -> u32 {
    private_helper()
}

pub mod tests {
    pub fn helper_in_tests() -> u32 {
        crate::crate_helper()
    }
}

#[test]
fn standalone_test() {
    assert_eq!(ANSWER, 42);
}
