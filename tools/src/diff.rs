//! Unified diffs of a rewrite, as `vibes fix --dry-run` prints them.

const CONTEXT: usize = 3;

/// Renders the change from `old` to `new` as a unified diff labelled `path`,
/// or an empty string when they are equal.
///
/// ```
/// let diff = vibescript_tools::diff::unified_diff("a.vibe", "x = 1\ny = 2\n", "x = 1\ny = 3\n");
/// assert_eq!(diff, "--- a/a.vibe\n+++ b/a.vibe\n@@ -1,2 +1,2 @@\n x = 1\n-y = 2\n+y = 3\n");
/// ```
pub fn unified_diff(path: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let script = edit_script(&a, &b);
    let mut out = if path.starts_with('/') {
        format!("--- {path}\n+++ {path}\n")
    } else {
        format!("--- a/{path}\n+++ b/{path}\n")
    };
    let mut index = 0;
    while let Some(op) = script.get(index) {
        if matches!(op, Op::Keep(..)) {
            index += 1;
            continue;
        }
        let start = index.saturating_sub(CONTEXT);
        let mut end = index;
        let mut quiet = 0;
        while let Some(op) = script.get(end) {
            if matches!(op, Op::Keep(..)) {
                if quiet == CONTEXT * 2 {
                    break;
                }
                quiet += 1;
            } else {
                quiet = 0;
            }
            end += 1;
        }
        let end = (end - quiet.saturating_sub(CONTEXT)).min(script.len());
        let (before, rest) = script.split_at(start);
        let (hunk, _) = rest.split_at(end - start);
        let (old_start, new_start) = position(before);
        let (old_len, new_len) = position(hunk);
        out.push_str(&format!(
            "@@ -{},{old_len} +{},{new_len} @@\n",
            old_start + usize::from(old_len > 0),
            new_start + usize::from(new_len > 0)
        ));
        for op in hunk {
            let (mark, line) = match op {
                Op::Keep(i, _) => (' ', a.get(*i)),
                Op::Delete(i) => ('-', a.get(*i)),
                Op::Insert(j) => ('+', b.get(*j)),
            };
            let Some(line) = line else {
                continue;
            };
            out.push(mark);
            out.push_str(line);
            if !line.ends_with('\n') {
                out.push_str("\n\\ No newline at end of file\n");
            }
        }
        index = end;
    }
    out
}

#[derive(Clone, Copy)]
enum Op {
    Keep(usize, usize),
    Delete(usize),
    Insert(usize),
}

fn position(ops: &[Op]) -> (usize, usize) {
    ops.iter().fold((0, 0), |(a, b), op| match op {
        Op::Keep(..) => (a + 1, b + 1),
        Op::Delete(_) => (a + 1, b),
        Op::Insert(_) => (a, b + 1),
    })
}

/// A shortest edit script by Myers' algorithm, with common ends trimmed first.
fn edit_script(a: &[&str], b: &[&str]) -> Vec<Op> {
    let prefix = a.iter().zip(b).take_while(|(x, y)| x == y).count();
    let (_, a_tail) = a.split_at(prefix);
    let (_, b_tail) = b.split_at(prefix);
    let suffix = a_tail
        .iter()
        .rev()
        .zip(b_tail.iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a_mid, _) = a_tail.split_at(a_tail.len() - suffix);
    let (b_mid, _) = b_tail.split_at(b_tail.len() - suffix);
    let mut ops: Vec<Op> = (0..prefix).map(|i| Op::Keep(i, i)).collect();
    for op in myers(a_mid, b_mid) {
        ops.push(match op {
            Op::Keep(i, j) => Op::Keep(i + prefix, j + prefix),
            Op::Delete(i) => Op::Delete(i + prefix),
            Op::Insert(j) => Op::Insert(j + prefix),
        });
    }
    for k in 0..suffix {
        ops.push(Op::Keep(a.len() - suffix + k, b.len() - suffix + k));
    }
    ops
}

/// A length as a signed diagonal; no slice is longer than `isize::MAX`.
fn signed(length: usize) -> isize {
    isize::try_from(length).unwrap_or(isize::MAX)
}

fn myers(a: &[&str], b: &[&str]) -> Vec<Op> {
    let (n, m) = (signed(a.len()), signed(b.len()));
    let max = a.len() + b.len();
    let offset = signed(max) + 1;
    let mut v = vec![0isize; 2 * max + 3];
    let mut trace = Vec::new();
    'outer: for d in 0..=signed(max) {
        // Only diagonals within this depth can be used when backtracking.
        trace.push(
            v.get((offset - d) as usize..=(offset + d) as usize)
                .unwrap_or_default()
                .to_vec(),
        );
        let mut k = -d;
        while k <= d {
            let left = v.get((offset + k - 1) as usize).copied().unwrap_or(0);
            let right = v.get((offset + k + 1) as usize).copied().unwrap_or(0);
            let mut x = if k == -d || (k != d && left < right) {
                right
            } else {
                left + 1
            };
            let mut y = x - k;
            while x < n
                && y < m
                && a.get(x as usize)
                    .zip(b.get(y as usize))
                    .is_some_and(|(a, b)| a == b)
            {
                x += 1;
                y += 1;
            }
            if let Some(slot) = v.get_mut((offset + k) as usize) {
                *slot = x;
            }
            if x >= n && y >= m {
                break 'outer;
            }
            k += 2;
        }
    }
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for (d, v) in trace.iter().enumerate().rev() {
        let d = signed(d);
        let k = x - y;
        let left = v.get((d + k - 1) as usize).copied().unwrap_or(0);
        let right = v.get((d + k + 1) as usize).copied().unwrap_or(0);
        let previous_k = if k == -d || (k != d && left < right) {
            k + 1
        } else {
            k - 1
        };
        let previous_x = v.get((d + previous_k) as usize).copied().unwrap_or(0);
        let previous_y = previous_x - previous_k;
        while x > previous_x && y > previous_y {
            x -= 1;
            y -= 1;
            ops.push(Op::Keep(x as usize, y as usize));
        }
        if d > 0 {
            if x == previous_x {
                ops.push(Op::Insert(previous_y as usize));
            } else {
                ops.push(Op::Delete(previous_x as usize));
            }
        }
        x = previous_x;
        y = previous_y;
    }
    ops.reverse();
    ops
}

#[cfg(test)]
mod tests {
    use super::unified_diff;

    #[test]
    fn hunks_carry_three_lines_of_context() {
        let old: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        let new = old.replace("line 5\n", "five\n").replace("line 18\n", "");
        let diff = unified_diff("f.vibe", &old, &new);
        assert_eq!(
            diff,
            "--- a/f.vibe\n+++ b/f.vibe\n\
             @@ -2,7 +2,7 @@\n line 2\n line 3\n line 4\n-line 5\n+five\n line 6\n line 7\n line 8\n\
             @@ -15,6 +15,5 @@\n line 15\n line 16\n line 17\n-line 18\n line 19\n line 20\n"
        );
        assert_eq!(unified_diff("f.vibe", &old, &old), "");
    }

    #[test]
    fn empty_inputs_and_unterminated_lines_keep_their_markers() {
        assert_eq!(unified_diff("f.vibe", "", ""), "");
        assert_eq!(
            unified_diff("f.vibe", "", "x"),
            "--- a/f.vibe\n+++ b/f.vibe\n@@ -0,0 +1,1 @@\n+x\n\\ No newline at end of file\n"
        );
        assert_eq!(
            unified_diff("f.vibe", "x", ""),
            "--- a/f.vibe\n+++ b/f.vibe\n@@ -1,1 +0,0 @@\n-x\n\\ No newline at end of file\n"
        );
    }

    #[test]
    fn repeated_lines_keep_the_same_shortest_path() {
        assert_eq!(
            unified_diff("f.vibe", "a\nb\na\n", "b\na\nb\n"),
            "--- a/f.vibe\n+++ b/f.vibe\n@@ -1,3 +1,3 @@\n-a\n b\n a\n+b\n"
        );
    }

    #[test]
    fn dense_replacements_keep_deletions_before_insertions() {
        let old: String = (0..128).map(|i| format!("old {i}\n")).collect();
        let new: String = (0..128).map(|i| format!("new {i}\n")).collect();
        let mut expected = "--- a/f.vibe\n+++ b/f.vibe\n@@ -1,128 +1,128 @@\n".to_owned();
        for line in old.split_inclusive('\n') {
            expected.push('-');
            expected.push_str(line);
        }
        for line in new.split_inclusive('\n') {
            expected.push('+');
            expected.push_str(line);
        }
        assert_eq!(unified_diff("f.vibe", &old, &new), expected);
    }

    #[test]
    fn short_scripts_reconstruct_both_inputs_with_minimal_edits() {
        let mut inputs = vec![Vec::new()];
        for len in 1..=5 {
            for bits in 0..1 << len {
                inputs.push(
                    (0..len)
                        .map(|i| if bits & (1 << i) == 0 { "a" } else { "b" })
                        .collect(),
                );
            }
        }
        for a in &inputs {
            for b in &inputs {
                let mut old = Vec::new();
                let mut new = Vec::new();
                let mut edits = 0;
                for op in super::edit_script(a, b) {
                    match op {
                        super::Op::Keep(i, j) => {
                            assert_eq!(a[i], b[j]);
                            old.push(a[i]);
                            new.push(b[j]);
                        }
                        super::Op::Delete(i) => {
                            old.push(a[i]);
                            edits += 1;
                        }
                        super::Op::Insert(j) => {
                            new.push(b[j]);
                            edits += 1;
                        }
                    }
                }
                assert_eq!(&old, a);
                assert_eq!(&new, b);
                // Independent dynamic programming oracle for the edit distance.
                let mut distances: Vec<_> = (0..=b.len()).collect();
                for (i, left) in a.iter().enumerate() {
                    let mut diagonal = distances[0];
                    distances[0] = i + 1;
                    for (j, right) in b.iter().enumerate() {
                        let above = distances[j + 1];
                        distances[j + 1] = if left == right {
                            diagonal
                        } else {
                            1 + above.min(distances[j])
                        };
                        diagonal = above;
                    }
                }
                assert_eq!(edits, distances[b.len()], "{a:?} -> {b:?}");
            }
        }
    }
}
