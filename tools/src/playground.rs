//! One JSON request per process for the browser playground. See `docs/playground.md`.

use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json, value::RawValue};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{self, Read, Write},
    sync::{Arc, Mutex},
};
use vibescript::{
    CallOptions, Capability, Engine, Error, ErrorKind, HostMethod, Limits, Script, Signature,
    SignatureParam, Stats, Value,
    diagnostic::{Code, Diagnostic, Span},
};

const INPUT_LIMIT: usize = 1 << 20;
const OUTPUT_LIMIT: usize = 64 << 10;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    op: Operation,
    source: String,
    #[serde(default)]
    files: BTreeMap<String, String>,
    #[serde(default)]
    entry: Option<String>,
    #[serde(default = "empty_arguments")]
    args: Box<RawValue>,
    #[serde(default)]
    limits: Quotas,
    #[serde(default)]
    capabilities: Vec<Preview>,
}

fn empty_arguments() -> Box<RawValue> {
    RawValue::from_string("[]".into()).unwrap()
}

#[derive(Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Check,
    Run,
    Format,
    Fix,
}

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Quotas {
    steps: u64,
    memory_bytes: usize,
    recursion: usize,
}

impl Default for Quotas {
    fn default() -> Self {
        Self {
            steps: 250_000,
            memory_bytes: 256 << 10,
            recursion: 32,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Preview {
    name: String,
    members: Vec<Member>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    name: String,
    signature: Contract,
    behavior: Behavior,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Behavior {
    Preview,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Contract {
    params: Vec<Parameter>,
    result: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Parameter {
    name: String,
    #[serde(rename = "type")]
    ty: String,
    #[serde(default)]
    optional: bool,
}

#[derive(Serialize)]
struct Response {
    ok: bool,
    output: Vec<String>,
    stderr: Vec<String>,
    result: Option<Box<RawValue>>,
    error: Option<Json>,
    diagnostics: Vec<Json>,
    stats: Json,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    files: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    applied: Option<usize>,
}

impl Default for Response {
    fn default() -> Self {
        Self {
            ok: false,
            output: Vec::new(),
            stderr: Vec::new(),
            result: None,
            error: None,
            diagnostics: Vec::new(),
            stats: stats_json(Stats::default()),
            source: None,
            files: None,
            applied: None,
        }
    }
}

impl Response {
    fn fail(&mut self, error: &Error, source: &str) {
        let location = error
            .diagnostic
            .as_ref()
            .map(|d| {
                json!({
                    "file": d.filename.as_deref().map(String::from_utf8_lossy),
                    "line": d.position.line, "column": d.position.column, "offset": error.offset,
                })
            })
            .or_else(|| {
                error.offset.map(|offset| {
                    let at = Span::at(offset).position(source);
                    json!({ "file": null, "line": at.line, "column": at.column, "offset": offset })
                })
            });
        self.error = Some(json!({
            "kind": format!("{:?}", error.kind), "message": error.message,
            "location": location,
        }));
        self.ok = false;
    }
}

fn stats_json(stats: Stats) -> Json {
    json!({ "steps": stats.steps, "peak_memory_bytes": stats.peak_memory_bytes,
        "retained_memory_bytes": stats.retained_memory_bytes })
}

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::Argument, message)
}

fn codec_options() -> CallOptions {
    CallOptions {
        limits: Limits {
            steps: Some(5_000_000),
            memory_bytes: Some(16 << 20),
            recursion: 128,
        },
        ..CallOptions::default()
    }
}

fn identifier(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

fn capabilities(previews: Vec<Preview>) -> vibescript::Result<Vec<Capability>> {
    let mut names = BTreeSet::new();
    let mut capabilities = Vec::new();
    if previews.len() > 64 {
        return Err(invalid("at most 64 capabilities are allowed"));
    }
    for preview in previews {
        if !identifier(&preview.name) || !names.insert(preview.name.clone()) {
            return Err(invalid("capability names must be unique identifiers"));
        }
        if preview.members.len() > 64 {
            return Err(invalid("at most 64 members per capability are allowed"));
        }
        let mut members = Vec::new();
        let mut member_names = BTreeSet::new();
        for member in preview.members {
            let Behavior::Preview = member.behavior;
            if !identifier(&member.name) || !member_names.insert(member.name.clone()) {
                return Err(invalid("member names must be unique identifiers"));
            }
            if member.signature.params.len() > 64 {
                return Err(invalid("at most 64 parameters per member are allowed"));
            }
            let mut parameters = BTreeSet::new();
            for p in &member.signature.params {
                if !identifier(&p.name) || p.name == "status" || !parameters.insert(p.name.clone())
                {
                    return Err(invalid(
                        "parameter names must be unique identifiers other than status",
                    ));
                }
            }
            let signature = Signature {
                params: member
                    .signature
                    .params
                    .into_iter()
                    .map(|p| SignatureParam {
                        name: p.name,
                        ty: p.ty,
                        optional: p.optional,
                    })
                    .collect(),
                result: member.signature.result,
                accepts_block: false,
            };
            let fields: Vec<_> = signature
                .params
                .iter()
                .map(|p| p.name.as_bytes().to_vec())
                .collect();
            let method = HostMethod::new(
                format!("{}.{}", preview.name, member.name),
                move |ctx, args, keywords| {
                    ctx.checkpoint()?;
                    if !keywords.is_empty() {
                        return Err(invalid("preview methods accept positional arguments only"));
                    }
                    ctx.charge(args.len() as u64 + 1)?;
                    let mut record: Vec<_> = fields
                        .iter()
                        .zip(args)
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect();
                    record.push((b"status".to_vec(), Value::bytes("preview")));
                    ctx.import(&Value::hash(record))
                },
            )
            .with_signature(signature)?;
            members.push((member.name.into_bytes(), method.value()));
        }
        capabilities.push(Capability::from_value(preview.name, Value::object(members)));
    }
    Ok(capabilities)
}

type Capture = Arc<Mutex<Vec<u8>>>;

fn capture(
    buffer: &Capture,
    ctx: &mut vibescript::CallContext,
    bytes: &[u8],
) -> vibescript::Result<()> {
    ctx.checkpoint()?;
    let mut output = buffer.lock().unwrap();
    if bytes.len() > OUTPUT_LIMIT - output.len() {
        return Err(Error::new(
            ErrorKind::OutputLimit,
            "playground output exceeds 65536 bytes",
        ));
    }
    output.extend_from_slice(bytes);
    Ok(())
}

fn lines(buffer: &Capture) -> Vec<String> {
    let bytes = buffer.lock().unwrap();
    if bytes.is_empty() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&bytes);
    text.strip_suffix('\n')
        .unwrap_or(&text)
        .split('\n')
        .map(str::to_owned)
        .collect()
}

fn inspect(engine: &Engine, source: &str) -> (Option<Script>, Vec<Diagnostic>, Option<Error>) {
    match engine.compile_with_options(source, &codec_options()) {
        Ok(script) => match engine.type_check(source) {
            Ok(checked) => (Some(script), checked.diagnostics, None),
            Err(error) => (None, Vec::new(), Some(error)),
        },
        Err(error) => {
            let diagnostics = if error.diagnostics().is_empty() && error.kind == ErrorKind::Syntax {
                vec![Diagnostic::error(
                    Code::SYNTAX,
                    Span::at(error.offset.unwrap_or(0)),
                    error.message.clone(),
                )]
            } else {
                error.diagnostics().to_vec()
            };
            (None, diagnostics, Some(error))
        }
    }
}

fn diagnostics_json(
    diagnostics: &[Diagnostic],
    source: &str,
    files: &BTreeMap<String, String>,
) -> Vec<Json> {
    diagnostics
        .iter()
        .map(|d| {
            let text = d
                .file
                .as_deref()
                .and_then(|file| std::str::from_utf8(file).ok())
                .and_then(|file| files.get(file))
                .map_or(source, String::as_str);
            serde_json::from_str(&d.to_json(text)).expect("diagnostics are valid JSON")
        })
        .collect()
}

fn execute(
    mut request: Request,
    response: &mut Response,
    output: Capture,
    stderr: Capture,
) -> vibescript::Result<()> {
    let q = &request.limits;
    if q.steps == 0
        || q.steps > 10_000_000
        || q.memory_bytes == 0
        || q.memory_bytes > 16 << 20
        || q.recursion == 0
        || q.recursion > 128
    {
        return Err(invalid(
            "limits must be positive: steps <= 10000000, memory_bytes <= 16777216, recursion <= 128",
        ));
    }
    if request.files.len() > 64 {
        return Err(invalid("at most 64 extra files are allowed"));
    }
    let capabilities = capabilities(request.capabilities)?;
    let mut engine = Engine::new();
    engine.set_strict_effects(true);
    engine.set_module_sources(request.files.clone())?;
    engine.set_output_writer(move |ctx, bytes| capture(&output, ctx, bytes));
    engine.set_error_writer(move |ctx, bytes| capture(&stderr, ctx, bytes));
    for capability in &capabilities {
        engine.declare_capability(capability)?;
    }
    if request.op == Operation::Format {
        response.source = Some(crate::format::format(&request.source));
        response.files = Some(
            request
                .files
                .into_iter()
                .map(|(name, text)| (name, crate::format::format(&text)))
                .collect(),
        );
        return Ok(());
    }
    let mut applied = 0;
    let mut seen = BTreeSet::new();
    let script = loop {
        let (script, diagnostics, error) = inspect(&engine, &request.source);
        if request.op == Operation::Fix && applied < 256 {
            // Apply one complete fix, then check the entire virtual project again:
            // offsets and dependent module types must always refer to the current text.
            let candidate = diagnostics.iter().find_map(|d| {
                let fix = d.applicable_fix()?;
                let file = d
                    .file
                    .as_deref()
                    .and_then(|file| std::str::from_utf8(file).ok());
                let text = match file {
                    Some(file) => request.files.get(file)?,
                    None => &request.source,
                };
                let next = fix.apply(text)?;
                (next != *text).then(|| (file.map(str::to_owned), next))
            });
            if let Some((file, next)) = candidate {
                if seen.insert((file.clone(), next.clone())) {
                    match file {
                        Some(file) => {
                            request.files.insert(file, next);
                        }
                        None => request.source = next,
                    }
                    applied += 1;
                    engine.set_module_sources(request.files.clone())?;
                    continue;
                }
            }
        }
        response.diagnostics = diagnostics_json(&diagnostics, &request.source, &request.files);
        if request.op == Operation::Fix {
            response.source = Some(request.source.clone());
            response.files = Some(request.files.clone());
            response.applied = Some(applied);
        }
        if let Some(error) = error {
            response.fail(&error, &request.source);
            return Ok(());
        }
        break script.unwrap();
    };
    if request.op != Operation::Run {
        return Ok(());
    }
    let args = vibescript::parse_json(request.args.get().as_bytes(), codec_options())?.value;
    let args = args
        .as_array()
        .ok_or_else(|| invalid("args must be a JSON array"))?;
    let (value, stats) = script.call_with_stats(
        request.entry.as_deref().unwrap_or("__main__"),
        args,
        CallOptions {
            capabilities,
            limits: Limits {
                steps: Some(q.steps),
                memory_bytes: Some(q.memory_bytes),
                recursion: q.recursion,
            },
            allow_require: true,
            ..CallOptions::default()
        },
    );
    response.stats = stats_json(stats);
    let encoded = vibescript::stringify_json(&value?, codec_options())?;
    let bytes = encoded.value.as_bytes().unwrap();
    if bytes.len() > INPUT_LIMIT {
        return Err(Error::new(
            ErrorKind::OutputLimit,
            "playground result exceeds 1048576 bytes",
        ));
    }
    response.result = Some(
        RawValue::from_string(String::from_utf8(bytes.to_vec()).expect("JSON is UTF-8"))
            .map_err(|error| Error::new(ErrorKind::Json, error.to_string()))?,
    );
    Ok(())
}

/// Reads one UTF-8 JSON request through EOF and writes one newline-terminated response.
///
/// Protocol and script errors are JSON responses; only stream I/O errors escape.
/// Input is capped at one MiB, and each captured output stream at 64 KiB.
pub fn serve(reader: impl Read, mut writer: impl Write) -> io::Result<()> {
    let mut input = Vec::new();
    reader
        .take(INPUT_LIMIT as u64 + 1)
        .read_to_end(&mut input)?;
    let mut response = Response::default();
    let output = Capture::default();
    let stderr = Capture::default();
    let result = if input.len() > INPUT_LIMIT {
        Err(invalid("request exceeds 1048576 bytes"))
    } else {
        serde_json::from_slice::<Request>(&input)
            .map_err(|error| invalid(format!("invalid request: {error}")))
            .and_then(|request| execute(request, &mut response, output.clone(), stderr.clone()))
    };
    if let Err(error) = result {
        response.fail(&error, "");
    }
    response.ok = response.error.is_none();
    response.output = lines(&output);
    response.stderr = lines(&stderr);
    serde_json::to_writer(&mut writer, &response)?;
    writer.write_all(b"\n")?;
    writer.flush()
}

#[cfg(test)]
mod tests;
