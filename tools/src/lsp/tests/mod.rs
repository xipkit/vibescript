//! Ports of the reference's `lsp_test.go` and `lsp_member_narrowing_test.go`,
//! driving the server in-process through its message handling.

use super::catalog::Catalog;
use super::server::{Inbound, Outbound};
use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

mod code_action_tests;
mod completion_tests;
mod diagnostics_tests;
mod docs_tests;
mod hover_tests;
mod narrowing_tests;
mod navigation_tests;
mod protocol_tests;
mod signature_tests;

pub(super) fn server() -> Server {
    Server::new()
}

/// A message with raw parameters, or none when `params` is `None`.
pub(super) fn message(method: &str, id: Option<&str>, params: Option<Value>) -> Inbound {
    Inbound {
        id: id.map(Box::from),
        method: method.to_owned(),
        params: params.map(|params| params.to_string().into_boxed_str()),
    }
}

/// Handles a message and decodes each reply from its wire form.
pub(super) fn handle(server: &mut Server, message: &Inbound) -> Vec<Value> {
    replies(server, message)
        .iter()
        .map(|reply| serde_json::from_str(&reply.json()).expect("replies are valid JSON"))
        .collect()
}

pub(super) fn replies(server: &mut Server, message: &Inbound) -> Vec<Outbound> {
    server.dispatch(message)
}

pub(super) fn document<'a>(server: &'a Server, uri: &str) -> &'a Document {
    server
        .document(uri)
        .unwrap_or_else(|| panic!("{uri} is not open"))
}

pub(super) fn open(server: &mut Server, uri: &str, text: &str) -> Vec<Value> {
    let params = json!({"textDocument": {"uri": uri, "text": text}});
    handle(server, &message("textDocument/didOpen", None, Some(params)))
}

pub(super) fn change(server: &mut Server, uri: &str, text: &str) -> Vec<Value> {
    let params = json!({"textDocument": {"uri": uri}, "contentChanges": [{"text": text}]});
    handle(
        server,
        &message("textDocument/didChange", None, Some(params)),
    )
}

pub(super) fn close(server: &mut Server, uri: &str) -> Vec<Value> {
    let params = json!({"textDocument": {"uri": uri}});
    handle(
        server,
        &message("textDocument/didClose", None, Some(params)),
    )
}

/// The diagnostics a didOpen publishes.
pub(super) fn open_diagnostics(server: &mut Server, uri: &str, text: &str) -> Vec<Value> {
    let replies = open(server, uri, text);
    assert_eq!(replies.len(), 1, "didOpen replies {replies:?}");
    assert_eq!(replies[0]["method"], "textDocument/publishDiagnostics");
    assert_eq!(replies[0]["params"]["uri"], uri);
    replies[0]["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .clone()
}

/// The single response to a request.
pub(super) fn request(server: &mut Server, method: &str, params: Value) -> Value {
    let replies = handle(server, &message(method, Some("1"), Some(params)));
    assert_eq!(replies.len(), 1, "{method} replies {replies:?}");
    assert_eq!(replies[0]["id"], 1);
    replies[0].clone()
}

pub(super) fn position(uri: &str, line: i64, character: i64) -> Value {
    json!({"textDocument": {"uri": uri}, "position": {"line": line, "character": character}})
}

pub(super) fn result(
    server: &mut Server,
    method: &str,
    uri: &str,
    line: i64,
    character: i64,
) -> Value {
    request(server, method, position(uri, line, character))["result"].clone()
}

/// The markdown a hover request returns against a server that opened `source`.
pub(super) fn hover_value(source: &str, line: i64, character: i64) -> String {
    let mut server = server();
    open(&mut server, "file:///tmp/test.vibe", source);
    hover_at(&mut server, "file:///tmp/test.vibe", line, character)
}

pub(super) fn hover_at(server: &mut Server, uri: &str, line: i64, character: i64) -> String {
    let result = result(server, "textDocument/hover", uri, line, character);
    assert_eq!(result["contents"]["kind"], "markdown", "{result}");
    result["contents"]["value"].as_str().unwrap().to_owned()
}

/// Completion items by label.
pub(super) fn completion_labels(
    server: &mut Server,
    uri: &str,
    line: i64,
    character: i64,
) -> BTreeMap<String, Value> {
    let result = result(server, "textDocument/completion", uri, line, character);
    assert_eq!(result["isIncomplete"], false);
    result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| (item["label"].as_str().unwrap().to_owned(), item.clone()))
        .collect()
}

/// The static keyword and builtin items, decoded.
pub(super) fn static_items() -> Vec<Value> {
    completion::static_entries(&Catalog::new())
        .iter()
        .map(|entry| serde_json::from_str(&server::completion_json(entry).encode()).unwrap())
        .collect()
}

pub(super) fn find<'a>(items: &'a [Value], label: &str) -> &'a Value {
    items
        .iter()
        .find(|item| item["label"] == label)
        .unwrap_or_else(|| panic!("missing completion item {label}"))
}

/// A documented builtin's signature without backticks.
pub(super) fn builtin_signature(name: &str) -> String {
    docs::builtin_docs()
        .get(name)
        .unwrap_or_else(|| panic!("builtin docs missing {name}"))
        .signature
        .replace('`', "")
}

pub(super) fn symbols(server: &mut Server, uri: &str) -> Vec<Value> {
    let response = request(
        server,
        "textDocument/documentSymbol",
        json!({"textDocument": {"uri": uri}}),
    );
    response["result"].as_array().unwrap().clone()
}

pub(super) fn names(symbols: &[Value]) -> Vec<&str> {
    symbols
        .iter()
        .map(|symbol| symbol["name"].as_str().unwrap())
        .collect()
}

pub(super) const NAVIGATION: &str = "def helper(n)
  n * 2
end

class Wallet
  def balance()
    1
  end

  def self.empty()
    Wallet.new
  end
end

enum Status
  Draft
  Published
end

def run()
  helper(1)
end
";

pub(super) const MODULE_NAVIGATION: &str = "module Billing
  LIMIT = 100

  module Codes
    PREFIX = \"B\"

    def self.tag
      PREFIX
    end
  end

  def self.code
    \"B-1\"
  end
end

class Account
  protected def guard
    1
  end

  public def shown
    2
  end
end

def run()
  Billing::LIMIT
end
";

/// The declaration a word resolves to in a document's navigation program.
pub(super) fn definition(server: &Server, uri: &str, word: &str) -> Option<Range> {
    let document = server.document(uri)?;
    navigation::definition(document.program.as_deref(), &document.lines, word)
}
