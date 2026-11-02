//! "Did you mean" suffixes for lookup failures, ranked like the reference:
//! case-only mismatches first, then names within a small edit distance.

use crate::{CallContext, Result, scan};
use std::fmt;

/// Candidates within this many edits are suggested.
const MAX_DISTANCE: usize = 2;
/// Names of at most this many runes accept only one edit.
const SHORT_NAME_RUNES: usize = 4;
/// Only this many leading candidates are considered.
const MAX_CANDIDATES: usize = 256;
const MAX_RESULTS: usize = 3;
/// Longer missing names, which only a host can supply, get no suggestion.
const MAX_NAME_BYTES: usize = 256;
/// Ranking considers no further candidates once they could take it past
/// reading this many bytes in all.
const MAX_INPUT_BYTES: usize = 64 << 10;
/// The most bytes one rune takes in UTF-8.
const MAX_RUNE_BYTES: usize = 4;

/// Collects the closest candidates to a missing name, keeping owned copies of
/// at most three, and renders ` (did you mean "x"?)` or nothing.
pub(crate) struct Suggestion<'n> {
    name: &'n str,
    /// The name's case-folded runes, empty when it is too long to rank.
    folded: Vec<u32>,
    limit: usize,
    offered: usize,
    read: usize,
    pending: usize,
    best: [(usize, Vec<u8>); MAX_RESULTS],
    len: usize,
}

impl<'n> Suggestion<'n> {
    pub(crate) fn new(name: &'n str) -> Self {
        let folded: Vec<u32> = if name.len() <= MAX_NAME_BYTES {
            runes(name.as_bytes())
                .map(crate::text::case::fold)
                .collect()
        } else {
            Vec::new()
        };
        let limit = if folded.len() <= SHORT_NAME_RUNES {
            1
        } else {
            MAX_DISTANCE
        };
        Self {
            name,
            folded,
            limit,
            offered: 0,
            read: 0,
            pending: 0,
            best: Default::default(),
            len: 0,
        }
    }

    /// Ranks more candidates. Only the first `MAX_CANDIDATES` offered count,
    /// and each costs a band around the diagonal of the edit-distance table.
    /// Each candidate checks for cancellation, and every chunk read is
    /// charged; a lookup reading less than a chunk stays free.
    pub(crate) fn offer<'c>(
        &mut self,
        ctx: &mut CallContext,
        candidates: impl IntoIterator<Item = &'c [u8]>,
    ) -> Result<&mut Self> {
        if self.folded.is_empty() {
            return Ok(self);
        }
        // A candidate further than the limit from the name's rune count can
        // never rank, so ranking reads no more of it than this.
        let reach = MAX_RUNE_BYTES * (self.folded.len() + self.limit + 1);
        for candidate in candidates {
            if self.offered == MAX_CANDIDATES {
                break;
            }
            let read = self.name.len() + candidate.len().min(reach);
            if self.read + read > MAX_INPUT_BYTES {
                self.offered = MAX_CANDIDATES;
                break;
            }
            ctx.checkpoint()?;
            ctx.scan_bytes(&mut self.pending, read)?;
            self.read += read;
            self.offered += 1;
            // A repeated candidate ranks the same as its first copy, so skipping
            // it among the kept results considers each name once.
            if candidate.is_empty()
                || candidate == self.name.as_bytes()
                || self.best[..self.len]
                    .iter()
                    .any(|(_, kept)| kept == candidate)
            {
                continue;
            }
            let Some(rank) = self.rank(candidate) else {
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
        Ok(self)
    }

    /// Case-only mismatches rank zero; anything else ranks by its edit
    /// distance.
    fn rank(&self, candidate: &[u8]) -> Option<usize> {
        let fold = crate::text::case::fold;
        if self.folded.iter().copied().eq(runes(candidate).map(fold)) {
            return Some(0);
        }
        let runes_in_reach = runes(candidate)
            .take(self.folded.len() + self.limit + 1)
            .count();
        distance(
            self.name.as_bytes(),
            self.folded.len(),
            candidate,
            runes_in_reach,
            self.limit,
        )
    }
}

/// Ranks `candidates` against the missing `name`.
pub(crate) fn did_you_mean<'n, 'c>(
    ctx: &mut CallContext,
    name: &'n str,
    candidates: impl IntoIterator<Item = &'c [u8]>,
) -> Result<Suggestion<'n>> {
    let mut suggestion = Suggestion::new(name);
    suggestion.offer(ctx, candidates)?;
    Ok(suggestion)
}

/// The error a host call or check reports for a function the script does not
/// declare, suggesting close declared names as the reference does. A ranking
/// that runs out of budget or is cancelled reports that instead.
pub(crate) fn missing_function(
    ctx: &mut CallContext,
    program: &crate::bytecode::Program,
    name: &str,
) -> crate::Error {
    let suggestion = match did_you_mean(ctx, name, program.names.keys().map(String::as_bytes)) {
        Ok(suggestion) => suggestion,
        Err(error) => return error,
    };
    crate::Error::new(
        crate::ErrorKind::Name,
        format!("function {name} not found{suggestion}"),
    )
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

/// The Levenshtein distance over runes when it is at most `limit`, given each
/// string's rune count; a count may stop short once it is more than `limit`
/// past the other. Cells more than `limit` off the diagonal can never lead
/// back within the limit, so only that band is computed, reading each string
/// once.
fn distance(a: &[u8], a_len: usize, b: &[u8], b_len: usize, limit: usize) -> Option<usize> {
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
        let mut ctx = CallContext::new(crate::CallOptions::default());
        did_you_mean(&mut ctx, name, candidates.iter().map(|c| c.as_bytes()))
            .unwrap()
            .to_string()
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
    fn names_past_the_byte_cap_get_no_suggestion() {
        let at_cap = "a".repeat(MAX_NAME_BYTES);
        let near = "a".repeat(MAX_NAME_BYTES - 1);
        assert_eq!(
            suggest(&at_cap, &[&near]),
            format!(" (did you mean \"{near}\"?)")
        );
        let over = "A".repeat(MAX_NAME_BYTES + 1);
        assert_eq!(suggest(&over, &[&over.to_lowercase()]), "");
    }

    #[test]
    fn ranking_stops_once_it_could_read_past_the_input_cap() {
        let name = "n".repeat(64);
        let near = format!("{name}x");
        // A filler counts the name plus the reach a long candidate is read to,
        // and the near candidate counts both in full.
        let filler = "f".repeat(1000);
        let read = name.len() + MAX_RUNE_BYTES * (name.len() + MAX_DISTANCE + 1);
        let fillers = (MAX_INPUT_BYTES - name.len() - near.len()) / read;
        assert!(fillers + 1 < MAX_CANDIDATES);
        let mut candidates = vec![filler.as_str(); fillers];
        candidates.push(&near);
        assert_eq!(
            suggest(&name, &candidates),
            format!(" (did you mean \"{near}\"?)")
        );
        candidates.insert(0, &filler);
        assert_eq!(suggest(&name, &candidates), "");
    }

    #[test]
    fn cancellation_stops_ranking_between_candidates() {
        let token = crate::CancellationToken::new();
        let mut ctx = CallContext::new(crate::CallOptions {
            cancellation: token.clone(),
            ..Default::default()
        });
        let pulled = std::cell::Cell::new(0);
        let candidates = std::iter::repeat_n(&b"length"[..], MAX_CANDIDATES).inspect(|_| {
            pulled.set(pulled.get() + 1);
            if pulled.get() == 10 {
                token.cancel();
            }
        });
        let error = did_you_mean(&mut ctx, "lengt", candidates).err().unwrap();
        assert_eq!(error.kind, crate::ErrorKind::Cancelled);
        assert_eq!(pulled.get(), 10);
    }

    #[test]
    fn bulk_ranking_is_charged_and_small_lookups_stay_free() {
        let options = || crate::CallOptions {
            limits: crate::Limits {
                steps: Some(10),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut ctx = CallContext::new(options());
        let suggestion = did_you_mean(&mut ctx, "lengt", [&b"length"[..]]).unwrap();
        assert_eq!(suggestion.to_string(), " (did you mean \"length\"?)");
        assert_eq!(ctx.stats().steps, 0);
        let name = "n".repeat(200);
        let filler = "f".repeat(200);
        let candidates = std::iter::repeat_n(filler.as_bytes(), MAX_CANDIDATES);
        let mut ctx = CallContext::new(options());
        let error = did_you_mean(&mut ctx, &name, candidates).err().unwrap();
        assert_eq!(error.kind, crate::ErrorKind::Steps);
    }

    #[test]
    fn dynamic_member_failures_charge_ranking_against_long_keys() {
        // The checker refuses unknown members in scripts, but a host object's
        // keys are only known when a call dispatches on it.
        let name = "a_rather_long_member_name";
        let short: Vec<_> = (0..64).map(|i| format!("k{i}").into_bytes()).collect();
        let long = "k".repeat(1 << 16);
        let long: Vec<_> = (0..64).map(|i| format!("{i}{long}").into_bytes()).collect();
        let fail = |keys: &[Vec<u8>], steps| {
            let mut ctx = CallContext::new(crate::CallOptions {
                limits: crate::Limits {
                    steps,
                    ..Default::default()
                },
                ..Default::default()
            });
            let mut fields: Vec<_> = keys
                .iter()
                .map(|key| (key.clone(), crate::Value::int(1)))
                .collect();
            fields.push((b"a_rather_long_member_nam".to_vec(), crate::Value::int(2)));
            let site = crate::bytecode::CallSite {
                name: 0,
                method: None,
                auto: true,
                scope: false,
            };
            let error =
                crate::members::call(&mut ctx, site, name, crate::Value::object(fields), &[])
                    .err()
                    .unwrap();
            (error, ctx.stats().steps)
        };
        // Looking the name up costs the same steps either way; only ranking
        // the long keys reads enough to be charged.
        let (_, lookup) = fail(&short, None);
        let budget = Some(lookup + 10);
        let (short, _) = fail(&short, budget);
        assert_eq!(short.kind, crate::ErrorKind::Name);
        assert!(
            short
                .message
                .ends_with("(did you mean \"a_rather_long_member_nam\"?)"),
            "{}",
            short.message
        );
        let (long, _) = fail(&long, budget);
        assert_eq!(long.kind, crate::ErrorKind::Steps, "{}", long.message);
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
                        distance(
                            a.as_bytes(),
                            a.chars().count(),
                            b.as_bytes(),
                            b.chars().count(),
                            limit
                        ),
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
