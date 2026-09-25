//! Unified diffs of a migration, as `vibes migrate` prints them.

/// Lines of context around each change.
const CONTEXT: usize = 3;

/// Renders the change from `old` to `new` as a unified diff labelled `path`,
/// or an empty string when they are equal.
///
/// ```
/// let diff = vibescript_tools::migrate::unified_diff("a.vibe", "x = 1\ny = 2\n", "x = 1\ny = 3\n");
/// assert_eq!(diff, "--- a/a.vibe\n+++ b/a.vibe\n@@ -1,2 +1,2 @@\n x = 1\n-y = 2\n+y = 3\n");
/// ```
pub fn unified_diff(path: &str, old: &str, new: &str) -> String {
    if old == new {
        return String::new();
    }
    let a: Vec<&str> = old.split_inclusive('\n').collect();
    let b: Vec<&str> = new.split_inclusive('\n').collect();
    let script = edit_script(&a, &b);
    // An absolute path is labelled as it is, a relative one as git does.
    let mut out = if path.starts_with('/') {
        format!("--- {path}\n+++ {path}\n")
    } else {
        format!("--- a/{path}\n+++ b/{path}\n")
    };
    // Group changes into hunks with their context.
    let mut index = 0;
    while index < script.len() {
        if matches!(script[index], Op::Keep(..)) {
            index += 1;
            continue;
        }
        let start = index.saturating_sub(CONTEXT);
        let mut end = index;
        let mut quiet = 0;
        while end < script.len() {
            if matches!(script[end], Op::Keep(..)) {
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
        let (old_start, new_start) = position(&script[..start]);
        let (old_len, new_len) = position(&script[start..end]);
        out.push_str(&format!(
            "@@ -{},{old_len} +{},{new_len} @@\n",
            old_start + usize::from(old_len > 0),
            new_start + usize::from(new_len > 0)
        ));
        for op in &script[start..end] {
            let (mark, line) = match op {
                Op::Keep(i, _) => (' ', a[*i]),
                Op::Delete(i) => ('-', a[*i]),
                Op::Insert(j) => ('+', b[*j]),
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

/// How many old and new lines a run of operations covers.
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
    let suffix = a[prefix..]
        .iter()
        .rev()
        .zip(b[prefix..].iter().rev())
        .take_while(|(x, y)| x == y)
        .count();
    let (a_mid, b_mid) = (&a[prefix..a.len() - suffix], &b[prefix..b.len() - suffix]);
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

fn myers(a: &[&str], b: &[&str]) -> Vec<Op> {
    let (n, m) = (a.len() as isize, b.len() as isize);
    let max = (n + m) as usize;
    let offset = max as isize + 1;
    let mut v = vec![0isize; 2 * max + 3];
    let mut trace = Vec::new();
    'outer: for d in 0..=max as isize {
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let index = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
                v[index + 1]
            } else {
                v[index - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[index] = x;
            if x >= n && y >= m {
                break 'outer;
            }
            k += 2;
        }
    }
    let mut ops = Vec::new();
    let (mut x, mut y) = (n, m);
    for d in (0..trace.len() as isize).rev() {
        let v = &trace[d as usize];
        let k = x - y;
        let index = (k + offset) as usize;
        let previous_k = if k == -d || (k != d && v[index - 1] < v[index + 1]) {
            k + 1
        } else {
            k - 1
        };
        let previous_x = v[(previous_k + offset) as usize];
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
}
