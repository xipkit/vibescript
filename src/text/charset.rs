use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Buffer, scan, value::Kind};

fn argument(message: &str) -> Error {
    Error::new(ErrorKind::Argument, message)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Token {
    code: u32,
    raw: bool,
}

impl Token {
    fn read(bytes: &[u8], position: &mut usize) -> Self {
        let (rune, width, valid) = scan::rune(&bytes[*position..]);
        let code = if valid {
            rune as u32
        } else {
            u32::from(bytes[*position])
        };
        *position += width;
        Self { code, raw: !valid }
    }

    fn escaped(ctx: &mut CallContext, bytes: &[u8], position: &mut usize) -> Result<Self> {
        ctx.charge(1)?;
        let token = Self::read(bytes, position);
        if token.code == u32::from(b'\\') && !token.raw && *position < bytes.len() {
            ctx.charge(1)?;
            return Ok(Self::read(bytes, position));
        }
        Ok(token)
    }

    fn encode(self, scratch: &mut [u8; 4]) -> &[u8] {
        if self.raw {
            scratch[0] = self.code as u8;
            &scratch[..1]
        } else {
            char::from_u32(self.code)
                .unwrap_or('\u{fffd}')
                .encode_utf8(scratch)
                .as_bytes()
        }
    }
}

#[derive(Clone, Copy)]
struct Span {
    low: u32,
    high: u32,
    raw: bool,
}

impl Span {
    fn length(self) -> u64 {
        u64::from(self.high - self.low) + 1
    }

    fn contains(self, token: Token) -> bool {
        self.raw == token.raw && (self.low..=self.high).contains(&token.code)
    }
}

struct Set {
    spans: Buffer<Span>,
    length: u64,
    complement: bool,
}

impl Set {
    fn parse(ctx: &mut CallContext, bytes: &[u8], complement: bool) -> Result<Self> {
        let complement = complement && bytes.len() > 1 && bytes[0] == b'^';
        let mut position = usize::from(complement);
        let mut set = Self {
            spans: Buffer::empty(),
            length: 0,
            complement,
        };
        while position < bytes.len() {
            let start = Token::escaped(ctx, bytes, &mut position)?;
            let end = if position + 1 < bytes.len() && bytes[position] == b'-' {
                ctx.charge(1)?;
                position += 1;
                Token::escaped(ctx, bytes, &mut position)?
            } else {
                start
            };
            if start.raw != end.raw {
                return Err(argument("invalid mixed byte and Unicode character range"));
            }
            if start.code > end.code {
                return Err(argument("character range endpoints are reversed"));
            }
            let span = Span {
                low: start.code,
                high: end.code,
                raw: start.raw,
            };
            set.length = set.length.saturating_add(span.length());
            set.spans.push(ctx, span)?;
        }
        Ok(set)
    }

    fn matches(&self, ctx: &mut CallContext, token: Token) -> Result<bool> {
        for span in &self.spans.data {
            ctx.charge(1)?;
            if span.contains(token) {
                return Ok(!self.complement);
            }
        }
        Ok(self.complement)
    }

    fn index(&self, ctx: &mut CallContext, token: Token) -> Result<Option<u64>> {
        if self.complement {
            return Ok(self.matches(ctx, token)?.then_some(u64::MAX));
        }
        let mut base = 0_u64;
        let mut found = None;
        for span in &self.spans.data {
            ctx.charge(1)?;
            if span.contains(token) {
                found = Some(base.saturating_add(u64::from(token.code - span.low)));
            }
            base = base.saturating_add(span.length());
        }
        Ok(found)
    }

    fn at(&self, ctx: &mut CallContext, index: u64) -> Result<Option<Token>> {
        if self.length == 0 {
            return Ok(None);
        }
        let mut index = index.min(self.length - 1);
        for span in &self.spans.data {
            ctx.charge(1)?;
            if index < span.length() {
                return Ok(Some(Token {
                    code: span.low + index as u32,
                    raw: span.raw,
                }));
            }
            index -= span.length();
        }
        unreachable!()
    }
}

fn matches(ctx: &mut CallContext, sets: &[Set], token: Token) -> Result<bool> {
    for set in sets {
        ctx.charge(1)?;
        if !set.matches(ctx, token)? {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Clone, Copy)]
enum Operation {
    Count,
    Delete,
    Translate,
    Squeeze,
}

fn walk(
    ctx: &mut CallContext,
    operation: Operation,
    sets: &[Set],
    bytes: &[u8],
    mut emit: impl FnMut(&mut CallContext, &[u8]) -> Result<()>,
) -> Result<usize> {
    let mut position = 0;
    let mut previous = None;
    let mut count = 0;
    while position < bytes.len() {
        ctx.charge(1)?;
        let token = Token::read(bytes, &mut position);
        let output = match operation {
            Operation::Count => {
                count += usize::from(matches(ctx, sets, token)?);
                None
            }
            Operation::Delete => (!matches(ctx, sets, token)?).then_some(token),
            Operation::Translate => {
                if let Some(index) = sets[0].index(ctx, token)? {
                    sets[1].at(ctx, index)?
                } else {
                    Some(token)
                }
            }
            Operation::Squeeze => {
                let repeated = previous == Some(token) && matches(ctx, sets, token)?;
                previous = Some(token);
                (!repeated).then_some(token)
            }
        };
        if let Some(token) = output {
            emit(ctx, token.encode(&mut [0; 4]))?;
        }
    }
    Ok(count)
}

pub(crate) fn call(
    ctx: &mut CallContext,
    name: &str,
    receiver: &Value,
    args: &[Value],
    keywords: bool,
    block: bool,
) -> Result<Option<Value>> {
    let Kind::Bytes(bytes) = &receiver.0 else {
        return Ok(None);
    };
    let operation = match name {
        "count" => Operation::Count,
        "delete" | "delete!" => Operation::Delete,
        "tr" | "tr!" => Operation::Translate,
        "squeeze" | "squeeze!" => Operation::Squeeze,
        _ => return Ok(None),
    };
    ctx.charge(1)?;
    if keywords || block {
        return Err(argument(
            "character-set methods do not accept keyword arguments or blocks",
        ));
    }
    match operation {
        Operation::Count | Operation::Delete if args.is_empty() => {
            return Err(argument(&format!(
                "string.{name} expects at least one character set"
            )));
        }
        Operation::Translate if args.len() != 2 => {
            return Err(argument(&format!(
                "string.{name} expects source and replacement character sets"
            )));
        }
        _ => {}
    }
    let mut sets = Buffer::with_capacity(ctx, args.len())?;
    for (index, arg) in args.iter().enumerate() {
        ctx.charge(1)?;
        let Kind::Bytes(bytes) = &arg.0 else {
            return Err(argument("character set must be a string"));
        };
        sets.data.push(Set::parse(
            ctx,
            &bytes.data,
            !matches!(operation, Operation::Translate) || index == 0,
        )?);
    }
    if matches!(operation, Operation::Count) {
        let count = walk(ctx, operation, &sets.data, &bytes.data, |_, _| Ok(()))?;
        return Ok(Some(Value::int(count as i64)));
    }
    let mut length = 0_usize;
    let mut changed = false;
    walk(ctx, operation, &sets.data, &bytes.data, |ctx, piece| {
        let Some(end) = length.checked_add(piece.len()) else {
            return ctx.fail(ErrorKind::Memory, "character-set output size overflow");
        };
        changed |= bytes.data.get(length..end) != Some(piece);
        length = end;
        Ok(())
    })?;
    let bang = name.ends_with('!');
    if !changed && length == bytes.data.len() {
        return Ok(Some(if bang { Value::nil() } else { receiver.clone() }));
    }
    let mut output = Buffer::with_capacity(ctx, length)?;
    walk(ctx, operation, &sets.data, &bytes.data, |ctx, piece| {
        output.extend(ctx, piece)
    })?;
    Value::from_bytes(ctx, output).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    #[test]
    fn wide_ranges_keep_one_span_instead_of_expanding_codepoints() {
        let mut ctx = CallContext::new(CallOptions::default());
        let set = Set::parse(&mut ctx, "\0-\u{10ffff}".as_bytes(), false).unwrap();
        assert_eq!(set.length, 0x110000);
        assert_eq!(set.spans.data.len(), 1);
        assert!(ctx.stats().peak_memory_bytes < 256);
        assert_eq!(
            set.at(&mut ctx, 0xd800)
                .unwrap()
                .unwrap()
                .encode(&mut [0; 4]),
            "�".as_bytes()
        );
        assert_eq!(set.at(&mut ctx, u64::MAX).unwrap().unwrap().code, 0x10ffff);
        drop(set);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn unchanged_results_reuse_input_storage_and_release_set_scratch() {
        for (method, args) in [
            ("count", vec![Value::bytes("z")]),
            ("delete", vec![Value::bytes("z")]),
            ("delete!", vec![Value::bytes("z")]),
            ("tr", vec![Value::bytes("^a"), Value::bytes("x")]),
            ("tr!", vec![Value::bytes("a"), Value::bytes("a")]),
            ("squeeze", vec![Value::bytes("z")]),
            ("squeeze!", vec![Value::bytes("z")]),
        ] {
            let mut ctx = CallContext::new(CallOptions::default());
            let input = ctx.import(&Value::bytes(vec![b'a'; 32768])).unwrap();
            let before = ctx.stats().retained_memory_bytes;
            let result = call(&mut ctx, method, &input, &args, false, false)
                .unwrap()
                .unwrap();
            assert_eq!(ctx.stats().retained_memory_bytes, before, "{method}");
            assert!(ctx.stats().peak_memory_bytes < before + 2048, "{method}");
            if method.ends_with('!') {
                assert!(matches!(result.0, Kind::Nil));
            }
            drop(input);
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                if method == "count" || method.ends_with('!') {
                    0
                } else {
                    before
                }
            );
            drop(result);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn parser_and_expansion_failures_release_scratch_and_latch_exhaustion() {
        for (limits, expected) in [
            (
                Limits {
                    steps: Some(64),
                    ..Limits::default()
                },
                ErrorKind::Steps,
            ),
            (
                Limits {
                    memory_bytes: Some(1024),
                    ..Limits::default()
                },
                ErrorKind::Memory,
            ),
        ] {
            let mut ctx = CallContext::new(CallOptions {
                limits,
                ..CallOptions::default()
            });
            let args = [Value::bytes("a".repeat(8192))];
            let error =
                call(&mut ctx, "count", &Value::bytes(""), &args, false, false).unwrap_err();
            assert_eq!(error.kind, expected);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.charge(0).unwrap_err().kind, expected);
        }
        let mut ctx = CallContext::new(CallOptions::default());
        let args = [
            Value::bytes("x".repeat(1024)),
            Value::bytes(format!("{}z-a", "a".repeat(1024))),
        ];
        let error = call(&mut ctx, "delete", &Value::bytes(""), &args, false, false).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        ctx.charge(1).unwrap();

        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(32768 + 1024),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let input = ctx.import(&Value::bytes(vec![b'a'; 32768])).unwrap();
        let before = ctx.stats().retained_memory_bytes;
        let args = [Value::bytes("a"), Value::bytes("🙂")];
        let error = call(&mut ctx, "tr", &input, &args, false, false).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(ctx.stats().retained_memory_bytes, before);
        assert!(ctx.stats().peak_memory_bytes < before + 1024);
        assert_eq!(ctx.charge(0).unwrap_err().kind, ErrorKind::Memory);
        drop(input);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn both_passes_charge_comparisons_and_interruption_releases_output() {
        let text = Value::bytes(format!("{}a", "z".repeat(512)));
        let set: String = (0..128)
            .map(|i| char::from_u32(0x3000 + i).unwrap())
            .chain(['z'])
            .collect();
        let set = Value::bytes(set);
        let mut ctx = CallContext::new(CallOptions::default());
        let result = call(
            &mut ctx,
            "count",
            &text,
            std::slice::from_ref(&set),
            false,
            false,
        )
        .unwrap()
        .unwrap();
        assert_eq!(result.as_int(), Some(512));
        let count_steps = ctx.stats().steps;
        for (method, args) in [
            ("delete", vec![set.clone()]),
            ("tr", vec![set.clone(), Value::bytes("X")]),
            ("squeeze", vec![set.clone()]),
        ] {
            let mut complete = CallContext::new(CallOptions::default());
            let value = call(&mut complete, method, &text, &args, false, false)
                .unwrap()
                .unwrap();
            assert!(complete.stats().steps > count_steps * 3 / 2, "{method}");
            drop(value);
            assert_eq!(complete.stats().retained_memory_bytes, 0);

            let mut limited = CallContext::new(CallOptions {
                limits: Limits {
                    steps: Some(count_steps * 3 / 2),
                    ..Limits::default()
                },
                ..CallOptions::default()
            });
            let error = call(&mut limited, method, &text, &args, false, false).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Steps, "{method}");
            assert_eq!(limited.stats().retained_memory_bytes, 0);
            assert_eq!(limited.charge(0).unwrap_err().kind, ErrorKind::Steps);
        }
    }

    #[test]
    fn cancelled_and_expired_calls_stop_before_parsing_even_empty_inputs() {
        for source in ["", "aa"] {
            for method in [
                "count", "delete", "delete!", "tr", "tr!", "squeeze", "squeeze!",
            ] {
                for kind in [ErrorKind::Cancelled, ErrorKind::Deadline] {
                    let mut options = CallOptions::default();
                    if kind == ErrorKind::Deadline {
                        options.deadline = Some(std::time::Instant::now());
                    } else {
                        options.cancellation.cancel();
                    }
                    let mut ctx = CallContext::new(options);
                    let args = [Value::bytes("a"), Value::bytes("b")];
                    let args = if method.starts_with("tr") {
                        &args[..]
                    } else {
                        &args[..1]
                    };
                    let error = call(&mut ctx, method, &Value::bytes(source), args, false, false)
                        .unwrap_err();
                    assert_eq!(error.kind, kind);
                    assert_eq!(ctx.stats().peak_memory_bytes, 0);
                    assert_eq!(ctx.charge(0).unwrap_err().kind, kind);
                }
            }
        }
    }
}
