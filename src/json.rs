use crate::{CallContext, Error, ErrorKind, Result, Value, budget::CHUNK};
use std::fmt::{self, Write};

mod parser;
mod writer;

const MAX_PAYLOAD: usize = 1 << 20;

pub(crate) fn parse(ctx: &mut CallContext, input: &[u8]) -> Result<Value> {
    ctx.checkpoint()?;
    let mut p = parser::Parser::new(ctx, input);
    document(&mut p)
}

fn document(p: &mut parser::Parser<'_>) -> Result<Value> {
    let v = p.value()?;
    p.space()?;
    if !p.finished() {
        return p.err("trailing JSON data", parser::Failure::Trailing);
    }
    Ok(v)
}

/// Parses script input for the builtin `name` (`JSON.parse` or
/// `JSON.parse_as`), reporting failures in the reference's wording.
pub(crate) fn parse_builtin(ctx: &mut CallContext, input: &[u8], name: &str) -> Result<Value> {
    ctx.checkpoint()?;
    if input.len() > MAX_PAYLOAD {
        return ctx.guard(
            ErrorKind::OutputLimit,
            &format!("{name} input exceeds limit {MAX_PAYLOAD} bytes"),
        );
    }
    let mut p = parser::Parser::new(ctx, input);
    let result = document(&mut p);
    let failure = p.failure;
    let error = match result {
        Ok(value) => return Ok(value),
        Err(error) => error,
    };
    match failure {
        Some(failure)
            if error.kind == ErrorKind::Json
                || (failure == parser::Failure::Depth && error.kind == ErrorKind::Recursion) =>
        {
            struct Rendered<'a>(parser::Failure, &'a str, &'a [u8]);
            impl fmt::Display for Rendered<'_> {
                fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                    self.0.render(self.1, self.2, f)
                }
            }
            let rendered = Rendered(failure, name, input);
            let (message, _charge) = crate::source::formatted(ctx, format_args!("{rendered}"))?;
            Err(error.with_message(message))
        }
        _ => Err(error),
    }
}

pub(crate) fn parse_float(ctx: &mut CallContext, input: &[u8]) -> Result<f64> {
    const DIGITS: usize = 1100;
    if input.len() <= DIGITS {
        return std::str::from_utf8(input)
            .unwrap()
            .parse()
            .map_err(|_| Error::new(ErrorKind::Json, "invalid JSON number"));
    }
    // Binary64 rounding boundaries need at most 768 significant decimal digits.
    // Keep extra digits and a nonzero tail marker, bounding the library conversion.
    let mut text = [0u8; DIGITS + 80];
    let negative = input[0] == b'-';
    let mut used = usize::from(negative);
    text[0] = b'-';
    let mut at = used;
    let mut significant = 0usize;
    let mut kept = 0usize;
    let mut fractional = false;
    let mut fraction_digits = 0usize;
    let mut tail = false;
    while at < input.len() && !matches!(input[at], b'e' | b'E') {
        if (at - usize::from(negative)) % 1024 == 0 {
            ctx.work_bytes((input.len() - at).min(1024))?;
        }
        let byte = input[at];
        at += 1;
        if byte == b'.' {
            fractional = true;
            continue;
        }
        fraction_digits += usize::from(fractional);
        if significant != 0 || byte != b'0' {
            significant += 1;
            if kept < DIGITS {
                text[used] = byte;
                used += 1;
                kept += 1;
            } else {
                tail |= byte != b'0';
            }
        }
    }
    let mut exponent = 0i128;
    if at < input.len() {
        at += 1;
        let exponent_negative = input.get(at) == Some(&b'-');
        if matches!(input.get(at), Some(b'+' | b'-')) {
            at += 1;
        }
        while at < input.len() {
            if at % 1024 == 0 {
                ctx.work_bytes((input.len() - at).min(1024))?;
            }
            exponent = exponent
                .saturating_mul(10)
                .saturating_add((input[at] - b'0') as i128);
            at += 1;
        }
        if exponent_negative {
            exponent = -exponent;
        }
    }
    if significant == 0 {
        return Ok(if negative { -0.0 } else { 0.0 });
    }
    exponent = exponent
        .saturating_sub(fraction_digits as i128)
        .saturating_add((significant - kept) as i128);
    if tail {
        text[used] = b'1';
        used += 1;
        exponent = exponent.saturating_sub(1);
    }
    let mut suffix = Number::new();
    write!(suffix, "e{exponent}").unwrap();
    text[used..used + suffix.bytes().len()].copy_from_slice(suffix.bytes());
    used += suffix.bytes().len();
    std::str::from_utf8(&text[..used])
        .unwrap()
        .parse()
        .map_err(|_| Error::new(ErrorKind::Json, "invalid JSON number"))
}

pub(crate) fn bytes_equal(ctx: &mut CallContext, a: &[u8], b: &[u8]) -> Result<bool> {
    if a.len() != b.len() {
        return Ok(false);
    }
    for (a, b) in a.chunks(CHUNK).zip(b.chunks(CHUNK)) {
        ctx.work_bytes(a.len())?;
        if a != b {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(crate) fn stringify(ctx: &mut CallContext, value: &Value) -> Result<Value> {
    stringify_with_limit(ctx, value, None)
}

/// Encodes for the script builtin `JSON.stringify`, under its output limit and
/// with the reference's failure wording.
pub(crate) fn stringify_builtin(ctx: &mut CallContext, value: &Value) -> Result<Value> {
    stringify_with_limit(ctx, value, Some(MAX_PAYLOAD))
}

fn stringify_with_limit(
    ctx: &mut CallContext,
    value: &Value,
    limit: Option<usize>,
) -> Result<Value> {
    let mut out = writer::Output::new(limit, limit.is_some());
    writer::write_value(ctx, value, &mut out)?;
    Value::from_bytes(ctx, out.buffer)
}

pub(crate) struct Number {
    buf: [u8; 512],
    len: usize,
}
impl Number {
    pub fn new() -> Self {
        Self {
            buf: [0; 512],
            len: 0,
        }
    }
    pub fn bytes(&self) -> &[u8] {
        &self.buf[..self.len]
    }
}
impl Write for Number {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let end = self.len + s.len();
        if end > self.buf.len() {
            return Err(fmt::Error);
        }
        self.buf[self.len..end].copy_from_slice(s.as_bytes());
        self.len = end;
        Ok(())
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn builtin_size_guards_release_partial_output_and_allow_recovery() {
        let mut parse_ctx = CallContext::new(CallOptions::default());
        assert_eq!(
            parse_builtin(&mut parse_ctx, &vec![b'?'; MAX_PAYLOAD + 1], "JSON.parse")
                .unwrap_err()
                .kind,
            ErrorKind::OutputLimit
        );
        assert_eq!(parse_ctx.stats().steps, 0);
        assert_eq!(parse_ctx.stats().peak_memory_bytes, 0);
        assert_eq!(
            parse_builtin(&mut parse_ctx, b"7", "JSON.parse")
                .unwrap()
                .as_int(),
            Some(7)
        );
        assert_eq!(
            parse_ctx.bytes(b"x").unwrap().as_bytes(),
            Some(b"x".as_slice())
        );

        let mut output_ctx = CallContext::new(CallOptions::default());
        let input = Value::array(vec![Value::bytes(vec![b'a'; MAX_PAYLOAD - 2])]);
        assert_eq!(
            stringify_builtin(&mut output_ctx, &input).unwrap_err().kind,
            ErrorKind::OutputLimit
        );
        assert_eq!(output_ctx.stats().retained_memory_bytes, 0);
        assert_eq!(
            stringify_builtin(&mut output_ctx, &Value::nil())
                .unwrap()
                .as_bytes(),
            Some(b"null".as_slice())
        );
    }
}

#[cfg(test)]
mod depth_tests {
    use super::*;
    use crate::{CallOptions, ErrorClass, budget::MAX_VALUE_DEPTH};
    use std::time::Instant;

    const DEEP_MESSAGE: &str = "JSON nesting too deep";

    fn nested(open: &str, close: &str, depth: usize, innermost: &str) -> String {
        format!("{}{innermost}{}", open.repeat(depth), close.repeat(depth))
    }

    /// Alternates arrays and single-key objects from the outside in.
    fn mixed(depth: usize, innermost: &str) -> String {
        let mut text = String::new();
        for level in 0..depth {
            text.push_str(if level % 2 == 0 { "[" } else { "{\"k\":" });
        }
        text.push_str(innermost);
        for level in (0..depth).rev() {
            text.push(if level % 2 == 0 { ']' } else { '}' });
        }
        text
    }

    /// Texts containing exactly `depth` containers, each in canonical output form.
    fn texts(depth: usize) -> Vec<String> {
        vec![
            nested("[", "]", depth, "0"),
            nested("[", "]", depth - 1, "[]"),
            nested("{\"a\":", "}", depth, "true"),
            nested("{\"a\":", "}", depth - 1, "{}"),
            mixed(depth, "\"x\""),
            mixed(depth - 1, "[]"),
            mixed(depth - 1, "{}"),
        ]
    }

    /// Host values containing exactly `depth` containers around `innermost`.
    fn hosts(depth: usize, innermost: Value) -> Vec<Value> {
        let mut arrays = innermost.clone();
        let mut hashes = innermost.clone();
        let mut alternating = innermost;
        for level in 0..depth {
            arrays = Value::array(vec![arrays]);
            hashes = Value::hash(vec![(b"a".to_vec(), hashes)]);
            alternating = if level % 2 == 0 {
                Value::array(vec![alternating])
            } else {
                Value::hash(vec![(b"k".to_vec(), alternating)])
            };
        }
        vec![arrays, hashes, alternating]
    }

    fn context() -> CallContext {
        CallContext::new(CallOptions::default())
    }

    fn assert_deep_rejection(ctx: &mut CallContext, error: &Error) {
        assert_eq!(error.kind, ErrorKind::Recursion);
        assert_eq!(error.class(), Some(ErrorClass::Limit));
        assert_eq!(error.message, DEEP_MESSAGE);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        // Nesting guards are recoverable; execution quotas latch instead.
        assert!(!ctx.exhausted());
        ctx.checkpoint().unwrap();
    }

    #[test]
    fn parse_allows_exactly_the_maximum_depth_and_round_trips_it() {
        for text in texts(MAX_VALUE_DEPTH) {
            let mut ctx = context();
            let value = parse(&mut ctx, text.as_bytes()).unwrap();
            assert_eq!(value.depth(), MAX_VALUE_DEPTH, "{text}");
            let encoded = stringify(&mut ctx, &value).unwrap();
            assert_eq!(encoded.as_bytes(), Some(text.as_bytes()));
            let again = parse(&mut ctx, encoded.as_bytes().unwrap()).unwrap();
            assert_eq!(again.depth(), MAX_VALUE_DEPTH);
            drop((value, encoded, again));
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn parse_rejects_the_next_container_at_entry_even_when_empty() {
        for text in texts(MAX_VALUE_DEPTH + 1) {
            let mut ctx = context();
            let error = parse(&mut ctx, text.as_bytes()).unwrap_err();
            assert_deep_rejection(&mut ctx, &error);
        }
        // The rejection happens before anything inside the container is read:
        // malformed contents behind the opener are never reached, and the only
        // work charged is the per-opener cost. Array openers cost one step;
        // object openers also scan and copy a one-byte key.
        for (opener, steps_per_level) in [("[", 1), ("{\"k\":", 3)] {
            let expected_steps = steps_per_level * MAX_VALUE_DEPTH as u64 + 1;
            let text = format!("{}?", opener.repeat(MAX_VALUE_DEPTH + 1));
            let mut ctx = context();
            let error = parse(&mut ctx, text.as_bytes()).unwrap_err();
            assert_deep_rejection(&mut ctx, &error);
            assert_eq!(ctx.stats().steps, expected_steps);
            let text = format!("{}?", opener.repeat(MAX_VALUE_DEPTH));
            let mut ctx = context();
            let error = parse(&mut ctx, text.as_bytes()).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Json);
            assert_eq!(
                error.message,
                format!("expected JSON value at byte {}", text.len() - 1)
            );
            assert_eq!(ctx.stats().steps, expected_steps);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn stringify_allows_exactly_the_maximum_depth_and_rejects_the_next_at_entry() {
        for host in hosts(MAX_VALUE_DEPTH, Value::int(0)) {
            let mut ctx = context();
            let encoded = stringify(&mut ctx, &host).unwrap();
            let parsed = parse(&mut ctx, encoded.as_bytes().unwrap()).unwrap();
            assert_eq!(parsed.depth(), MAX_VALUE_DEPTH);
            let again = stringify(&mut ctx, &parsed).unwrap();
            assert_eq!(again.as_bytes(), encoded.as_bytes());
            drop((encoded, parsed, again));
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
        for innermost in [Value::int(0), Value::array(vec![]), Value::hash(vec![])] {
            let extra = usize::from(innermost.depth() > 0);
            for host in hosts(MAX_VALUE_DEPTH - extra, innermost.clone()) {
                let mut ctx = context();
                assert!(stringify(&mut ctx, &host).is_ok());
            }
            for host in hosts(MAX_VALUE_DEPTH + 1 - extra, innermost.clone()) {
                let mut ctx = context();
                let error = stringify(&mut ctx, &host).unwrap_err();
                assert_deep_rejection(&mut ctx, &error);
            }
        }
        // One step per container opener, and nothing after the rejected one.
        let mut ctx = context();
        let host = hosts(MAX_VALUE_DEPTH + 1, Value::bytes(vec![b'a'; CHUNK])).remove(0);
        let error = stringify(&mut ctx, &host).unwrap_err();
        assert_deep_rejection(&mut ctx, &error);
        assert_eq!(ctx.stats().steps, MAX_VALUE_DEPTH as u64 + 1);
    }

    #[test]
    fn malformed_input_after_a_complete_deep_sibling_is_recoverable() {
        let deep = nested("[", "]", MAX_VALUE_DEPTH - 1, "1");
        for (prefix, suffix, offset, message) in [
            ("[", ",?]", 1, "expected JSON value"),
            ("[", ",[", 2, "expected JSON value"),
            ("[", "", 0, "expected comma or closing bracket"),
            ("[", "]x", 1, "trailing JSON data"),
            ("{\"a\":", ",\"b\":?}", 5, "expected JSON value"),
            ("{\"a\":", ",1:2}", 1, "expected JSON object key"),
            ("{\"a\":", ",\"b\" 2}", 5, "expected colon"),
            ("{\"a\":", "", 0, "expected comma or closing brace"),
        ] {
            let text = format!("{prefix}{deep}{suffix}");
            let mut ctx = context();
            let error = parse(&mut ctx, text.as_bytes()).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Json, "{text}");
            assert_eq!(
                error.message,
                format!("{message} at byte {}", prefix.len() + deep.len() + offset)
            );
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            // Input errors do not latch: the same context keeps working.
            ctx.charge(1).unwrap();
            let value = parse(&mut ctx, format!("[{deep}]").as_bytes()).unwrap();
            assert_eq!(value.depth(), MAX_VALUE_DEPTH);
            drop(value);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    fn assert_exact_quota_boundaries(label: &str, run: &dyn Fn(&mut CallContext) -> Result<Value>) {
        let mut baseline = context();
        let value = run(&mut baseline).unwrap();
        let steps = baseline.stats().steps;
        let peak = baseline.stats().peak_memory_bytes;
        drop(value);
        assert_eq!(baseline.stats().retained_memory_bytes, 0, "{label}");
        assert!(steps > 0 && peak > 0, "{label}");

        let stride = (steps / 64).max(1);
        for limit in (0..steps).step_by(stride as usize).chain([steps - 1]) {
            let mut ctx = context();
            ctx.options.limits.steps = Some(limit);
            let error = run(&mut ctx).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Steps, "{label} steps {limit}");
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                0,
                "{label} steps {limit}"
            );
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        let mut ctx = context();
        ctx.options.limits.steps = Some(steps);
        let value = run(&mut ctx).unwrap();
        assert_eq!(ctx.stats().steps, steps, "{label}");
        drop(value);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);

        let stride = (peak / 64).max(1);
        for limit in (0..peak).step_by(stride).chain([peak - 1]) {
            let mut ctx = context();
            ctx.options.limits.memory_bytes = Some(limit);
            let error = run(&mut ctx).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Memory, "{label} memory {limit}");
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                0,
                "{label} memory {limit}"
            );
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
        let mut ctx = context();
        ctx.options.limits.memory_bytes = Some(peak);
        let value = run(&mut ctx).unwrap();
        assert_eq!(ctx.stats().peak_memory_bytes, peak, "{label}");
        drop(value);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn deep_parse_quota_failures_release_partial_trees_and_frames() {
        for text in texts(MAX_VALUE_DEPTH) {
            assert_exact_quota_boundaries(&text, &|ctx| parse(ctx, text.as_bytes()));
        }
        // A completed deep sibling is released when a later element exhausts the quota.
        let deep = nested("[", "]", MAX_VALUE_DEPTH - 2, "1");
        let text = format!("[{deep},[{deep}]]");
        assert_exact_quota_boundaries(&text, &|ctx| parse(ctx, text.as_bytes()));
    }

    #[test]
    fn deep_stringify_quota_failures_release_partial_output_and_frames() {
        for host in hosts(MAX_VALUE_DEPTH, Value::bytes(b"payload".to_vec())) {
            assert_exact_quota_boundaries("stringify", &|ctx| stringify(ctx, &host));
        }
        let mut ctx = context();
        let deep = hosts(MAX_VALUE_DEPTH - 2, Value::int(1)).remove(0);
        let host = Value::array(vec![deep.clone(), Value::array(vec![deep])]);
        let expected = stringify(&mut ctx, &host).unwrap();
        assert_exact_quota_boundaries("siblings", &|ctx| stringify(ctx, &host));
        let mut ctx = context();
        assert_eq!(
            parse(&mut ctx, expected.as_bytes().unwrap())
                .unwrap()
                .depth(),
            MAX_VALUE_DEPTH
        );
    }

    #[test]
    fn cancellation_and_deadlines_are_observed_before_deep_codec_work() {
        let text = nested("[", "]", MAX_VALUE_DEPTH, "0");
        let host = hosts(MAX_VALUE_DEPTH, Value::int(0)).remove(0);
        for deadline in [false, true] {
            for encode in [false, true] {
                let mut ctx = context();
                if deadline {
                    ctx.options.deadline = Some(Instant::now());
                } else {
                    ctx.cancellation().cancel();
                }
                let error = if encode {
                    stringify(&mut ctx, &host)
                } else {
                    parse(&mut ctx, text.as_bytes())
                }
                .unwrap_err();
                assert_eq!(
                    error.kind,
                    if deadline {
                        ErrorKind::Deadline
                    } else {
                        ErrorKind::Cancelled
                    }
                );
                assert_eq!(error.class(), None);
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
                assert_eq!(ctx.checkpoint().unwrap_err(), error);
            }
        }
    }

    #[test]
    fn long_strings_reserve_headroom_for_the_open_containers_only() {
        let payload = vec![b'a'; (64 * MAX_VALUE_DEPTH).max(CHUNK)];
        let mut ctx = context();
        let root = stringify(&mut ctx, &Value::bytes(payload.clone())).unwrap();
        let root_retained = ctx.stats().retained_memory_bytes;
        drop(root);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let host = hosts(MAX_VALUE_DEPTH, Value::bytes(payload.clone())).remove(0);
        let mut ctx = context();
        let nested = stringify(&mut ctx, &host).unwrap();
        assert_eq!(
            nested.as_bytes().unwrap().len(),
            payload.len() + 2 + 2 * MAX_VALUE_DEPTH
        );
        // The closing brackets fit in the reserved headroom, so the output
        // buffer never doubled after the string was written.
        assert_eq!(
            ctx.stats().retained_memory_bytes,
            root_retained + 2 * MAX_VALUE_DEPTH
        );
        drop(nested);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn key_order_duplicates_escapes_and_numbers_are_unchanged() {
        let mut ctx = context();
        let value = parse(&mut ctx, br#"{"b":1,"a":[2,{"c":null}],"b":3}"#).unwrap();
        let entries = value.as_hash().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0.as_bytes(), Some(b"b".as_slice()));
        assert_eq!(entries[0].1.as_int(), Some(3));
        assert_eq!(entries[1].0.as_bytes(), Some(b"a".as_slice()));
        assert_eq!(
            stringify(&mut ctx, &value).unwrap().as_bytes(),
            Some(br#"{"b":3,"a":[2,{"c":null}]}"#.as_slice())
        );

        let parsed_text = parse(
            &mut ctx,
            br#"["\u0041\ud83d\ude00\udc00\ud83dx\n\/<>&\u2028"]"#,
        )
        .unwrap();
        let text = parsed_text.as_array().unwrap()[0].clone();
        let decoded = "A\u{1f600}\u{fffd}\u{fffd}x\n/<>&\u{2028}";
        assert_eq!(text.as_bytes(), Some(decoded.as_bytes()));
        // Valid replacement characters are emitted raw; only invalid bytes
        // become \ufffd.
        let encoded = "[\"A\u{1f600}\u{fffd}\u{fffd}x\\n/\\u003c\\u003e\\u0026\\u2028\"]";
        assert_eq!(
            stringify(&mut ctx, &Value::array(vec![text]))
                .unwrap()
                .as_bytes(),
            Some(encoded.as_bytes())
        );
        assert_eq!(
            stringify(&mut ctx, &Value::bytes(vec![0xff]))
                .unwrap()
                .as_bytes(),
            Some(br#""\ufffd""#.as_slice())
        );

        let parsed_numbers = parse(&mut ctx, b"[-0.0,1.5e3,1e21,12345678901234567890,-7]").unwrap();
        let numbers = parsed_numbers.as_array().unwrap();
        assert!(
            numbers[0]
                .as_float()
                .is_some_and(|n| n == 0.0 && n.is_sign_negative())
        );
        assert_eq!(numbers[1].as_float(), Some(1500.0));
        assert_eq!(numbers[3].as_int(), None);
        assert_eq!(numbers[4].as_int(), Some(-7));
        assert_eq!(
            stringify(&mut ctx, &Value::array(numbers.to_vec()))
                .unwrap()
                .as_bytes(),
            Some(b"[-0,1500,1e+21,12345678901234567890,-7]".as_slice())
        );
        for (text, message) in [
            ("[1e400]", "JSON number outside finite f64 range at byte 6"),
            ("01", "trailing JSON data at byte 1"),
            ("[1.]", "invalid JSON fraction at byte 3"),
            ("[\"a", "unterminated JSON string at byte 3"),
            ("[\"\\x\"]", "invalid JSON escape at byte 4"),
            ("[tru]", "invalid JSON literal at byte 1"),
            (
                "[\"\u{1}\"]",
                "unescaped control byte in JSON string at byte 3",
            ),
        ] {
            let error = parse(&mut ctx, text.as_bytes()).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Json, "{text}");
            assert_eq!(error.message, message, "{text}");
        }
        assert_eq!(
            stringify(&mut ctx, &Value::array(vec![Value::float(f64::NAN)]))
                .unwrap_err()
                .message,
            "cannot encode a non-finite float"
        );
        drop((value, parsed_text, parsed_numbers));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
