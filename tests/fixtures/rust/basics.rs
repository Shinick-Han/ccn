//! Basic branching shapes: the base-1 guard and plain conditionals.

pub fn identity(value: i32) -> i32 {
    value
}

pub fn pick(flag: bool) -> i32 {
    if flag {
        1
    } else {
        0
    }
}

pub fn grade(score: u32) -> &'static str {
    if score >= 90 {
        "A"
    } else if score >= 80 {
        "B"
    } else {
        "C"
    }
}
