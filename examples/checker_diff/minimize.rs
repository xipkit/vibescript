//! Reduces a finding to a small program that still shows it.

use super::harness::{self, Case, Finding, Scratch, Verdict};

/// What a reduced program must still show: the same kind of finding, and
/// for a failed check, the same message, which names the check.
fn signature(finding: &Finding) -> String {
    let checked = finding
        .detail
        .lines()
        .find_map(|line| line.strip_prefix("checked: "))
        .unwrap_or_default();
    // Positions move as lines go.
    let message = match checked.find("): ").or_else(|| checked.find(": ")) {
        Some(index) if finding.kind == harness::FindingKind::CheckFailed => &checked[index..],
        _ => "",
    };
    format!("{} {message}", finding.kind.name())
}

fn same(verdict: &Verdict, wanted: &str) -> bool {
    matches!(verdict, Verdict::Finding(finding) if signature(finding) == wanted)
}

/// Removes lines, and blocks of lines, from every file of `case` while it
/// still shows `finding`, until no single removal keeps it.
pub fn minimize(case: &Case, finding: &Finding, scratch: &mut Scratch) -> Case {
    let wanted = signature(finding);
    let mut best = case.clone();
    loop {
        let before = size(&best);
        for file in 0..=best.modules.len() {
            best = reduce_file(best, file, &wanted, scratch);
        }
        best = drop_modules(best, &wanted, scratch);
        if size(&best) >= before {
            return best;
        }
    }
}

fn size(case: &Case) -> usize {
    case.main.len()
        + case
            .modules
            .iter()
            .map(|(_, source)| source.len())
            .sum::<usize>()
}

fn text(case: &Case, file: usize) -> &str {
    if file == case.modules.len() {
        &case.main
    } else {
        &case.modules[file].1
    }
}

fn with_text(case: &Case, file: usize, text: String) -> Case {
    let mut next = case.clone();
    if file == next.modules.len() {
        next.main = text;
    } else {
        next.modules[file].1 = text;
    }
    next
}

fn drop_modules(case: Case, wanted: &str, scratch: &mut Scratch) -> Case {
    let mut best = case;
    let mut index = 0;
    while index < best.modules.len() {
        let mut next = best.clone();
        next.modules.remove(index);
        if same(&harness::judge(&next, scratch), wanted) {
            best = next;
        } else {
            index += 1;
        }
    }
    best
}

/// The end of the block that line `start` opens: the following lines
/// indented deeper, and a closing line at its own indentation.
fn block_end(lines: &[&str], start: usize) -> usize {
    let indent = |line: &str| line.len() - line.trim_start().len();
    let depth = indent(lines[start]);
    let mut end = start + 1;
    while end < lines.len() && (lines[end].trim().is_empty() || indent(lines[end]) > depth) {
        end += 1;
    }
    if end < lines.len() {
        let closing = lines[end].trim();
        if closing == "end" || closing.starts_with('}') || closing.starts_with("end)") {
            end += 1;
        }
    }
    end
}

fn reduce_file(case: Case, file: usize, wanted: &str, scratch: &mut Scratch) -> Case {
    let mut best = case;
    // Whole blocks first, then chunks of lines of halving size.
    let mut index = 0;
    loop {
        let source = text(&best, file).to_owned();
        let lines: Vec<&str> = source.lines().collect();
        if index >= lines.len() {
            break;
        }
        let end = block_end(&lines, index);
        let mut kept: Vec<&str> = lines[..index].to_vec();
        kept.extend_from_slice(&lines[end..]);
        let candidate = with_text(&best, file, kept.join("\n") + "\n");
        if end > index && same(&harness::judge(&candidate, scratch), wanted) {
            best = candidate;
        } else {
            index += 1;
        }
    }
    let mut chunk = text(&best, file).lines().count().max(1);
    while chunk >= 1 {
        let mut start = 0;
        loop {
            let source = text(&best, file).to_owned();
            let lines: Vec<&str> = source.lines().collect();
            if start >= lines.len() {
                break;
            }
            let end = (start + chunk).min(lines.len());
            let mut kept: Vec<&str> = lines[..start].to_vec();
            kept.extend_from_slice(&lines[end..]);
            let candidate = with_text(&best, file, kept.join("\n") + "\n");
            if same(&harness::judge(&candidate, scratch), wanted) {
                best = candidate;
            } else {
                start += chunk;
            }
        }
        chunk /= 2;
    }
    best
}
