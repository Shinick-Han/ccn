//! Error propagation, while loops, and if-let guards.

pub fn parse_all(inputs: &[&str]) -> Result<Vec<i32>, std::num::ParseIntError> {
    let mut parsed = Vec::new();
    for input in inputs {
        parsed.push(input.parse::<i32>()?);
    }
    Ok(parsed)
}

pub fn countdown(mut n: i32) -> i32 {
    while n > 0 {
        n -= 1;
    }
    n
}

pub fn unwrap_or_default(value: Option<i32>) -> i32 {
    if let Some(inner) = value {
        inner
    } else {
        0
    }
}
