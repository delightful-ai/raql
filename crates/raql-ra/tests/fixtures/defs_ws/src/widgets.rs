pub struct Widget {
    pub size: u32,
}

impl Widget {
    pub const DEFAULT_SIZE: u32 = 3;

    pub fn grow(&mut self, by: u32) -> u32 {
        self.size += by;
        self.size
    }
}

pub trait Render {
    type Output;

    fn render(&self) -> Self::Output;
}

impl Render for Widget {
    type Output = u32;

    fn render(&self) -> u32 {
        self.size
    }
}

pub fn duplicate_name() -> u32 {
    1
}
