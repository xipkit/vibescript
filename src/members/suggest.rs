//! "Did you mean" suffixes for lookup failures, ranked like the reference:
//! case-only mismatches first, then names within a small edit distance.

use crate::scan;
use std::fmt;

/// Candidates within this many edits are suggested.
const MAX_DISTANCE: usize = 2;
/// Names of at most this many runes accept only one edit.
const SHORT_NAME_RUNES: usize = 4;
/// Only this many leading candidates are considered.
const MAX_CANDIDATES: usize = 256;
const MAX_RESULTS: usize = 3;

/// Collects the closest candidates to a missing name, keeping owned copies of
/// at most three, and renders ` (did you mean "x"?)` or nothing.
pub(crate) struct Suggestion<'n> {
    name: &'n str,
    limit: usize,
    offered: usize,
    best: [(usize, Vec<u8>); MAX_RESULTS],
    len: usize,
}

impl<'n> Suggestion<'n> {
    pub(crate) fn new(name: &'n str) -> Self {
        let limit = if name.chars().count() <= SHORT_NAME_RUNES {
            1
        } else {
            MAX_DISTANCE
        };
        Self {
            name,
            limit,
            offered: 0,
            best: Default::default(),
            len: 0,
        }
    }

    /// Ranks more candidates. Only the first `MAX_CANDIDATES` offered count,
    /// and each costs a band around the diagonal of the edit-distance table.
    pub(crate) fn offer<'c>(
        &mut self,
        candidates: impl IntoIterator<Item = &'c [u8]>,
    ) -> &mut Self {
        for candidate in candidates {
            if self.offered == MAX_CANDIDATES {
                break;
            }
            self.offered += 1;
            // A repeated candidate ranks the same as its first copy, so skipping
            // it among the kept results considers each name once.
            if self.name.is_empty()
                || candidate.is_empty()
                || candidate == self.name.as_bytes()
                || self.best[..self.len]
                    .iter()
                    .any(|(_, kept)| kept == candidate)
            {
                continue;
            }
            let Some(rank) = rank(self.name.as_bytes(), candidate, self.limit) else {
                continue;
            };
            let position = self.best[..self.len]
                .partition_point(|(other, kept)| (*other, kept.as_slice()) <= (rank, candidate));
            if position == MAX_RESULTS {
                continue;
            }
            if self.len < MAX_RESULTS {
                self.len += 1;
            }
            self.best[position..self.len].rotate_right(1);
            self.best[position] = (rank, candidate.to_vec());
        }
        self
    }
}

/// Ranks `candidates` against the missing `name`.
pub(crate) fn did_you_mean<'c>(
    name: &str,
    candidates: impl IntoIterator<Item = &'c [u8]>,
) -> Suggestion<'_> {
    let mut suggestion = Suggestion::new(name);
    suggestion.offer(candidates);
    suggestion
}

fn runes(bytes: &[u8]) -> impl Iterator<Item = char> + '_ {
    let mut rest = bytes;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let (rune, width, _) = scan::rune(rest);
        rest = &rest[width..];
        Some(rune)
    })
}

/// Case-only mismatches rank zero; anything else ranks by its edit distance.
fn rank(name: &[u8], candidate: &[u8], limit: usize) -> Option<usize> {
    let fold = |rune| crate::text::case::fold(rune);
    if runes(name).map(fold).eq(runes(candidate).map(fold)) {
        return Some(0);
    }
    distance(name, candidate, limit)
}

/// The Levenshtein distance over runes when it is at most `limit`. Cells more
/// than `limit` off the diagonal can never lead back within the limit, so only
/// that band is computed, reading each string once.
fn distance(a: &[u8], b: &[u8], limit: usize) -> Option<usize> {
    let (a_len, b_len) = (runes(a).count(), runes(b).count());
    if a_len.abs_diff(b_len) > limit {
        return None;
    }
    let (short, long, short_len, long_len) = if a_len <= b_len {
        (a, b, a_len, b_len)
    } else {
        (b, a, b_len, a_len)
    };
    const BAND: usize = 2 * MAX_DISTANCE + 1;
    let unreachable = limit + 1;
    // Row j keeps column i at band offset i + limit - j.
    let at = |row: &[usize; BAND], j: usize, i: usize| match (i + limit).checked_sub(j) {
        Some(offset) if offset <= 2 * limit => row[offset],
        _ => unreachable,
    };
    let mut previous = [unreachable; BAND];
    for i in 0..=short_len.min(limit) {
        previous[i + limit] = i;
    }
    // The band reads short runes up to `limit` behind and ahead of the row.
    let mut pending = runes(short);
    let mut window = ['\0'; BAND];
    let mut read = 0;
    for (index, long_rune) in runes(long).enumerate() {
        let j = index + 1;
        let mut current = [unreachable; BAND];
        let mut row_min = unreachable;
        for i in j.saturating_sub(limit)..=(j + limit).min(short_len) {
            let value = if i == 0 {
                j
            } else {
                while read < i {
                    window[read % BAND] = pending.next().unwrap();
                    read += 1;
                }
                let cost = usize::from(window[(i - 1) % BAND] != long_rune);
                (at(&previous, j - 1, i) + 1)
                    .min(at(&current, j, i - 1) + 1)
                    .min(at(&previous, j - 1, i - 1) + cost)
                    .min(unreachable)
            };
            current[i + limit - j] = value;
            row_min = row_min.min(value);
        }
        if row_min > limit {
            return None;
        }
        previous = current;
    }
    let result = at(&previous, long_len, short_len);
    (result <= limit).then_some(result)
}

impl fmt::Display for Suggestion<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Some(((_, last), rest)) = self.best[..self.len].split_last() else {
            return Ok(());
        };
        f.write_str(" (did you mean ")?;
        match rest {
            [] => {}
            [(_, only)] => {
                quote(f, only)?;
                f.write_str(" or ")?;
            }
            _ => {
                for (_, name) in rest {
                    quote(f, name)?;
                    f.write_str(", ")?;
                }
                f.write_str("or ")?;
            }
        }
        quote(f, last)?;
        f.write_str("?)")
    }
}

/// Quotes a name the way Go's `%q` does for the printable names seen here,
/// escaping quotes, backslashes, control characters and invalid bytes.
fn quote(f: &mut fmt::Formatter<'_>, bytes: &[u8]) -> fmt::Result {
    f.write_str("\"")?;
    let mut rest = bytes;
    while !rest.is_empty() {
        let (rune, width, valid) = scan::rune(rest);
        if !valid {
            write!(f, "\\x{:02x}", rest[0])?;
        } else {
            match rune {
                '"' => f.write_str("\\\"")?,
                '\\' => f.write_str("\\\\")?,
                '\u{7}' => f.write_str("\\a")?,
                '\u{8}' => f.write_str("\\b")?,
                '\u{c}' => f.write_str("\\f")?,
                '\n' => f.write_str("\\n")?,
                '\r' => f.write_str("\\r")?,
                '\t' => f.write_str("\\t")?,
                '\u{b}' => f.write_str("\\v")?,
                rune if rune.is_control() => {
                    if (rune as u32) < 0x80 {
                        write!(f, "\\x{:02x}", rune as u32)?;
                    } else if (rune as u32) < 0x10000 {
                        write!(f, "\\u{:04x}", rune as u32)?;
                    } else {
                        write!(f, "\\U{:08x}", rune as u32)?;
                    }
                }
                rune => write!(f, "{rune}")?,
            }
        }
        rest = &rest[width..];
    }
    f.write_str("\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn suggest(name: &str, candidates: &[&str]) -> String {
        did_you_mean(name, candidates.iter().map(|c| c.as_bytes())).to_string()
    }

    #[test]
    fn ranks_case_matches_then_edits_then_names() {
        assert_eq!(
            suggest("lengt", &["length", "size"]),
            " (did you mean \"length\"?)"
        );
        assert_eq!(
            suggest("uppcase", &["upcase", "upcase!", "downcase"]),
            " (did you mean \"upcase\" or \"upcase!\"?)"
        );
        assert_eq!(
            suggest("uniq!", &["uniq", "union", "sum"]),
            " (did you mean \"uniq\" or \"union\"?)"
        );
        assert_eq!(
            suggest("LENGTH", &["length"]),
            " (did you mean \"length\"?)"
        );
        assert_eq!(
            suggest(
                "abcdef",
                &["abcdeg", "abcdxy", "ABCDEF", "abcdez", "abcdey"]
            ),
            " (did you mean \"ABCDEF\", \"abcdeg\", or \"abcdey\"?)"
        );
        assert_eq!(
            suggest("to_s", &["to_a", "to_h", "size"]),
            " (did you mean \"to_a\" or \"to_h\"?)"
        );
        assert_eq!(suggest("zzzzzz", &["length"]), "");
        assert_eq!(suggest("", &["a"]), "");
        assert_eq!(suggest("same", &["same"]), "");
    }

    #[test]
    fn short_names_accept_one_edit_and_long_names_two() {
        assert_eq!(suggest("tims", &["times"]), " (did you mean \"times\"?)");
        assert_eq!(suggest("abz", &["abs"]), " (did you mean \"abs\"?)");
        assert_eq!(suggest("ab", &["abcd"]), "");
        assert_eq!(
            suggest("lengtt", &["length"]),
            " (did you mean \"length\"?)"
        );
        assert_eq!(
            suggest("lenxxh", &["length"]),
            " (did you mean \"length\"?)"
        );
        assert_eq!(suggest("lxnxxh", &["length"]), "");
        assert_eq!(
            suggest("id2nam", &["id2name"]),
            " (did you mean \"id2name\"?)"
        );
        assert_eq!(suggest("abcdefgh", &["xbcdefghij"]), "");
        assert_eq!(
            suggest("abcdefgh", &["bcdefghi"]),
            " (did you mean \"bcdefghi\"?)"
        );
    }

    #[test]
    fn candidates_beyond_the_cap_and_duplicates_are_ignored() {
        let mut many = vec!["filler"; MAX_CANDIDATES];
        many.push("length");
        assert_eq!(suggest("lengt", &many), "");
        assert_eq!(
            suggest("lengt", &["length", "length"]),
            " (did you mean \"length\"?)"
        );
    }

    #[test]
    fn distance_matches_the_full_table_within_the_limit() {
        fn full(a: &str, b: &str) -> usize {
            let a: Vec<char> = a.chars().collect();
            let b: Vec<char> = b.chars().collect();
            let mut row: Vec<usize> = (0..=a.len()).collect();
            for (j, cb) in b.iter().enumerate() {
                let mut next = vec![j + 1; a.len() + 1];
                for (i, ca) in a.iter().enumerate() {
                    next[i + 1] = (row[i + 1] + 1)
                        .min(next[i] + 1)
                        .min(row[i] + usize::from(ca != cb));
                }
                row = next;
            }
            row[a.len()]
        }
        let words = [
            "", "a", "ab", "ba", "abc", "acb", "xyz", "abcd", "abdc", "length", "lengtt", "lenght",
            "size", "sizes", "héllo", "hello", "ab√", "tap",
        ];
        for a in words {
            for b in words {
                for limit in 1..=MAX_DISTANCE {
                    let expected = Some(full(a, b)).filter(|&d| d <= limit);
                    assert_eq!(
                        distance(a.as_bytes(), b.as_bytes(), limit),
                        expected,
                        "{a} {b} {limit}"
                    );
                }
            }
        }
    }

    #[test]
    fn quoting_escapes_like_go() {
        assert_eq!(suggest("ab\"", &["ab\"c"]), " (did you mean \"ab\\\"c\"?)");
        assert_eq!(suggest("a\nb", &["a\nbc"]), " (did you mean \"a\\nbc\"?)");
    }
}
