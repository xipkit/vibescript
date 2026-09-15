// Copyright 2010 The Go Authors. All rights reserved.
// Pattern matching is adapted from Go 1.27.1 src/path/match.go.
// The original BSD license is reproduced in licenses/Go-BSD-3-Clause.txt.

use super::{argument, clean, trimmed};
use crate::{CallContext, Error, ErrorKind, Result, budget::Buffer};

pub(crate) struct Policy {
    allow: Vec<Pattern>,
    deny: Vec<Pattern>,
}

impl Policy {
    pub fn new(allow: &[String], deny: &[String]) -> Result<Self> {
        fn compile(inputs: &[String]) -> Result<Vec<Pattern>> {
            inputs
                .iter()
                .map(|input| {
                    if trimmed(input.as_bytes()).is_empty() {
                        return Err(argument("module policy pattern cannot be empty"));
                    }
                    if trimmed(input.as_bytes()) != input.as_bytes() {
                        return Err(argument(
                            "module policy pattern has ambiguous edge whitespace",
                        ));
                    }
                    let mut name = Vec::with_capacity(input.len().max(1));
                    normalize(input.as_bytes(), &mut name);
                    if name.is_empty() {
                        return Err(argument("module policy pattern cannot be empty"));
                    }
                    let mut pattern = Pattern::parse(&name)?;
                    pattern.universal = name == b"*";
                    Ok(pattern)
                })
                .collect()
        }
        Ok(Self {
            allow: compile(allow)?,
            deny: compile(deny)?,
        })
    }

    pub fn check(&self, ctx: &mut CallContext, name: &[u8]) -> Result<()> {
        ctx.charge((name.len() as u64).saturating_add(1))?;
        let mut normalized = Buffer::with_capacity(ctx, name.len().max(1))?;
        normalize(name, &mut normalized.data);
        if normalized.data.is_empty() {
            return if self.allow.is_empty() && self.deny.is_empty() {
                Ok(())
            } else {
                Err(argument("module name is invalid"))
            };
        }
        for pattern in &self.deny {
            if pattern.matches(ctx, &normalized.data)? {
                return Err(Error::new(
                    ErrorKind::Runtime,
                    "require: module denied by policy",
                ));
            }
        }
        if self.allow.is_empty() {
            return Ok(());
        }
        for pattern in &self.allow {
            if pattern.matches(ctx, &normalized.data)? {
                return Ok(());
            }
        }
        Err(Error::new(
            ErrorKind::Runtime,
            "require: module not allowed by policy",
        ))
    }
}

fn normalize(input: &[u8], output: &mut Vec<u8>) {
    clean(input, output);
    if output == b"." {
        output.clear();
        return;
    }
    let base = output.rsplit(|&b| b == b'/').next().unwrap();
    if let Some(stem) = base.strip_suffix(b".vibe") {
        if !stem.is_empty() && !stem.contains(&b'.') {
            output.truncate(output.len() - 5);
        }
    }
}

enum Token {
    Byte(u8),
    Rune,
    Star,
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

struct Pattern {
    tokens: Vec<Token>,
    universal: bool,
}

impl Pattern {
    fn parse(input: &[u8]) -> Result<Self> {
        let mut tokens = Vec::new();
        let mut at = 0;
        while at < input.len() {
            let byte = input[at];
            at += 1;
            let token = match byte {
                b'*' => {
                    if matches!(tokens.last(), Some(Token::Star)) {
                        continue;
                    }
                    Token::Star
                }
                b'?' => Token::Rune,
                b'\\' => {
                    let byte = *input.get(at).ok_or_else(bad_pattern)?;
                    at += 1;
                    Token::Byte(byte)
                }
                b'[' => {
                    let negated = input.get(at) == Some(&b'^');
                    at += usize::from(negated);
                    let mut ranges = Vec::new();
                    loop {
                        if input.get(at) == Some(&b']') && !ranges.is_empty() {
                            at += 1;
                            break;
                        }
                        let low = class_rune(input, &mut at)?;
                        let high = if input.get(at) == Some(&b'-') {
                            at += 1;
                            class_rune(input, &mut at)?
                        } else {
                            low
                        };
                        ranges.push((low, high));
                    }
                    Token::Class { negated, ranges }
                }
                _ => Token::Byte(byte),
            };
            tokens.push(token);
        }
        Ok(Self {
            tokens,
            universal: false,
        })
    }

    fn matches(&self, ctx: &mut CallContext, input: &[u8]) -> Result<bool> {
        ctx.charge(1)?;
        if self.universal {
            return Ok(!input.is_empty());
        }
        let mut start = 0;
        let mut position = 0;
        while start < self.tokens.len() {
            ctx.charge(1)?;
            let star = matches!(self.tokens[start], Token::Star);
            start += usize::from(star);
            let mut end = start;
            while end < self.tokens.len() && !matches!(self.tokens[end], Token::Star) {
                ctx.charge(1)?;
                end += 1;
            }
            if start == end && star {
                for &byte in &input[position..] {
                    ctx.charge(1)?;
                    if byte == b'/' {
                        return Ok(false);
                    }
                }
                return Ok(true);
            }
            loop {
                ctx.charge(1)?;
                if let Some(next) = self.chunk(ctx, start..end, input, position)? {
                    if end < self.tokens.len() || next == input.len() {
                        position = next;
                        break;
                    }
                }
                if !star || position == input.len() || input[position] == b'/' {
                    return Ok(false);
                }
                // Go searches each byte offset, including inside UTF-8 sequences.
                position += 1;
            }
            // A matched chunk is committed; later failures do not retry it.
            start = end;
        }
        Ok(position == input.len())
    }

    fn chunk(
        &self,
        ctx: &mut CallContext,
        range: std::ops::Range<usize>,
        input: &[u8],
        mut at: usize,
    ) -> Result<Option<usize>> {
        for token in &self.tokens[range] {
            ctx.charge(1)?;
            if at == input.len() {
                return Ok(None);
            }
            let size = match token {
                Token::Byte(byte) => (input[at] == *byte).then_some(1),
                Token::Rune => (input[at] != b'/').then(|| crate::scan::rune(&input[at..]).1),
                Token::Class { negated, ranges } => {
                    let (rune, size, _) = crate::scan::rune(&input[at..]);
                    let mut matched = false;
                    for &(low, high) in ranges {
                        ctx.charge(1)?;
                        matched |= low <= rune && rune <= high;
                    }
                    (matched != *negated).then_some(size)
                }
                Token::Star => unreachable!(),
            };
            let Some(size) = size else { return Ok(None) };
            at += size;
        }
        Ok(Some(at))
    }
}

fn bad_pattern() -> Error {
    argument("invalid module policy pattern")
}

fn class_rune(input: &[u8], at: &mut usize) -> Result<char> {
    let byte = *input.get(*at).ok_or_else(bad_pattern)?;
    if matches!(byte, b'-' | b']') {
        return Err(bad_pattern());
    }
    if byte == b'\\' {
        *at += 1;
    }
    if *at >= input.len() {
        return Err(bad_pattern());
    }
    let (rune, width, valid) = crate::scan::rune(&input[*at..]);
    *at += width;
    if !valid || *at == input.len() {
        return Err(bad_pattern());
    }
    Ok(rune)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, CancellationToken, Limits};

    fn decode_hex(value: &serde_json::Value) -> Vec<u8> {
        let text = value.as_str().unwrap().as_bytes();
        assert_eq!(text.len() % 2, 0);
        text.chunks_exact(2)
            .map(|pair| {
                let digit = |b: u8| (b as char).to_digit(16).unwrap() as u8;
                digit(pair[0]) * 16 + digit(pair[1])
            })
            .collect()
    }

    #[test]
    fn glob_reference_observations_match_go() {
        let cases: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/module-loading/glob-reference.json"
        ))
        .unwrap();
        let cases = cases.as_array().unwrap();
        assert_eq!(cases.len(), 8852);
        let mut differences = Vec::new();
        for (index, case) in cases.iter().enumerate() {
            let pattern = decode_hex(&case["pattern_hex"]);
            let name = decode_hex(&case["name_hex"]);
            let actual = Pattern::parse(&pattern);
            let invalid = actual.is_err();
            let matched = match actual {
                Ok(pattern) => pattern
                    .matches(&mut CallContext::new(CallOptions::default()), &name)
                    .unwrap(),
                Err(_) => false,
            };
            if invalid != case["invalid"].as_bool().unwrap()
                || matched != case["matched"].as_bool().unwrap()
            {
                differences.push(format!(
                    "{index}: {pattern:?} / {name:?}: got match={matched}, invalid={invalid}, want {case}"
                ));
            }
        }
        assert!(
            differences.is_empty(),
            "{} differences: {:?}",
            differences.len(),
            &differences[..differences.len().min(20)]
        );
    }

    #[test]
    fn policy_reference_observations_match_go() {
        let cases: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/module-loading/policy-reference.json"
        ))
        .unwrap();
        let cases = cases.as_array().unwrap();
        assert_eq!(cases.len(), 1004);
        for case in cases {
            let strings = |key: &str| {
                case[key]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|value| value.as_str().unwrap().to_owned())
                    .collect::<Vec<_>>()
            };
            let policy = Policy::new(&strings("allow"), &strings("deny"));
            assert_eq!(
                policy.is_err(),
                case["invalid"].as_bool().unwrap(),
                "{case}"
            );
            let Ok(policy) = policy else { continue };
            let input = decode_hex(&case["name_hex"]);
            let mut ctx = CallContext::new(CallOptions::default());
            let name = crate::loading::Name::parse(&mut ctx, &input).unwrap();
            let result = policy.check(&mut ctx, name.normalized.as_bytes().unwrap());
            if let Err(error) = &result {
                assert_eq!(error.kind, ErrorKind::Runtime, "{case}: {error}");
            }
            assert_eq!(result.is_ok(), case["allowed"].as_bool().unwrap(), "{case}");
        }
    }

    fn check(allow: &[&str], deny: &[&str], name: &[u8]) -> bool {
        let policy = Policy::new(
            &allow.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
            &deny.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap();
        policy
            .check(&mut CallContext::new(CallOptions::default()), name)
            .is_ok()
    }

    #[test]
    fn policy_preserves_extensions_whitespace_and_deny_precedence() {
        assert!(check(&["*"], &[], b"nested/tool.vibe"));
        assert!(check(&["*.vibe"], &[], b"nested/tool.vibe"));
        assert!(!check(&["nested/*"], &[], b"nested/deeper/tool.vibe"));
        assert!(check(&["*"], &["tool"], b"tool.vibe.vibe"));
        assert!(!check(&["*"], &["tool"], b"tool.vibe"));
        assert!(!check(&["*"], &["tool.vibe.vibe"], b"tool.vibe.vibe"));
        assert!(check(&["*"], &[".vibe"], b".vibe.vibe"));
        assert!(!check(&["*"], &[".vibe"], b".vibe"));
        assert!(!check(&["*"], &["pkg/..vibe"], b"pkg/..vibe"));
        assert!(check(&["*"], &["pkg/..vibe"], b"pkg.vibe"));
        assert!(check(&["./tool /.vibe"], &[], b"tool /.vibe"));
        assert!(check(&["./ tool /."], &[], b" tool .vibe"));
        assert!(!check(&["tool"], &[], b"tool .vibe"));
        assert!(check(&["*"], &[], b"\xff.vibe"));
        assert!(!check(&["*"], &[], b"."));
        assert!(check(&[], &[], b"."));
    }

    #[test]
    fn malformed_patterns_fail_even_when_the_prefix_would_not_match() {
        for pattern in [
            "", " ", ".", " ./tool ", "[", "a[", "[^]", "[-x]", "[x-]", "[x-y", "[x--y]",
            "pkg/[bad",
        ] {
            assert!(
                Policy::new(&[pattern.to_owned()], &[]).is_err(),
                "{pattern:?}"
            );
        }
        for pattern in [b"[".as_slice(), b"x[", b"[\\]", b"\\", b"[a-\\]", b"[\xff]"] {
            assert!(Pattern::parse(pattern).is_err(), "{pattern:?}");
        }
        assert!(Pattern::parse(b"[z-a]").is_ok());
    }

    #[test]
    fn glob_matching_preserves_byte_and_rune_rules() {
        let cases: &[(&[u8], &[u8], bool)] = &[
            (b"", b"", true),
            (b"*", b"nested/tool", false),
            (b"**", b"nested/tool", false),
            (b"a*b*c", b"axbyc", true),
            (b"a*b*c", b"axbycx", false),
            (b"*[^a]*b", b"b/b", false),
            (b"*[^a]*b", "é/b".as_bytes(), false),
            (b"[a-z]", b"q", true),
            (b"[z-a]", b"q", false),
            (b"[^x]", b"/", true),
            (b"[/]", b"/", true),
            (b"?", b"/", false),
            (b"?", "é".as_bytes(), true),
            (b"??", "é".as_bytes(), false),
            (b"*\xa9", "é".as_bytes(), true),
            (b"*?", b"\xff", true),
            (b"[[]", b"[", true),
            (b"[\\]]", b"]", true),
            (b"\\*", b"*", true),
            ("[é-ê]".as_bytes(), "ê".as_bytes(), true),
        ];
        for (pattern, input, expected) in cases {
            let mut ctx = CallContext::new(CallOptions::default());
            assert_eq!(
                Pattern::parse(pattern)
                    .unwrap()
                    .matches(&mut ctx, input)
                    .unwrap(),
                *expected,
                "{pattern:?} / {input:?}"
            );
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn match_work_memory_and_cancellation_are_bounded() {
        let pattern = Pattern::parse(b"*a*a*a*a*a*a*a*a*b").unwrap();
        let input = vec![b'a'; 4096];
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(2048),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            pattern.matches(&mut ctx, &input).unwrap_err().kind,
            ErrorKind::Steps
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(1024),
                steps: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            Policy::new(&["*".to_owned()], &[])
                .unwrap()
                .check(&mut ctx, &input)
                .unwrap_err()
                .kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let mut ctx = CallContext::new(CallOptions {
            cancellation,
            ..CallOptions::default()
        });
        assert_eq!(
            pattern.matches(&mut ctx, b"a").unwrap_err().kind,
            ErrorKind::Cancelled
        );
    }
}
