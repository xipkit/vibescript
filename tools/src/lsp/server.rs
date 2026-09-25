//! The transport-free protocol state machine.

use super::document::{
    CompletionItem, Diagnostic, Document, Options, Position, QuickFix, Range, SignatureHelp, Symbol,
};
use super::json::{self, Invalid, Json};
use super::{hover, navigation, text};
use serde_json::value::RawValue;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use vibescript::CancellationToken;

/// A Language Server Protocol server that handles one JSON-RPC message at a
/// time and returns the messages to send, so any transport can carry it.
///
/// It answers `initialize`, `shutdown`, `exit`, the full-text document sync
/// notifications, `textDocument/hover`, `completion`, `signatureHelp`,
/// `definition`, `documentSymbol` and `formatting`, and publishes diagnostics
/// on every open and change. Documents live only in memory; the server reads
/// the file system only to check the files a document requires.
///
/// ```
/// use vibescript_tools::lsp::Server;
///
/// let mut server = Server::new();
/// let open = r#"{"jsonrpc":"2.0","method":"textDocument/didOpen","params":
///     {"textDocument":{"uri":"file:///tmp/a.vibe","text":"def run(\n"}}}"#;
/// let replies = server.handle(open);
/// assert!(replies[0].contains(r#""method":"textDocument/publishDiagnostics""#));
///
/// let hover = r#"{"jsonrpc":"2.0","id":1,"method":"textDocument/hover","params":
///     {"textDocument":{"uri":"file:///tmp/a.vibe"},"position":{"line":0,"character":1}}}"#;
/// let replies = server.handle(hover);
/// assert!(replies[0].starts_with(r#"{"jsonrpc":"2.0","id":1,"result":{"contents""#));
///
/// server.handle(r#"{"jsonrpc":"2.0","method":"exit"}"#);
/// assert!(server.exit_requested());
/// ```
pub struct Server {
    documents: HashMap<String, Document>,
    options: Options,
    exit: bool,
    /// The document whose analysis is running, so a transport reading ahead
    /// can cancel it when a newer version arrives.
    pub(crate) in_flight: InFlight,
}

pub(crate) type InFlight = Arc<Mutex<Option<(String, CancellationToken)>>>;

impl Default for Server {
    fn default() -> Self {
        Self::new()
    }
}

impl Server {
    /// Creates a server with the default check [`Options`].
    pub fn new() -> Self {
        Self::with_options(Options::default())
    }

    /// Creates a server whose document checks use `options`. Cancelling
    /// `options.cancellation` stops any check in progress.
    pub fn with_options(options: Options) -> Self {
        Self {
            documents: HashMap::new(),
            options,
            exit: false,
            in_flight: InFlight::default(),
        }
    }

    /// Handles the JSON text of one request or notification and returns the
    /// JSON text of each response and notification to send, in order.
    ///
    /// Text that is not a JSON-RPC object is ignored, as are notifications the
    /// server does not know. Unknown requests fail with `-32601`, and request
    /// parameters of the wrong shape with `-32602`.
    pub fn handle(&mut self, message: &str) -> Vec<String> {
        match Inbound::parse(message.as_bytes()) {
            Some(message) => self.dispatch(&message).iter().map(Outbound::json).collect(),
            None => Vec::new(),
        }
    }

    /// Whether the client has sent `exit`, after which a transport should stop.
    pub fn exit_requested(&self) -> bool {
        self.exit
    }

    /// An open document, analyzed as last published.
    pub fn document(&self, uri: &str) -> Option<&Document> {
        self.documents.get(uri)
    }

    /// Handles one decoded message.
    pub(crate) fn dispatch(&mut self, message: &Inbound) -> Vec<Outbound> {
        let id = message.id.clone();
        let respond = |result: Json| match &id {
            Some(id) => vec![Outbound::Result {
                id: Some(id.clone()),
                result,
            }],
            None => Vec::new(),
        };
        let invalid = |text: &'static str| match &id {
            Some(id) => vec![Outbound::Error {
                id: id.clone(),
                code: -32602,
                message: text,
            }],
            None => Vec::new(),
        };
        match message.method.as_str() {
            // The reference answers even an initialize without an id.
            "initialize" => vec![Outbound::Result {
                id: id.clone(),
                result: capabilities(self.options.static_types),
            }],
            "initialized" => Vec::new(),
            "exit" => {
                self.exit = true;
                Vec::new()
            }
            "shutdown" => respond(Json::Null),
            "textDocument/didOpen" => {
                let Ok((uri, text)) = open_params(message) else {
                    return Vec::new();
                };
                self.publish(&uri, &text).into_iter().collect()
            }
            "textDocument/didChange" => {
                let Ok((uri, Some(text))) = change_params(message) else {
                    return Vec::new();
                };
                self.publish(&uri, &text).into_iter().collect()
            }
            "textDocument/didClose" => {
                let Ok(uri) = document_params(message) else {
                    return Vec::new();
                };
                if self.documents.remove(&uri).is_none() {
                    return Vec::new();
                }
                // A closed document's diagnostics would go stale; clear them.
                vec![diagnostics_notification(&uri, &[])]
            }
            _ if id.is_none() && REQUESTS.contains(&message.method.as_str()) => Vec::new(),
            "textDocument/formatting" => {
                let Ok(uri) = document_params(message) else {
                    return invalid("invalid formatting params");
                };
                match self.documents.get(&uri) {
                    Some(document) => respond(formatting_edits(&document.text, &document.lines)),
                    None => respond(Json::Null),
                }
            }
            "textDocument/codeAction" if self.options.static_types => {
                let Some(id) = &id else {
                    return Vec::new();
                };
                let Ok((uri, range)) = range_params(message) else {
                    return invalid("invalid codeAction params");
                };
                let actions = match self.documents.get(&uri) {
                    Some(document) => document
                        .code_actions(range)
                        .into_iter()
                        .map(|(diagnostic, fix)| code_action_json(&uri, diagnostic, fix))
                        .collect(),
                    None => Vec::new(),
                };
                vec![Outbound::Result {
                    id: Some(id.clone()),
                    result: Json::Array(actions),
                }]
            }
            "textDocument/definition" => {
                let Ok((uri, line, character)) = position_params(message) else {
                    return invalid("invalid definition params");
                };
                let lines = self.lines(&uri);
                let word = text::word_at(lines, line, character);
                let program = self.documents.get(&uri).and_then(|d| d.program.as_deref());
                let result = navigation::definition(program, lines, &word)
                    .map_or(Json::Null, |range| location(&uri, range));
                respond(result)
            }
            "textDocument/documentSymbol" => {
                let Ok(uri) = document_params(message) else {
                    return invalid("invalid documentSymbol params");
                };
                let symbols = match self.documents.get(&uri) {
                    Some(document) => document.symbol_tree().iter().map(symbol_json).collect(),
                    None => Vec::new(),
                };
                respond(Json::Array(symbols))
            }
            "textDocument/signatureHelp" => {
                let Ok((uri, line, character)) = position_params(message) else {
                    return invalid("invalid signatureHelp params");
                };
                let compiled = self.documents.get(&uri).and_then(|d| d.compiled.as_deref());
                let help = super::signature::help(
                    super::catalog::catalog(),
                    compiled,
                    self.lines(&uri),
                    line,
                    character,
                );
                respond(help.as_ref().map_or(Json::Null, signature_json))
            }
            "textDocument/completion" => {
                let Ok((uri, line, character)) = position_params(message) else {
                    return invalid("invalid completion params");
                };
                let items = match self.documents.get(&uri) {
                    Some(document) => document.completion_items(line, character),
                    None => super::completion::Request {
                        source: "",
                        lines: NO_TEXT,
                        line,
                        character,
                    }
                    .entries(None),
                };
                respond(Json::Object(vec![
                    ("isIncomplete", Json::Bool(false)),
                    (
                        "items",
                        Json::Array(items.iter().map(|item| completion_json(item)).collect()),
                    ),
                ]))
            }
            "textDocument/hover" => {
                let Ok((uri, line, character)) = position_params(message) else {
                    return invalid("invalid hover params");
                };
                let lines = self.lines(&uri);
                let word = text::word_at(lines, line, character);
                if word.is_empty() {
                    return respond(Json::Null);
                }
                let program = self.documents.get(&uri).and_then(|d| d.program.as_deref());
                let catalog = super::catalog::catalog();
                let value = hover::markdown(catalog, program, lines, line, character, &word);
                respond(Json::Object(vec![(
                    "contents",
                    Json::Object(vec![
                        ("kind", Json::str("markdown")),
                        ("value", Json::str(value)),
                    ]),
                )]))
            }
            _ => match id {
                Some(id) => vec![Outbound::Error {
                    id,
                    code: -32601,
                    message: "method not found",
                }],
                None => Vec::new(),
            },
        }
    }

    /// A document's lines; an unknown document reads as one empty line.
    pub(crate) fn lines(&self, uri: &str) -> &[String] {
        self.documents
            .get(uri)
            .map_or(NO_TEXT, |document| document.lines.as_slice())
    }

    /// Analyzes a document's new text and publishes its diagnostics, unless a
    /// newer version cancelled the analysis. Identical text republishes the
    /// last result without analysis.
    fn publish(&mut self, uri: &str, text: &str) -> Option<Outbound> {
        let token = self.options.cancellation.child_token();
        let options = Options {
            cancellation: token.clone(),
            ..self.options.clone()
        };
        *self.in_flight.lock().unwrap_or_else(|e| e.into_inner()) = Some((uri.to_owned(), token));
        match self.documents.get_mut(uri) {
            Some(document) => document.update(text, &options),
            None => {
                let document = Document::analyze(uri, text, &options);
                self.documents.insert(uri.to_owned(), document);
            }
        }
        *self.in_flight.lock().unwrap_or_else(|e| e.into_inner()) = None;
        let document = &self.documents[uri];
        if document.interrupted() {
            return None;
        }
        Some(diagnostics_notification(uri, document.diagnostics()))
    }
}

/// Requests that need an id; as notifications they are ignored.
const REQUESTS: &[&str] = &[
    "textDocument/formatting",
    "textDocument/definition",
    "textDocument/documentSymbol",
    "textDocument/signatureHelp",
    "textDocument/completion",
    "textDocument/hover",
];

const NO_TEXT: &[String] = &[String::new()];

/// One decoded client message.
#[derive(Clone, Debug)]
pub(crate) struct Inbound {
    /// The request id, verbatim; `None` for notifications and a `null` id.
    pub id: Option<Box<str>>,
    pub method: String,
    /// The raw parameters; `None` when absent, which differs from `null`.
    pub params: Option<Box<str>>,
}

impl Inbound {
    /// Decodes a payload the way the reference unmarshals its envelope: it
    /// must be a JSON object, and `jsonrpc` and `method` must be strings or
    /// `null` when present. Anything else is skipped.
    pub(crate) fn parse(payload: &[u8]) -> Option<Self> {
        let text = String::from_utf8_lossy(payload);
        let fields: HashMap<String, Box<RawValue>> = serde_json::from_str(&text).ok()?;
        let lookup = |name: &str| -> Option<&RawValue> {
            fields.get(name).map(AsRef::as_ref).or_else(|| {
                fields
                    .iter()
                    .find(|(key, _)| key.to_lowercase() == name)
                    .map(|(_, value)| value.as_ref())
            })
        };
        let string = |raw: Option<&RawValue>| -> Option<String> {
            match raw.map(|raw| serde_json::from_str::<serde_json::Value>(raw.get())) {
                None | Some(Ok(serde_json::Value::Null)) => Some(String::new()),
                Some(Ok(serde_json::Value::String(text))) => Some(text),
                Some(_) => None,
            }
        };
        string(lookup("jsonrpc"))?;
        let method = string(lookup("method"))?;
        let id = lookup("id")
            .map(RawValue::get)
            .filter(|raw| *raw != "null")
            .map(Box::from);
        let params = lookup("params").map(|raw| Box::from(raw.get()));
        Some(Self { id, method, params })
    }

    /// The document a text-sync notification names.
    pub(crate) fn document(&self) -> Option<String> {
        document_params(self).ok()
    }
}

/// One server message.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Outbound {
    Result {
        id: Option<Box<str>>,
        result: Json,
    },
    Error {
        id: Box<str>,
        code: i64,
        message: &'static str,
    },
    Notification {
        method: &'static str,
        params: Json,
    },
}

impl Outbound {
    /// Encodes the message with the reference's field order.
    pub(crate) fn json(&self) -> String {
        let version = ("jsonrpc", Json::str("2.0"));
        let message = match self {
            Self::Result { id, result } => {
                let mut fields = vec![version];
                if let Some(id) = id {
                    fields.push(("id", Json::Raw(id.clone())));
                }
                fields.push(("result", result.clone()));
                Json::Object(fields)
            }
            Self::Error { id, code, message } => Json::Object(vec![
                version,
                ("id", Json::Raw(id.clone())),
                (
                    "error",
                    Json::Object(vec![
                        ("code", Json::Int(*code)),
                        ("message", Json::str(*message)),
                    ]),
                ),
            ]),
            Self::Notification { method, params } => Json::Object(vec![
                version,
                ("method", Json::str(*method)),
                ("params", params.clone()),
            ]),
        };
        message.encode()
    }
}

/// The server's capabilities; a static-language server also offers quick
/// fixes as code actions.
fn capabilities(static_types: bool) -> Json {
    let mut capabilities = Vec::new();
    if static_types {
        capabilities.push((
            "codeActionProvider",
            Json::Object(vec![(
                "codeActionKinds",
                Json::Array(vec![Json::str("quickfix")]),
            )]),
        ));
    }
    capabilities.extend([
        (
            "completionProvider",
            Json::Object(vec![
                ("resolveProvider", Json::Bool(false)),
                ("triggerCharacters", Json::Array(vec![Json::str(".")])),
            ]),
        ),
        ("definitionProvider", Json::Bool(true)),
        ("documentFormattingProvider", Json::Bool(true)),
        ("documentSymbolProvider", Json::Bool(true)),
        ("hoverProvider", Json::Bool(true)),
        (
            "signatureHelpProvider",
            Json::Object(vec![(
                "triggerCharacters",
                Json::Array(vec![Json::str("("), Json::str(",")]),
            )]),
        ),
        ("textDocumentSync", Json::Int(1)),
    ]);
    Json::Object(vec![("capabilities", Json::Object(capabilities))])
}

pub(crate) fn diagnostics_notification(uri: &str, diagnostics: &[Diagnostic]) -> Outbound {
    Outbound::Notification {
        method: "textDocument/publishDiagnostics",
        params: Json::Object(vec![
            ("uri", Json::str(uri)),
            (
                "diagnostics",
                Json::Array(diagnostics.iter().map(diagnostic_json).collect()),
            ),
        ]),
    }
}

fn position_json(position: Position) -> Json {
    Json::Object(vec![
        ("line", Json::Int(i64::from(position.line))),
        ("character", Json::Int(i64::from(position.character))),
    ])
}

fn range_json(range: Range) -> Json {
    Json::Object(vec![
        ("start", position_json(range.start)),
        ("end", position_json(range.end)),
    ])
}

/// A position as the reference encodes a map: keys sorted.
fn sorted_position(position: Position) -> Json {
    Json::Object(vec![
        ("character", Json::Int(i64::from(position.character))),
        ("line", Json::Int(i64::from(position.line))),
    ])
}

fn diagnostic_json(diagnostic: &Diagnostic) -> Json {
    let mut fields = vec![
        ("range", range_json(diagnostic.range)),
        ("severity", Json::Int(i64::from(diagnostic.severity.code()))),
    ];
    if let Some(code) = &diagnostic.code {
        fields.push(("code", Json::str(code.clone())));
    }
    fields.push(("source", Json::str("vibes-lsp")));
    fields.push(("message", Json::str(diagnostic.message.clone())));
    Json::Object(fields)
}

/// A quick fix as a `CodeAction` whose workspace edit changes `uri`.
fn code_action_json(uri: &str, diagnostic: &Diagnostic, fix: &QuickFix) -> Json {
    let edits = fix
        .edits
        .iter()
        .map(|(range, text)| {
            Json::Object(vec![
                ("range", range_json(*range)),
                ("newText", Json::str(text.clone())),
            ])
        })
        .collect();
    let changes = Json::Object(vec![(
        "changes",
        Json::Map(vec![(uri.to_owned(), Json::Array(edits))]),
    )]);
    Json::Object(vec![
        ("title", Json::str(fix.title.clone())),
        ("kind", Json::str("quickfix")),
        (
            "diagnostics",
            Json::Array(vec![diagnostic_json(diagnostic)]),
        ),
        ("isPreferred", Json::Bool(fix.preferred)),
        ("edit", changes),
    ])
}

fn location(uri: &str, range: Range) -> Json {
    Json::Object(vec![
        (
            "range",
            Json::Object(vec![
                ("end", sorted_position(range.end)),
                ("start", sorted_position(range.start)),
            ]),
        ),
        ("uri", Json::str(uri)),
    ])
}

pub(crate) fn symbol_json(symbol: &Symbol) -> Json {
    let mut fields = vec![
        ("name", Json::str(symbol.name.clone())),
        ("kind", Json::Int(i64::from(symbol.kind.code()))),
        ("range", range_json(symbol.range)),
        ("selectionRange", range_json(symbol.selection_range)),
    ];
    if !symbol.children.is_empty() {
        fields.push((
            "children",
            Json::Array(symbol.children.iter().map(symbol_json).collect()),
        ));
    }
    Json::Object(fields)
}

fn signature_json(help: &SignatureHelp) -> Json {
    let parameters = help
        .parameters
        .iter()
        .map(|label| Json::Object(vec![("label", Json::str(label.clone()))]))
        .collect();
    Json::Object(vec![
        (
            "activeParameter",
            Json::Int(i64::from(help.active_parameter)),
        ),
        ("activeSignature", Json::Int(0)),
        (
            "signatures",
            Json::Array(vec![Json::Object(vec![
                ("label", Json::str(help.label.clone())),
                ("parameters", Json::Array(parameters)),
            ])]),
        ),
    ])
}

/// A completion item as the reference encodes its maps: keys sorted.
pub(crate) fn completion_json(item: &CompletionItem) -> Json {
    let mut fields = vec![("detail", Json::str(item.detail.clone()))];
    if let Some(documentation) = &item.documentation {
        fields.push((
            "documentation",
            Json::Object(vec![
                ("kind", Json::str("markdown")),
                ("value", Json::str(documentation.clone())),
            ]),
        ));
    }
    fields.push(("kind", Json::Int(i64::from(item.kind.code()))));
    fields.push(("label", Json::str(item.label.clone())));
    Json::Object(fields)
}

/// One full-document edit when formatting changes the text, or none.
pub(crate) fn formatting_edits(source: &str, lines: &[String]) -> Json {
    let formatted = crate::format::format(source);
    if formatted == source {
        return Json::Array(Vec::new());
    }
    let last = lines.len().saturating_sub(1);
    let text = lines.get(last).map_or("", String::as_str);
    let end = Position {
        line: u32::try_from(last).unwrap_or(u32::MAX),
        character: u32::try_from(text::utf16_character(text, text.chars().count()))
            .unwrap_or(u32::MAX),
    };
    Json::Array(vec![Json::Object(vec![
        ("newText", Json::str(formatted)),
        (
            "range",
            Json::Object(vec![
                ("end", sorted_position(end)),
                ("start", sorted_position(Position::default())),
            ]),
        ),
    ])])
}

fn open_params(message: &Inbound) -> Result<(String, String), Invalid> {
    let params = json::params(message.params.as_deref())?;
    let document = json::nested(json::root(&params), "textDocument")?;
    Ok((json::text(document, "uri")?, json::text(document, "text")?))
}

/// The document and the last full-text change, if any.
pub(crate) fn change_params(message: &Inbound) -> Result<(String, Option<String>), Invalid> {
    let params = json::params(message.params.as_deref())?;
    let root = json::root(&params);
    let uri = json::text(json::nested(root, "textDocument")?, "uri")?;
    let mut latest = None;
    for change in json::structs(root, "contentChanges")? {
        latest = Some(json::text(change, "text")?);
    }
    Ok((uri, latest))
}

fn document_params(message: &Inbound) -> Result<String, Invalid> {
    let params = json::params(message.params.as_deref())?;
    json::text(json::nested(json::root(&params), "textDocument")?, "uri")
}

/// The document and range a code action request names.
fn range_params(message: &Inbound) -> Result<(String, Range), Invalid> {
    let params = json::params(message.params.as_deref())?;
    let root = json::root(&params);
    let uri = json::text(json::nested(root, "textDocument")?, "uri")?;
    let range = json::nested(root, "range")?;
    let position = |name| -> Result<Position, Invalid> {
        let position = json::nested(range, name)?;
        let number = |field| {
            json::int(position, field).map(|value| u32::try_from(value.max(0)).unwrap_or(u32::MAX))
        };
        Ok(Position::new(number("line")?, number("character")?))
    };
    Ok((
        uri,
        Range {
            start: position("start")?,
            end: position("end")?,
        },
    ))
}

fn position_params(message: &Inbound) -> Result<(String, i64, i64), Invalid> {
    let params = json::params(message.params.as_deref())?;
    let root = json::root(&params);
    let uri = json::text(json::nested(root, "textDocument")?, "uri")?;
    let position = json::nested(root, "position")?;
    Ok((
        uri,
        json::int(position, "line")?,
        json::int(position, "character")?,
    ))
}
