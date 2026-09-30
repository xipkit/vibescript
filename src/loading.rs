use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Buffer};
use std::{path::PathBuf, sync::Arc};

pub(crate) use resolver::Origin;

mod cache;
mod files;
mod policy;
mod resolver;

/// Filesystem roots, policy and compilation-cache bounds for required source files.
#[derive(Clone, Debug)]
pub struct ModuleConfig {
    /// Ordered directories in which non-relative module requests are resolved.
    pub paths: Vec<PathBuf>,
    /// Allowed module patterns; an empty list permits any name not denied.
    pub allow: Vec<String>,
    /// Denied module patterns, checked before the allow list.
    pub deny: Vec<String>,
    /// Revalidate cached source metadata between calls when enabled.
    pub development: bool,
    /// Maximum cached compiled modules. Zero selects the default of 1,000.
    pub cache_limit: usize,
    /// Maximum source bytes per file. Zero selects the default of one MiB.
    pub source_limit: usize,
}

impl Default for ModuleConfig {
    fn default() -> Self {
        Self {
            paths: Vec::new(),
            allow: Vec::new(),
            deny: Vec::new(),
            development: false,
            cache_limit: 1000,
            source_limit: 1 << 20,
        }
    }
}

pub(crate) struct Loader {
    /// Whether scripts and the files they require keep every runtime type
    /// check ([`crate::Engine::set_keep_type_checks`]).
    pub keep_type_checks: bool,
    resolver: Arc<resolver::Resolver>,
    cache: cache::Cache<Arc<crate::code::Code>>,
    cache_limit: usize,
    development: bool,
}

pub(crate) struct Pin {
    request: Value,
    relative: bool,
    caller: Option<Origin>,
    pub code: Arc<crate::code::Code>,
}

impl Default for Loader {
    fn default() -> Self {
        Self::new(ModuleConfig::default()).expect("empty module configuration is valid")
    }
}

impl Loader {
    pub fn memory(sources: std::collections::BTreeMap<String, String>) -> Result<Self> {
        Ok(Self {
            keep_type_checks: false,
            resolver: Arc::new(resolver::Resolver::memory(sources)?),
            cache: cache::Cache::new(1000),
            cache_limit: 1000,
            development: false,
        })
    }

    pub fn new(config: ModuleConfig) -> Result<Self> {
        let cache_limit = if config.cache_limit == 0 {
            1000
        } else {
            config.cache_limit
        };
        Ok(Self {
            keep_type_checks: false,
            resolver: Arc::new(resolver::Resolver::new(
                &config.paths,
                &config.allow,
                &config.deny,
                if config.source_limit == 0 {
                    1 << 20
                } else {
                    config.source_limit
                },
            )?),
            cache: cache::Cache::new(cache_limit),
            cache_limit,
            development: config.development,
        })
    }

    pub fn fresh(&self) -> Self {
        Self {
            keep_type_checks: self.keep_type_checks,
            resolver: self.resolver.clone(),
            cache: cache::Cache::new(self.cache_limit),
            cache_limit: self.cache_limit,
            development: self.development,
        }
    }

    pub fn clear(&self) {
        self.cache.clear();
    }

    /// The source text and origin a `require` would load for the static checker.
    pub fn source(
        &self,
        request: &str,
        caller: Option<&Origin>,
        memory: Option<usize>,
    ) -> Result<(String, Origin)> {
        let mut options = crate::CallOptions::default();
        if let Some(memory) = memory {
            options.limits.memory_bytes = Some(
                options
                    .limits
                    .memory_bytes
                    .map_or(memory, |limit| limit.min(memory)),
            );
        }
        let mut ctx = CallContext::new(options);
        let mut candidates = self
            .resolver
            .candidates(&mut ctx, request.as_bytes(), caller)?;
        while let Some(candidate) = candidates.next(&mut ctx)? {
            if let Some(source) = self.resolver.read(&mut ctx, &candidate)? {
                let bytes = source.contents.as_bytes().unwrap();
                let text = std::str::from_utf8(bytes).map_err(|_| {
                    Error::new(ErrorKind::Syntax, "required module source is not UTF-8")
                })?;
                // The copy returned is made beside the one read.
                let _copy = ctx.reserve(text.len())?;
                return Ok((text.to_owned(), candidate.origin()));
            }
        }
        Err(Error::new(
            ErrorKind::Name,
            "require: module not found in configured module paths",
        ))
    }

    pub fn load(
        &self,
        ctx: &mut CallContext,
        pins: &mut Buffer<Pin>,
        input: &[u8],
        caller: Option<&Origin>,
        receiving: &Arc<crate::code::Code>,
    ) -> Result<Arc<crate::code::Code>> {
        let request = Name::parse(ctx, input)?;
        let caller = if request.relative { caller } else { None };
        for pin in &pins.data {
            ctx.charge(1)?;
            if pin.relative == request.relative
                && pin.caller.as_ref() == caller
                && crate::json::bytes_equal(
                    ctx,
                    pin.request.as_bytes().unwrap(),
                    request.normalized.as_bytes().unwrap(),
                )?
            {
                return Ok(pin.code.clone());
            }
        }
        let mut candidates = self.resolver.candidates(ctx, input, caller)?;
        while let Some(candidate) = candidates.next(ctx)? {
            let mut pinned = None;
            for pin in &pins.data {
                ctx.charge(candidate.relative.as_bytes().unwrap().len() as u64 + 1)?;
                if pin.code.origin.as_ref().is_some_and(|origin| {
                    origin.root == candidate.root
                        && origin.relative.as_ref() == candidate.relative.as_bytes().unwrap()
                }) {
                    pinned = Some(pin.code.clone());
                    break;
                }
            }
            if let Some(code) = pinned {
                pins.push(
                    ctx,
                    Pin {
                        request: request.normalized,
                        relative: request.relative,
                        caller: caller.cloned(),
                        code: code.clone(),
                    },
                )?;
                return Ok(code);
            }
            let (epoch, cached) =
                self.cache
                    .lookup(ctx, &candidate.root, candidate.relative.as_bytes().unwrap())?;
            let code = if let Some(cached) = cached {
                if !self.development || self.resolver.valid(ctx, &candidate, cached.stamp)? {
                    Some(cached.code.clone())
                } else {
                    self.cache.invalidate(ctx, &cached)?;
                    None
                }
            } else {
                None
            };
            let code = if let Some(code) = code {
                code
            } else {
                let Some(source) = self.resolver.read(ctx, &candidate)? else {
                    continue;
                };
                let source_text = std::str::from_utf8(source.contents.as_bytes().unwrap())
                    .map_err(|_| {
                        crate::compilation::error(
                            &crate::compilation::Meter(std::cell::RefCell::new(&mut *ctx)),
                            None,
                            format_args!("required source must be UTF-8"),
                        )
                    })?;
                ctx.work_bytes(source_text.len())?;
                let origin = candidate.origin();
                let compiled = crate::code::Code::compile_module(
                    ctx,
                    source_text,
                    receiving,
                    origin.clone(),
                    self,
                );
                ctx.checkpoint()?;
                let code = compiled?;
                self.cache
                    .insert(ctx, &epoch, origin, source.stamp, code)?
                    .code
                    .clone()
            };
            pins.push(
                ctx,
                Pin {
                    request: request.normalized,
                    relative: request.relative,
                    caller: caller.cloned(),
                    code: code.clone(),
                },
            )?;
            return Ok(code);
        }
        Err(Error::new(ErrorKind::Name, "require: module not found"))
    }
}

#[cfg(test)]
pub(crate) mod test_support;

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
    fn invalid_source_compilation_accounts_rejection_and_never_caches_it() {
        let directory = test_support::Directory::new();
        let path = directory.write("answer.vibe", &[0xff]);
        let loader = Loader::new(ModuleConfig {
            paths: vec![directory.0.clone()],
            ..ModuleConfig::default()
        })
        .unwrap();
        let receiving = crate::code::Code::compile("nil", &Default::default()).unwrap();
        let run = |ctx: &mut CallContext| {
            loader.load(ctx, &mut Buffer::empty(), b"answer", None, &receiving)
        };
        let mut context = CallContext::new(CallOptions::default());
        let error = run(&mut context).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Syntax);
        assert_eq!(error.message, "required source must be UTF-8");
        assert!(error.retained_charge.is_some());
        let stats = context.stats();
        assert!(stats.retained_memory_bytes >= error.message.capacity());
        drop(error);
        assert_eq!(context.stats().retained_memory_bytes, 0);
        for memory in [false, true] {
            for short in [0, 1] {
                let mut options = CallOptions::default();
                if memory {
                    options.limits.memory_bytes = Some(stats.peak_memory_bytes - short);
                } else {
                    options.limits.steps = Some(stats.steps - short as u64);
                }
                let mut context = CallContext::new(options);
                let error = run(&mut context).unwrap_err();
                let kind = if short == 0 {
                    ErrorKind::Syntax
                } else if memory {
                    ErrorKind::Memory
                } else {
                    ErrorKind::Steps
                };
                assert_eq!(error.kind, kind);
                if short != 0 {
                    assert_eq!(context.checkpoint().unwrap_err().kind, kind);
                }
                drop(error);
                assert_eq!(context.stats().retained_memory_bytes, 0);
            }
        }
        std::fs::write(&path, "def answer;42;end").unwrap();
        let code = run(&mut context).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(Arc::ptr_eq(&code, &run(&mut context).unwrap()));
        assert_eq!(context.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn cached_compilation_releases_the_first_invocations_temporary_storage() {
        let directory = test_support::Directory::new();
        let source = format!(
            "def unused;{}end;class Box;def original;{}end;alias copied original;end;def value -> int;42;end",
            "1;".repeat(1024),
            r#"["text",:symbol,/pattern/,999999999999999999999999,{label:"value"},["word#{1}"]];"#
        );
        let path = directory.write("answer.vibe", source.as_bytes());
        let loader = Loader::new(ModuleConfig {
            paths: vec![directory.0.clone()],
            ..ModuleConfig::default()
        })
        .unwrap();
        let receiving = crate::code::Code::compile("nil", &Default::default()).unwrap();
        let mut context = CallContext::new(CallOptions::default());
        let memory = Arc::downgrade(&context.identity());
        let mut pins = Buffer::empty();
        let code = loader
            .load(&mut context, &mut pins, b"answer", None, &receiving)
            .unwrap();
        let peak = context.stats().peak_memory_bytes;
        assert!(peak > source.len() * 4);
        drop(pins);
        assert_eq!(context.stats().retained_memory_bytes, 0);
        drop(context);
        assert!(memory.upgrade().is_none());

        std::fs::remove_file(path).unwrap();
        let mut context = CallContext::new(CallOptions::default());
        let mut pins = Buffer::empty();
        let cached = loader
            .load(&mut context, &mut pins, b"answer", None, &receiving)
            .unwrap();
        assert!(Arc::ptr_eq(&code, &cached));
        assert!(context.stats().peak_memory_bytes < peak);
        drop(pins);
        assert_eq!(context.stats().retained_memory_bytes, 0);
    }

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
