pub fn get(k: &str) -> i32 {
    k.len() as i32
}

pub fn caller(m: &std::collections::HashMap<String, i32>, k: &str) -> Option<i32> {
    m.get(k).copied()
}
