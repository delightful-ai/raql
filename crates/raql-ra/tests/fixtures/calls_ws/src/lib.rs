pub trait Greet {
    fn greet(&self) -> u32;
}

pub struct En;
pub struct Es;

impl Greet for En {
    fn greet(&self) -> u32 {
        1
    }
}

impl Greet for Es {
    fn greet(&self) -> u32 {
        2
    }
}

pub struct Counter {
    pub n: u32,
}

impl Counter {
    pub fn tick(&mut self) -> u32 {
        self.n += 1;
        self.n
    }
}

pub fn helper() -> u32 {
    3
}

pub fn static_call() -> u32 {
    helper()
}

pub fn generic_call<G: Greet>(g: &G) -> u32 {
    g.greet()
}

pub fn dyn_call(g: &dyn Greet) -> u32 {
    g.greet()
}

pub fn inherent_method_call(c: &mut Counter) -> u32 {
    c.tick()
}

pub fn closure_using() -> u32 {
    let f = |x: u32| x + 1;
    f(helper())
}

macro_rules! call_with {
    ($f:ident) => {
        $f()
    };
}

pub fn macro_call() -> u32 {
    call_with!(helper)
}

macro_rules! call_helper {
    () => {
        helper()
    };
}

pub fn hidden_macro_call() -> u32 {
    call_helper!()
}

#[cfg(feature = "never-enabled")]
pub fn gated_caller() -> u32 {
    helper()
}

pub fn takes_fn_value() -> u32 {
    // A non-call reference to `helper`: not a call edge.
    let f: fn() -> u32 = helper;
    f()
}
