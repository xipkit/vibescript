use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Buffer};

mod cache;
mod files;
mod policy;
mod resolver;

#[cfg(test)]
mod test_support;

pub(crate) struct Name {
    pub normalized: Value,
    pub relative: bool,
}

impl Name {
    pub fn parse(ctx: &mut CallContext, input: &[u8]) -> Result<Self> {
        ctx.charge((input.len() as u64).saturating_add(1))?;
        let input = trimmed(input);
        if input.is_empty() {
            return Err(argument("module name must be non-empty"));
        }
        let relative = [b"./".as_slice(), b"../", b".\\", b"..\\"]
            .iter()
            .any(|prefix| input.starts_with(prefix));
        let mut output = Buffer::with_capacity(ctx, input.len().max(1))?;
        clean(input, &mut output.data);
        if output.data == b"." {
            return Err(argument("module name resolves to current directory"));
        }
        if output.data.first() == Some(&b'/') {
            return Err(argument("module name must be relative"));
        }
        if !relative && output.data.split(|&b| b == b'/').any(|part| part == b"..") {
            return Err(argument("module name escapes search paths"));
        }
        let base = output.data.rsplit(|&b| b == b'/').next().unwrap();
        if base == b".." {
            return Err(argument("module name resolves to a directory"));
        }
        if !base.contains(&b'.') {
            output.extend(ctx, b".vibe")?;
        }
        output.shrink(ctx)?;
        Ok(Self {
            normalized: Value::from_bytes(ctx, output)?,
            relative,
        })
    }
}

fn argument(message: &str) -> Error {
    Error::new(ErrorKind::Argument, format!("require: {message}"))
}

fn trimmed(input: &[u8]) -> &[u8] {
    let mut first = input.len();
    let mut last = 0;
    let mut at = 0;
    while at < input.len() {
        let (rune, width, _) = crate::scan::rune(&input[at..]);
        if !rune.is_whitespace() {
            first = first.min(at);
            last = at + width;
        }
        at += width;
    }
    if last == 0 {
        &input[..0]
    } else {
        &input[first..last]
    }
}

// The output never exceeds max(input.len(), 1), so callers can reserve it first.
fn clean(input: &[u8], output: &mut Vec<u8>) {
    output.clear();
    let absolute = input.first().is_some_and(|b| matches!(b, b'/' | b'\\'));
    if absolute {
        output.push(b'/');
    }
    for part in input.split(|&b| matches!(b, b'/' | b'\\')) {
        if part.is_empty() || part == b"." {
            continue;
        }
        if part == b".." {
            let start = output.iter().rposition(|&b| b == b'/').map_or(0, |i| i + 1);
            if !output.is_empty() && output.len() > start && &output[start..] != b".." {
                output.truncate(start.saturating_sub(1).max(usize::from(absolute)));
                continue;
            }
            if absolute {
                continue;
            }
        }
        if !output.is_empty() && output.last() != Some(&b'/') {
            output.push(b'/');
        }
        output.extend_from_slice(part);
    }
    if output.is_empty() {
        output.push(b'.');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, CancellationToken, Limits};

    #[test]
    fn requests_preserve_relative_intent_and_filename_bytes() {
        let cases: &[(&[u8], &[u8], bool)] = &[
            (b" tool ", b"tool.vibe", false),
            (b"./tool", b"tool.vibe", true),
            (b".\\pkg\\tool", b"pkg/tool.vibe", true),
            (b"pkg\\..\\tool", b"tool.vibe", false),
            (b"../shared/tool", b"../shared/tool.vibe", true),
            (b"pkg///tool.vibe.vibe", b"pkg/tool.vibe.vibe", false),
            (b"./ tool /. ", b" tool .vibe", true),
            (b".vibe", b".vibe", false),
            (b"pkg/..vibe", b"pkg/..vibe", false),
            (b"./\xff", b"\xff.vibe", true),
            ("\u{2003}tool\u{0085}".as_bytes(), b"tool.vibe", false),
        ];
        for (input, expected, relative) in cases {
            let mut ctx = CallContext::new(CallOptions::default());
            let name = Name::parse(&mut ctx, input).unwrap();
            assert_eq!(name.normalized.as_bytes().unwrap(), *expected, "{input:?}");
            assert_eq!(name.relative, *relative, "{input:?}");
        }
    }

    #[test]
    fn requests_reject_directories_absolute_paths_and_implicit_escapes() {
        for input in [
            "",
            " \t ",
            ".",
            "./",
            "pkg/..",
            "..",
            "../..",
            "/tool",
            "\\tool",
            "pkg/../../tool",
        ] {
            let mut ctx = CallContext::new(CallOptions::default());
            assert_eq!(
                Name::parse(&mut ctx, input.as_bytes()).err().unwrap().kind,
                ErrorKind::Argument,
                "{input:?}"
            );
        }
    }

    #[test]
    fn normalization_is_bounded_and_releases_scratch() {
        let input = "segment/../".repeat(8192) + "tool";
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: Some(64),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            Name::parse(&mut ctx, input.as_bytes()).err().unwrap().kind,
            ErrorKind::Steps
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(4096),
                steps: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        assert_eq!(
            Name::parse(&mut ctx, input.as_bytes()).err().unwrap().kind,
            ErrorKind::Memory
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                steps: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let name = Name::parse(&mut ctx, input.as_bytes()).unwrap();
        assert_eq!(name.normalized.as_bytes().unwrap(), b"tool.vibe");
        assert!(ctx.stats().retained_memory_bytes < 512);
        drop(name);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let mut ctx = CallContext::new(CallOptions {
            cancellation,
            ..CallOptions::default()
        });
        assert_eq!(
            Name::parse(&mut ctx, b"tool").err().unwrap().kind,
            ErrorKind::Cancelled
        );
    }
}
