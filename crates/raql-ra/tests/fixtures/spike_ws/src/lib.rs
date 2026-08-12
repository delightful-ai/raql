pub mod extra;

pub struct Widget {
    pub count: u32,
}

impl Widget {
    pub fn bump(&mut self) -> u32 {
        self.count += 1;
        self.count
    }
}

pub fn alpha(w: &mut Widget) -> u32 {
    let from_beta = beta();
    let from_bump = w.bump();
    from_beta + from_bump + gamma()
}

pub fn beta() -> u32 {
    1
}

pub fn gamma() -> u32 {
    2
}
