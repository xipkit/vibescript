use super::format::{self, MONTHS, OUTPUT_LIMIT, Output, View, WEEKDAYS};
use crate::{CallContext, Error, ErrorKind, Result, Value, casing, json, scan};
use std::fmt::Write;

#[derive(Clone, Copy, Default)]
struct Token {
    length: usize,
    directive: u8,
    width: Option<usize>,
    colons: usize,
    no_pad: bool,
    pad: Option<u8>,
    upper: bool,
    toggle: bool,
}

fn invalid() -> Error {
    Error::new(ErrorKind::Argument, "invalid strftime format")
}

fn checkpoint(ctx: &mut CallContext, index: usize) -> Result<()> {
    if index % 1024 == 0 {
        ctx.charge(1)?;
        ctx.checkpoint()?;
    }
    Ok(())
}

fn token(ctx: &mut CallContext, input: &[u8]) -> Result<Option<Token>> {
    let mut token = Token::default();
    let mut i = 1;
    while i < input.len() {
        checkpoint(ctx, i)?;
        match input[i] {
            b'-' => token.no_pad = true,
            b'_' => token.pad = Some(b' '),
            b'0' => token.pad = Some(b'0'),
            b'^' => token.upper = true,
            b'#' => token.toggle = true,
            _ => break,
        }
        i += 1;
    }
    let start = i;
    let mut width = Some(0usize);
    while input.get(i).is_some_and(u8::is_ascii_digit) {
        checkpoint(ctx, i)?;
        width = width
            .and_then(|n| n.checked_mul(10))
            .and_then(|n| n.checked_add(usize::from(input[i] - b'0')))
            .filter(|&n| n <= i64::MAX as usize);
        i += 1;
    }
    if i > start {
        token.width = width;
    }
    while input.get(i) == Some(&b':') {
        checkpoint(ctx, i)?;
        token.colons += 1;
        i += 1;
    }
    let Some(&directive) = input.get(i) else {
        return Ok(None);
    };
    token.directive = directive;
    token.length = i + 1;
    Ok(Some(token))
}

fn supported(token: Token) -> bool {
    (token.colons == 0 || (token.directive == b'z' && token.colons <= 3))
        && b"%AaBbCcDdeFHhIjkLlMmNnPpRrSsTtuwXxYyZz".contains(&token.directive)
}

pub(super) fn recognized(ctx: &mut CallContext, input: &[u8]) -> Result<bool> {
    let mut i = 0;
    let mut recognized = false;
    while i < input.len() {
        checkpoint(ctx, i)?;
        if input[i] != b'%' {
            i += 1;
            continue;
        }
        let Some(token) = token(ctx, &input[i..])? else {
            return Ok(false);
        };
        recognized |= supported(token) && (token.directive != b'%' || token.length != 2);
        i += token.length;
    }
    Ok(recognized)
}

fn layout_signature(ctx: &mut CallContext, input: &[u8]) -> Result<bool> {
    let mut signature = false;
    for i in 0..input.len() {
        checkpoint(ctx, i)?;
        if input[i] == b'%' {
            return Ok(false);
        }
        signature |= [
            b"2006".as_slice(),
            b"15:04",
            b"01/02",
            b"02/01",
            b"01-02",
            b"02-01",
        ]
        .iter()
        .any(|signature| input[i..].starts_with(signature));
    }
    Ok(signature)
}

fn uppercase(
    ctx: &mut CallContext,
    token: Token,
    inherited: bool,
    text: &[u8],
) -> Result<Option<bool>> {
    if token.toggle {
        let mut saw_cased = false;
        for chunk in text.chunks(1024) {
            ctx.charge(1)?;
            ctx.checkpoint()?;
            for byte in chunk {
                if byte.is_ascii_lowercase() {
                    return Ok(Some(true));
                }
                saw_cased |= byte.is_ascii_uppercase();
            }
        }
        return Ok(Some(!saw_cased));
    }
    Ok((token.upper || inherited).then_some(true))
}

fn mapped_size(ctx: &mut CallContext, mut input: &[u8], upper: Option<bool>) -> Result<usize> {
    let Some(upper) = upper else {
        return Ok(input.len());
    };
    let mut size = 0usize;
    let mut scanned = 0;
    while !input.is_empty() {
        if scanned == 0 {
            ctx.charge(1)?;
            ctx.checkpoint()?;
        }
        let (char, length, _) = scan::rune(input);
        let Some(next) = size.checked_add(casing::map(char, upper).len_utf8()) else {
            return ctx.fail(ErrorKind::Memory, "case mapping size overflow");
        };
        size = next;
        input = &input[length..];
        scanned = (scanned + length) % 1024;
        if scanned < length {
            scanned = 0;
        }
    }
    Ok(size)
}

fn mapped(
    ctx: &mut CallContext,
    out: &mut Output<'_>,
    mut input: &[u8],
    upper: Option<bool>,
) -> Result<()> {
    let Some(upper) = upper else {
        return out.append(ctx, input);
    };
    let mut buffer = [0; 4096];
    let mut used = 0;
    let mut scanned = 0;
    while !input.is_empty() {
        if scanned == 0 {
            ctx.charge(1)?;
            ctx.checkpoint()?;
        }
        let (char, length, _) = scan::rune(input);
        let mut utf8 = [0; 4];
        let bytes = casing::map(char, upper).encode_utf8(&mut utf8).as_bytes();
        if used + bytes.len() > buffer.len() {
            out.append(ctx, &buffer[..used])?;
            used = 0;
        }
        buffer[used..used + bytes.len()].copy_from_slice(bytes);
        used += bytes.len();
        input = &input[length..];
        scanned += length;
        if scanned >= 1024 {
            scanned = 0;
        }
    }
    out.append(ctx, &buffer[..used])
}

fn padded(
    ctx: &mut CallContext,
    out: &mut Output<'_>,
    text: &[u8],
    width: usize,
    pad: u8,
    signed: bool,
    upper: Option<bool>,
) -> Result<()> {
    let padding = if text.is_empty() {
        0
    } else {
        width.saturating_sub(text.len())
    };
    let size = mapped_size(ctx, text, upper)?;
    out.reserve(ctx, size.saturating_add(padding))?;
    let sign = signed && pad == b'0' && matches!(text.first(), Some(b'+' | b'-'));
    if sign {
        out.append(ctx, &text[..1])?;
    }
    out.repeat(ctx, pad, padding)?;
    mapped(ctx, out, &text[usize::from(sign)..], upper)
}

#[derive(Clone, Copy)]
enum Kind {
    Numeric,
    Year,
    Name,
    Literal,
    Offset,
    Compound,
    Subsecond,
}

fn directive(
    ctx: &mut CallContext,
    view: &View<'_>,
    token: Token,
    inherited: bool,
    out: &mut Output<'_>,
) -> Result<()> {
    let mut number = json::Number::new();
    macro_rules! numeric {
        ($value:expr, $width:expr, $pad:expr, $kind:expr) => {{
            write!(number, "{}", $value).unwrap();
            (number.bytes(), $width, $pad, $kind)
        }};
    }
    let (text, default_width, default_pad, kind): (&[u8], usize, u8, Kind) = match token.directive {
        b'Y' => numeric!(view.date.year, 4, b'0', Kind::Year),
        b'C' => numeric!(view.date.year.div_euclid(100), 2, b'0', Kind::Numeric),
        b'y' => numeric!(view.date.year.rem_euclid(100), 2, b'0', Kind::Numeric),
        b'm' => numeric!(view.date.month, 2, b'0', Kind::Numeric),
        b'd' | b'e' => numeric!(
            view.date.day,
            2,
            if token.directive == b'e' { b' ' } else { b'0' },
            Kind::Numeric
        ),
        b'j' => numeric!(view.date.yearday, 3, b'0', Kind::Numeric),
        b'H' | b'k' => numeric!(
            view.date.hour,
            2,
            if token.directive == b'k' { b' ' } else { b'0' },
            Kind::Numeric
        ),
        b'I' | b'l' => numeric!(
            format::hour12(view.date.hour),
            2,
            if token.directive == b'l' { b' ' } else { b'0' },
            Kind::Numeric
        ),
        b'M' => numeric!(view.date.minute, 2, b'0', Kind::Numeric),
        b'S' => numeric!(view.date.second, 2, b'0', Kind::Numeric),
        b'w' => numeric!(view.date.weekday, 1, b'0', Kind::Numeric),
        b'u' => numeric!(
            if view.date.weekday == 0 {
                7
            } else {
                view.date.weekday
            },
            1,
            b'0',
            Kind::Numeric
        ),
        b's' => numeric!(view.stamp.seconds(), 1, b'0', Kind::Numeric),
        b'L' | b'N' => {
            write!(number, "{:09}", view.stamp.nanos()).unwrap();
            (
                number.bytes(),
                if token.directive == b'L' { 3 } else { 9 },
                b'0',
                Kind::Subsecond,
            )
        }
        b'p' | b'P' => {
            let text: &[u8] = match (view.date.hour < 12, token.directive) {
                (true, b'p') => b"AM",
                (false, b'p') => b"PM",
                (true, _) => b"am",
                (false, _) => b"pm",
            };
            (text, 0, b' ', Kind::Name)
        }
        b'A' => (WEEKDAYS[view.date.weekday as usize], 0, b' ', Kind::Name),
        b'a' => (
            &WEEKDAYS[view.date.weekday as usize][..3],
            0,
            b' ',
            Kind::Name,
        ),
        b'B' => (MONTHS[view.date.month as usize - 1], 0, b' ', Kind::Name),
        b'b' | b'h' => (
            &MONTHS[view.date.month as usize - 1][..3],
            0,
            b' ',
            Kind::Name,
        ),
        b'Z' => (view.zone.name, 0, b' ', Kind::Name),
        b'z' => {
            let seconds = i64::from(view.zone.seconds);
            let magnitude = seconds.unsigned_abs();
            let (hours, minutes, seconds_part) =
                (magnitude / 3600, magnitude % 3600 / 60, magnitude % 60);
            write!(
                number,
                "{}{:02}",
                if seconds < 0 { '-' } else { '+' },
                hours
            )
            .unwrap();
            if token.colons != 3 || minutes != 0 || seconds_part != 0 {
                if token.colons > 0 {
                    number.write_char(':').unwrap();
                }
                write!(number, "{minutes:02}").unwrap();
            }
            if token.colons == 2 || (token.colons == 3 && seconds_part != 0) {
                write!(number, ":{seconds_part:02}").unwrap();
            }
            (number.bytes(), 0, b'0', Kind::Offset)
        }
        b'n' => (b"\n", 1, b' ', Kind::Literal),
        b't' => (b"\t", 1, b' ', Kind::Literal),
        b'%' => (b"%", 1, b' ', Kind::Literal),
        b'F' => (b"%Y-%m-%d", 0, b' ', Kind::Compound),
        b'T' | b'X' => (b"%H:%M:%S", 0, b' ', Kind::Compound),
        b'R' => (b"%H:%M", 0, b' ', Kind::Compound),
        b'D' | b'x' => (b"%m/%d/%y", 0, b' ', Kind::Compound),
        b'r' => (b"%I:%M:%S %p", 0, b' ', Kind::Compound),
        b'c' => (b"%a %b %e %T %Y", 0, b' ', Kind::Compound),
        _ => unreachable!(),
    };
    let width = token.width.unwrap_or(default_width);
    match kind {
        Kind::Compound => {
            // Fixed sub-formats nest at most twice and render fewer than 512 bytes.
            let mut buffer = [0; 512];
            let mut fixed = Output::fixed(&mut buffer);
            render(ctx, view, text, token.upper || inherited, &mut fixed)?;
            let length = fixed.len();
            padded(
                ctx,
                out,
                &buffer[..length],
                width,
                if token.pad == Some(b'0') { b'0' } else { b' ' },
                false,
                None,
            )
        }
        Kind::Subsecond => {
            out.reserve(ctx, width)?;
            out.append(ctx, &text[..width.min(9)])?;
            out.repeat(ctx, b'0', width.saturating_sub(9))
        }
        Kind::Offset => padded(ctx, out, text, width, b'0', true, None),
        _ => {
            let width = if token.no_pad {
                0
            } else if matches!(kind, Kind::Year) && token.width.is_none() {
                4 + usize::from(text.first() == Some(&b'-'))
            } else {
                width
            };
            let upper = uppercase(ctx, token, inherited, text)?;
            padded(
                ctx,
                out,
                text,
                width,
                token.pad.unwrap_or(default_pad),
                true,
                upper,
            )
        }
    }
}

fn render(
    ctx: &mut CallContext,
    view: &View<'_>,
    mut input: &[u8],
    inherited: bool,
    out: &mut Output<'_>,
) -> Result<()> {
    while !input.is_empty() {
        ctx.charge(1)?;
        ctx.checkpoint()?;
        if input[0] != b'%' {
            let n = input[..input.len().min(4096)]
                .iter()
                .position(|&b| b == b'%')
                .unwrap_or(input.len().min(4096));
            out.append(ctx, &input[..n])?;
            input = &input[n..];
            continue;
        }
        let token = token(ctx, input)?.ok_or_else(invalid)?;
        if supported(token) {
            directive(ctx, view, token, inherited, out)?;
        } else {
            out.append(ctx, &input[..token.length])?;
        }
        input = &input[token.length..];
    }
    Ok(())
}

pub(super) fn format(ctx: &mut CallContext, value: &Value, input: &[u8]) -> Result<Value> {
    let view = View::new(ctx, value)?;
    if layout_signature(ctx, input)? {
        if input.len() > OUTPUT_LIMIT {
            return ctx.guard(
                ErrorKind::OutputLimit,
                "time formatting output limit exceeded",
            );
        }
        let mut compare = Output::compare(input, 64 * OUTPUT_LIMIT);
        format::render(ctx, &view, input, &mut compare)?;
        if !compare.matches() {
            return Err(Error::new(
                ErrorKind::Argument,
                "time.strftime expects percent directives; use format for Go layouts",
            ));
        }
    }
    let mut out = Output::buffer(OUTPUT_LIMIT);
    render(ctx, &view, input, false, &mut out)?;
    out.finish(ctx)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    fn time(ctx: &mut CallContext, name: &[u8], offset: i32) -> Value {
        let zone = super::super::zone::Zone::fixed(ctx, name, offset).unwrap();
        super::super::value(ctx, super::super::Stamp::new(0, 0), Some(zone)).unwrap()
    }

    #[test]
    fn timezone_case_uses_simple_unicode_mapping_and_preserves_raw_bytes() {
        let mut ctx = CallContext::new(CallOptions::default());
        for (name, layout, expected) in [
            ("éȺı".as_bytes(), "%^Z", "ÉȺI".as_bytes()),
            ("AéK".as_bytes(), "%#010Z", "0000aék".as_bytes()),
            ("Aİȿᾀß".as_bytes(), "%^Z", "AİⱾᾈß".as_bytes()),
            ("Aİȿᾀß".as_bytes(), "%#Z", "aiȿᾀß".as_bytes()),
            (b"A\xff".as_slice(), "%Z", b"A\xff".as_slice()),
            (b"A\xff", "%^Z", "A�".as_bytes()),
            (b"A\xff", "%#Z", "a�".as_bytes()),
            (b"-Ab", "%^08Z", b"-00000AB"),
            (b"", "%100Z", b""),
        ] {
            let value = time(&mut ctx, name, 0);
            let result = format(&mut ctx, &value, layout.as_bytes()).unwrap();
            assert_eq!(result.as_bytes(), Some(expected), "{name:?}: {layout}");
        }
        let value = time(&mut ctx, b"", -1);
        let result = format(&mut ctx, &value, b"%z %:z %::z %:::z").unwrap();
        assert_eq!(
            result.as_bytes(),
            Some(b"-0000 -00:00 -00:00:01 -00:00:01".as_slice())
        );
        let result = super::format::format(&mut ctx, &value, b"MST -07:00:00").unwrap();
        assert_eq!(result.as_bytes(), Some(b"+0000 +00:00:-01".as_slice()));
    }

    #[test]
    fn output_guards_allow_recovery_without_allocating_requested_padding() {
        for layout in [
            "%1000000000N",
            "%1000000000F",
            "%1000000000Y",
            "%1000000000z",
        ] {
            let mut ctx = CallContext::new(CallOptions::default());
            let value = Value::time(0, 0).unwrap();
            let before = ctx.stats().peak_memory_bytes;
            assert_eq!(
                format(&mut ctx, &value, layout.as_bytes())
                    .unwrap_err()
                    .kind,
                ErrorKind::OutputLimit
            );
            assert_eq!(ctx.stats().peak_memory_bytes, before);
            assert_eq!(ctx.bytes(b"x").unwrap().as_bytes(), Some(b"x".as_slice()));
        }
    }

    #[test]
    fn timezone_case_growth_and_shrinking_obey_limits() {
        let mut owner = CallContext::new(CallOptions::default());
        let shrinking = time(&mut owner, format!("A{}", "K".repeat(131072)).as_bytes(), 0);
        let mut ctx = CallContext::new(CallOptions::default());
        let imported = ctx.import(&shrinking).unwrap();
        let before = ctx.stats().retained_memory_bytes;
        let result = format(&mut ctx, &imported, b"%#Z").unwrap();
        let bytes = result.as_bytes().unwrap();
        assert_eq!(bytes.len(), 131073);
        assert_eq!(bytes[0], b'a');
        assert!(bytes[1..].iter().all(|&b| b == b'k'));
        drop(result);
        assert_eq!(ctx.stats().retained_memory_bytes, before);
        drop(imported);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);

        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(64),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            format(&mut ctx, &shrinking, b"%#Z").unwrap_err().kind,
            ErrorKind::Steps
        );
        let mut ctx = CallContext::new(CallOptions::default());
        let growing = time(&mut ctx, "ȿ".repeat(400000).as_bytes(), 0);
        let peak = ctx.stats().peak_memory_bytes;
        assert_eq!(
            format(&mut ctx, &growing, b"%^Z").unwrap_err().kind,
            ErrorKind::OutputLimit
        );
        assert_eq!(ctx.stats().peak_memory_bytes, peak);
    }

    #[test]
    fn layout_diagnostics_compare_without_materializing_expansion() {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let value = time(&mut ctx, &vec![b'A'; 32768], 0);
        let layout = format!("2006{}", "MST".repeat(1024));
        let peak = ctx.stats().peak_memory_bytes;
        let error = format(&mut ctx, &value, layout.as_bytes()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Argument);
        assert!(error.message.len() < 256);
        assert_eq!(ctx.stats().peak_memory_bytes, peak);
        let layout = format!("2006{}", "MST".repeat(2049));
        assert_eq!(
            format(&mut ctx, &value, layout.as_bytes())
                .unwrap_err()
                .kind,
            ErrorKind::OutputLimit
        );
        assert_eq!(ctx.stats().peak_memory_bytes, peak);
    }
}
