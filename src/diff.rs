//! Line-oriented diff engine.
//!
//! Uses Myers' O(ND) difference algorithm in its linear-space variant: the
//! `find_middle_snake` routine locates a middle snake of a shortest edit
//! script, and the input is split there and processed recursively. Memory is
//! O(N + M) beyond the returned rows, and time is O((N + M) * D) where D is the
//! number of inserted plus deleted lines. Typical edits (D small) are close to
//! linear; even wholly unrelated 5k-line files stay bounded by the recursion
//! depth and the per-level V arrays rather than by an N*M table.

/// Classification of a single row in a unified line sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffKind {
    /// Line exists unchanged in both sides.
    Context,
    /// Line exists only in the new input.
    Added,
    /// Line exists only in the old input.
    Removed,
}

/// One row of a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// Whether this row is context, an addition, or a removal.
    pub kind: DiffKind,
    /// 1-based line number in the old input, when the line exists there.
    pub old_line: Option<usize>,
    /// 1-based line number in the new input, when the line exists there.
    pub new_line: Option<usize>,
    /// The line text without its terminator.
    pub text: String,
}

/// Compute a unified line sequence for `old` -> `new`.
///
/// Removals precede additions at each changed position. Identical inputs yield
/// only [`DiffKind::Context`] rows.
pub fn diff_lines(old: &[String], new: &[String]) -> Vec<DiffLine> {
    let mut out = Vec::new();
    append_diff(old, new, 0, 0, &mut out);
    removals_first(&mut out);
    out
}

/// Stable-partition every contiguous changed block so removals precede
/// additions, which is the conventional unified-diff presentation of a
/// replacement. Context rows separate blocks and are never moved.
fn removals_first(rows: &mut [DiffLine]) {
    let mut start = 0;
    while start < rows.len() {
        if rows[start].kind == DiffKind::Context {
            start += 1;
            continue;
        }
        let mut end = start;
        while end < rows.len() && rows[end].kind != DiffKind::Context {
            end += 1;
        }
        rows[start..end].sort_by_key(|row| row.kind == DiffKind::Added);
        start = end;
    }
}

/// Number of rows that represent a change (additions plus removals).
pub fn changed_count(rows: &[DiffLine]) -> usize {
    rows.iter()
        .filter(|row| matches!(row.kind, DiffKind::Added | DiffKind::Removed))
        .count()
}

fn append_diff(
    old: &[String],
    new: &[String],
    old_offset: usize,
    new_offset: usize,
    out: &mut Vec<DiffLine>,
) {
    // Peel off the common prefix and suffix first. Besides being cheap, this
    // guarantees that recursive splits make progress even when a middle snake
    // degenerates to a point on a boundary.
    let prefix = old
        .iter()
        .zip(new.iter())
        .take_while(|(left, right)| left == right)
        .count();
    for (index, text) in old.iter().enumerate().take(prefix) {
        push_context(out, old_offset + index, new_offset + index, text);
    }

    let old_middle = &old[prefix..];
    let new_middle = &new[prefix..];
    let suffix = old_middle
        .iter()
        .rev()
        .zip(new_middle.iter().rev())
        .take_while(|(left, right)| left == right)
        .count();
    let old_core = &old_middle[..old_middle.len() - suffix];
    let new_core = &new_middle[..new_middle.len() - suffix];

    append_core(
        old_core,
        new_core,
        old_offset + prefix,
        new_offset + prefix,
        out,
    );

    // Shared suffix, in original order.
    let old_suffix_start = prefix + old_core.len();
    let new_suffix_start = prefix + new_core.len();
    for index in 0..suffix {
        push_context(
            out,
            old_offset + old_suffix_start + index,
            new_offset + new_suffix_start + index,
            &old_middle[old_core.len() + index],
        );
    }
}

/// Diff a core region whose first and last lines are known to differ.
fn append_core(
    old: &[String],
    new: &[String],
    old_offset: usize,
    new_offset: usize,
    out: &mut Vec<DiffLine>,
) {
    // Base cases: one side exhausted, so the remainder is a pure block change.
    if old.is_empty() {
        for (index, text) in new.iter().enumerate() {
            out.push(DiffLine {
                kind: DiffKind::Added,
                old_line: None,
                new_line: Some(new_offset + index + 1),
                text: text.clone(),
            });
        }
        return;
    }
    if new.is_empty() {
        for (index, text) in old.iter().enumerate() {
            out.push(DiffLine {
                kind: DiffKind::Removed,
                old_line: Some(old_offset + index + 1),
                new_line: None,
                text: text.clone(),
            });
        }
        return;
    }

    let snake = find_middle_snake(old, new);
    let empty = snake.old_start == snake.old_end;
    let at_start = snake.old_start == 0 && snake.new_start == 0;
    let at_end = snake.old_end == old.len() && snake.new_end == new.len();
    if empty && (at_start || at_end) {
        // Degenerate split with no progress; replacing the whole core keeps
        // the result well formed (though it may not be minimal).
        for (index, text) in old.iter().enumerate() {
            out.push(DiffLine {
                kind: DiffKind::Removed,
                old_line: Some(old_offset + index + 1),
                new_line: None,
                text: text.clone(),
            });
        }
        for (index, text) in new.iter().enumerate() {
            out.push(DiffLine {
                kind: DiffKind::Added,
                old_line: None,
                new_line: Some(new_offset + index + 1),
                text: text.clone(),
            });
        }
        return;
    }

    // Left of the middle snake (which re-peels its own common edges).
    append_diff(
        &old[..snake.old_start],
        &new[..snake.new_start],
        old_offset,
        new_offset,
        out,
    );

    // The middle snake itself is a run of common lines.
    for index in 0..(snake.old_end - snake.old_start) {
        push_context(
            out,
            old_offset + snake.old_start + index,
            new_offset + snake.new_start + index,
            &old[snake.old_start + index],
        );
    }

    // Right of the middle snake.
    append_diff(
        &old[snake.old_end..],
        &new[snake.new_end..],
        old_offset + snake.old_end,
        new_offset + snake.new_end,
        out,
    );
}

fn push_context(out: &mut Vec<DiffLine>, old_index: usize, new_index: usize, text: &str) {
    out.push(DiffLine {
        kind: DiffKind::Context,
        old_line: Some(old_index + 1),
        new_line: Some(new_index + 1),
        text: text.to_string(),
    });
}

/// A middle snake of a shortest edit script, in old/new coordinates.
struct Snake {
    old_start: usize,
    new_start: usize,
    old_end: usize,
    new_end: usize,
}

/// Find a middle snake of a shortest edit script using Myers' linear-space
/// refinement.
///
/// Forward and backward searches advance by edit distance `d` in lockstep.
/// When the two frontier stripes overlap on a diagonal, the overlap spans a
/// snake that lies on an optimal path; both callers split around it.
fn find_middle_snake(old: &[String], new: &[String]) -> Snake {
    let n = old.len();
    let m = new.len();
    let delta = n as isize - m as isize;
    let odd = delta % 2 != 0;
    let max_d = (n + m).div_ceil(2);
    let offset = max_d as isize + 1;
    let size = 2 * max_d + 3;
    let mut forward = vec![0isize; size];
    let mut backward = vec![0isize; size];

    for d in 0..=max_d {
        let d = d as isize;

        // Forward search from (0, 0). `forward[k]` is the furthest old-line
        // index reachable on diagonal k = x - y.
        for k in (-d..=d).step_by(2) {
            let index = (k + offset) as usize;
            let mut x = if k == -d || (k != d && forward[index - 1] < forward[index + 1]) {
                forward[index + 1]
            } else {
                forward[index - 1] + 1
            };
            let mut y = x - k;
            while x < n as isize && y < m as isize && old[x as usize] == new[y as usize] {
                x += 1;
                y += 1;
            }
            forward[index] = x;

            // Backward frontier for diagonal `delta - k` was built at d - 1.
            // An overlap means a shortest path crosses here.
            if odd {
                let back_diagonal = delta - k;
                if back_diagonal > -d && back_diagonal < d {
                    let back_index = (back_diagonal + offset) as usize;
                    if x + backward[back_index] >= n as isize {
                        let old_end = x as usize;
                        let new_end = (x - k) as usize;
                        let old_start = n - backward[back_index] as usize;
                        let new_start = (old_start as isize - k) as usize;
                        return Snake {
                            old_start,
                            new_start,
                            old_end,
                            new_end,
                        };
                    }
                }
            }
        }

        // Backward search from (n, m) over the reversed inputs. `backward[k]`
        // is the furthest count `u` walked back along old on diagonal
        // k = u - v; original coordinates are x = n - u, y = m - v.
        for k in (-d..=d).step_by(2) {
            let index = (k + offset) as usize;
            let mut u = if k == -d || (k != d && backward[index - 1] < backward[index + 1]) {
                backward[index + 1]
            } else {
                backward[index - 1] + 1
            };
            let mut v = u - k;
            while u < n as isize
                && v < m as isize
                && old[(n as isize - u - 1) as usize] == new[(m as isize - v - 1) as usize]
            {
                u += 1;
                v += 1;
            }
            backward[index] = u;

            // The forward frontier for this original diagonal exists at d.
            if !odd {
                let forward_diagonal = delta - k;
                if forward_diagonal >= -d && forward_diagonal <= d {
                    let forward_index = (forward_diagonal + offset) as usize;
                    if forward[forward_index] + u >= n as isize {
                        let old_start = (n as isize - u) as usize;
                        let new_start = (old_start as isize - forward_diagonal) as usize;
                        let old_end = forward[forward_index] as usize;
                        let new_end = (old_end as isize - forward_diagonal) as usize;
                        return Snake {
                            old_start,
                            new_start,
                            old_end,
                            new_end,
                        };
                    }
                }
            }
        }
    }

    // Unreachable for valid inputs: the frontiers must overlap by d = max_d.
    // Fall back to a conservative whole-block replacement so a logic error can
    // never loop or panic.
    Snake {
        old_start: 0,
        new_start: 0,
        old_end: 0,
        new_end: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    fn render(rows: &[DiffLine]) -> Vec<(DiffKind, Option<usize>, Option<usize>, String)> {
        rows.iter()
            .map(|row| (row.kind, row.old_line, row.new_line, row.text.clone()))
            .collect()
    }

    fn assert_well_formed(old: &[String], new: &[String], rows: &[DiffLine]) {
        let mut old_seen = Vec::new();
        let mut new_seen = Vec::new();
        for row in rows {
            match row.kind {
                DiffKind::Context => {
                    let o = row.old_line.expect("context has old line");
                    let n = row.new_line.expect("context has new line");
                    assert_eq!(old[o - 1], row.text);
                    assert_eq!(new[n - 1], row.text);
                    old_seen.push(o);
                    new_seen.push(n);
                }
                DiffKind::Removed => {
                    let o = row.old_line.expect("removal has old line");
                    assert!(row.new_line.is_none());
                    assert_eq!(old[o - 1], row.text);
                    old_seen.push(o);
                }
                DiffKind::Added => {
                    let n = row.new_line.expect("addition has new line");
                    assert!(row.old_line.is_none());
                    assert_eq!(new[n - 1], row.text);
                    new_seen.push(n);
                }
            }
        }
        old_seen.sort_unstable();
        new_seen.sort_unstable();
        assert_eq!(old_seen, (1..=old.len()).collect::<Vec<_>>());
        assert_eq!(new_seen, (1..=new.len()).collect::<Vec<_>>());
    }

    #[test]
    fn identical() {
        let old = lines(&["a", "b", "c"]);
        let rows = diff_lines(&old, &old);
        assert_eq!(
            render(&rows),
            vec![
                (DiffKind::Context, Some(1), Some(1), "a".into()),
                (DiffKind::Context, Some(2), Some(2), "b".into()),
                (DiffKind::Context, Some(3), Some(3), "c".into()),
            ]
        );
        assert_eq!(changed_count(&rows), 0);
        assert_well_formed(&old, &old, &rows);
    }

    #[test]
    fn insert_one_line() {
        let old = lines(&["a", "c"]);
        let new = lines(&["a", "b", "c"]);
        let rows = diff_lines(&old, &new);
        assert_eq!(
            render(&rows),
            vec![
                (DiffKind::Context, Some(1), Some(1), "a".into()),
                (DiffKind::Added, None, Some(2), "b".into()),
                (DiffKind::Context, Some(2), Some(3), "c".into()),
            ]
        );
        assert_eq!(changed_count(&rows), 1);
        assert_well_formed(&old, &new, &rows);
    }

    #[test]
    fn remove_one_line() {
        let old = lines(&["a", "b", "c"]);
        let new = lines(&["a", "c"]);
        let rows = diff_lines(&old, &new);
        assert_eq!(
            render(&rows),
            vec![
                (DiffKind::Context, Some(1), Some(1), "a".into()),
                (DiffKind::Removed, Some(2), None, "b".into()),
                (DiffKind::Context, Some(3), Some(2), "c".into()),
            ]
        );
        assert_eq!(changed_count(&rows), 1);
        assert_well_formed(&old, &new, &rows);
    }

    #[test]
    fn replace_one_line_removes_before_adds() {
        let old = lines(&["a", "b", "c"]);
        let new = lines(&["a", "B", "c"]);
        let rows = diff_lines(&old, &new);
        assert_eq!(
            render(&rows),
            vec![
                (DiffKind::Context, Some(1), Some(1), "a".into()),
                (DiffKind::Removed, Some(2), None, "b".into()),
                (DiffKind::Added, None, Some(2), "B".into()),
                (DiffKind::Context, Some(3), Some(3), "c".into()),
            ]
        );
        assert_eq!(changed_count(&rows), 2);
        assert_well_formed(&old, &new, &rows);
    }

    #[test]
    fn add_whole_file() {
        let old: Vec<String> = Vec::new();
        let new = lines(&["x", "y"]);
        let rows = diff_lines(&old, &new);
        assert_eq!(
            render(&rows),
            vec![
                (DiffKind::Added, None, Some(1), "x".into()),
                (DiffKind::Added, None, Some(2), "y".into()),
            ]
        );
        assert_eq!(changed_count(&rows), 2);
        assert_well_formed(&old, &new, &rows);
    }

    #[test]
    fn delete_whole_file() {
        let old = lines(&["x", "y"]);
        let new: Vec<String> = Vec::new();
        let rows = diff_lines(&old, &new);
        assert_eq!(
            render(&rows),
            vec![
                (DiffKind::Removed, Some(1), None, "x".into()),
                (DiffKind::Removed, Some(2), None, "y".into()),
            ]
        );
        assert_eq!(changed_count(&rows), 2);
        assert_well_formed(&old, &new, &rows);
    }

    #[test]
    fn both_empty() {
        let rows = diff_lines(&[], &[]);
        assert!(rows.is_empty());
        assert_eq!(changed_count(&rows), 0);
    }

    #[test]
    fn multiple_hunks() {
        let old = lines(&[
            "one", "two", "three", "four", "five", "six", "seven", "eight",
        ]);
        let new = lines(&[
            "one", "TWO", "three", "four", "five", "six", "SEVEN", "eight", "nine",
        ]);
        let rows = diff_lines(&old, &new);
        assert_eq!(
            render(&rows),
            vec![
                (DiffKind::Context, Some(1), Some(1), "one".into()),
                (DiffKind::Removed, Some(2), None, "two".into()),
                (DiffKind::Added, None, Some(2), "TWO".into()),
                (DiffKind::Context, Some(3), Some(3), "three".into()),
                (DiffKind::Context, Some(4), Some(4), "four".into()),
                (DiffKind::Context, Some(5), Some(5), "five".into()),
                (DiffKind::Context, Some(6), Some(6), "six".into()),
                (DiffKind::Removed, Some(7), None, "seven".into()),
                (DiffKind::Added, None, Some(7), "SEVEN".into()),
                (DiffKind::Context, Some(8), Some(8), "eight".into()),
                (DiffKind::Added, None, Some(9), "nine".into()),
            ]
        );
        assert_eq!(changed_count(&rows), 5);
        assert_well_formed(&old, &new, &rows);
    }

    #[test]
    fn duplicate_whitespace_lines_align_deterministically() {
        let old = lines(&["", "a", "", "b", ""]);
        let new = lines(&["", "a", "", "", "b", ""]);
        let rows = diff_lines(&old, &new);
        assert_well_formed(&old, &new, &rows);
        assert_eq!(changed_count(&rows), 1);
        assert!(rows.iter().any(|row| {
            row.kind == DiffKind::Added && row.new_line == Some(4) && row.text.is_empty()
        }));
        // Running twice must yield the same alignment.
        assert_eq!(rows, diff_lines(&old, &new));
    }

    #[test]
    fn five_thousand_identical_lines_complete() {
        let old: Vec<String> = (0..5_000).map(|i| format!("line {i}")).collect();
        let rows = diff_lines(&old, &old);
        assert_eq!(rows.len(), 5_000);
        assert!(rows.iter().all(|row| row.kind == DiffKind::Context));
        assert_eq!(changed_count(&rows), 0);
    }

    #[test]
    fn five_thousand_lines_one_change_completes() {
        let old: Vec<String> = (0..5_000).map(|i| format!("line {i}")).collect();
        let mut new = old.clone();
        new[2_500] = "changed line".to_string();
        let rows = diff_lines(&old, &new);
        assert_eq!(rows.len(), 5_001);
        assert_eq!(changed_count(&rows), 2);
        assert_well_formed(&old, &new, &rows);
    }

    #[test]
    fn unrelated_sequences_stay_bounded() {
        let old: Vec<String> = (0..1_000).map(|i| format!("old {i}")).collect();
        let new: Vec<String> = (0..1_000).map(|i| format!("new {i}")).collect();
        let rows = diff_lines(&old, &new);
        assert_eq!(changed_count(&rows), 2_000);
        assert_well_formed(&old, &new, &rows);
    }
}
