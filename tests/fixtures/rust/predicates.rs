//! Compound predicates in boolean expressions.

pub fn can_publish(is_admin: bool, approved: bool) -> bool {
    is_admin && approved
}

pub fn should_retry(attempts: u32, fatal: bool) -> bool {
    attempts < 3 || fatal
}

pub fn mixed(a: bool, b: bool, c: bool) -> bool {
    a && (b || c)
}

pub fn clamped(value: i32, low: i32, high: i32) -> i32 {
    if value < low || value > high {
        low
    } else {
        value
    }
}
