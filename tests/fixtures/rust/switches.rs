//! Match arms and loops: each arm is one decision point.

pub fn route(method: &str) -> &'static str {
    match method {
        "GET" => "read",
        "POST" => "write",
        _ => "unknown",
    }
}

pub fn sum(values: &[i32]) -> i32 {
    let mut total = 0;
    for &value in values {
        total += value;
    }
    total
}

pub fn find(values: &[i32], target: i32) -> Option<usize> {
    let mut index = 0;
    loop {
        if index >= values.len() {
            return None;
        }
        if values[index] == target {
            return Some(index);
        }
        index += 1;
    }
}
