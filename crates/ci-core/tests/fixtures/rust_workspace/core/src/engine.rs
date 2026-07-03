pub struct Engine {
    pub id: u32,
}

impl Engine {
    pub fn new() -> Self {
        Engine { id: 0 }
    }

    pub fn start(&self) -> u32 {
        self.id + 1
    }
}
