pub mod engine;
pub mod util;

// Re-export façade — the `pub use` bug (#1) makes this invisible today.
pub use engine::Engine;

pub trait Runner {
    fn run(&self) -> u32;
}

pub struct FastRunner;

impl Runner for FastRunner {
    fn run(&self) -> u32 {
        1
    }
}

pub fn call_dynamic(r: &dyn Runner) -> u32 {
    r.run()
}
