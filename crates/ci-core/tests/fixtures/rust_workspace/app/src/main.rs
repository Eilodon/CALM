use demo_core::{call_dynamic, Engine, FastRunner};

fn main() {
    let e = Engine::new();
    let n = e.start();
    let d = call_dynamic(&FastRunner);
    println!("{} {}", n, d);
}
