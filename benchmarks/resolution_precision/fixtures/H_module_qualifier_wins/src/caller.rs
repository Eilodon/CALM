fn write() -> i32 {
    0
}

pub fn use_it() -> i32 {
    crate::telemetry::write()
}
