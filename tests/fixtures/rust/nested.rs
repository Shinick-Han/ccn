//! Nested closures are measured separately from their enclosing function.

pub fn make_adder(base: i32) -> impl Fn(i32) -> i32 {
    move |value| {
        if value > 0 {
            base + value
        } else {
            base
        }
    }
}

pub fn apply_all(values: &[i32], limit: i32) -> Vec<i32> {
    values
        .iter()
        .filter(|value| **value > limit)
        .copied()
        .collect()
}

pub fn count_matches(values: &[i32], target: i32) -> usize {
    values.iter().filter(|v| **v == target).count()
}
