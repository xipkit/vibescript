//! Static diagnostics with codes, and their fixes as code actions.

use super::*;

fn static_server() -> Server {
    Server::with_options(Options {
        static_types: true,
        ..Options::default()
    })
}

fn code_actions(server: &mut Server, uri: &str, range: Value) -> Vec<Value> {
    let params = json!({
        "textDocument": {"uri": uri},
        "range": range,
        "context": {"diagnostics": []},
    });
    handle(
        server,
        &message("textDocument/codeAction", Some("7"), Some(params)),
    )
}

const URI: &str = "file:///tmp/names.vibe";
const SOURCE: &str = "names = %w[ada grace]\nn = names.size\nok = n.eql?(2)\n";

#[test]
fn static_diagnostics_carry_their_codes() {
    let mut server = static_server();
    let published = open(&mut server, URI, SOURCE);
    let diagnostics = &published[0]["params"]["diagnostics"];
    let codes: Vec<&str> = diagnostics
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["V0410", "V0401", "V0403"]);
    assert_eq!(
        diagnostics[1],
        json!({
            "range": {"start": {"line": 1, "character": 10}, "end": {"line": 1, "character": 14}},
            "severity": 1,
            "code": "V0401",
            "source": "vibes-lsp",
            "message": "`size` was removed; use `length`",
        })
    );
}

#[test]
fn fixes_are_offered_as_quick_fixes_for_the_range() {
    let mut server = static_server();
    open(&mut server, URI, SOURCE);
    let range = json!({"start": {"line": 1, "character": 11}, "end": {"line": 1, "character": 11}});
    let reply = code_actions(&mut server, URI, range);
    assert_eq!(reply[0]["id"], 7);
    let actions = reply[0]["result"].as_array().unwrap();
    assert_eq!(actions.len(), 1, "{actions:?}");
    let action = &actions[0];
    assert_eq!(action["title"], "use `length`");
    assert_eq!(action["kind"], "quickfix");
    assert_eq!(action["isPreferred"], true);
    assert_eq!(action["diagnostics"][0]["code"], "V0401");
    assert_eq!(
        action["edit"]["changes"][URI],
        json!([{
            "range": {"start": {"line": 1, "character": 4}, "end": {"line": 1, "character": 14}},
            "newText": "names.length",
        }])
    );
    // Applying the edit leaves nothing for that diagnostic.
    let fixed = "names = %w[ada grace]\nn = names.length\nok = n.eql?(2)\n";
    let published = change(&mut server, URI, fixed);
    let codes: Vec<&str> = published[0]["params"]["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, ["V0410", "V0403"]);
}

#[test]
fn a_suggestion_is_not_preferred_and_ranges_select_actions() {
    let mut server = static_server();
    open(&mut server, URI, SOURCE);
    let range = json!({"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 14}});
    let actions = code_actions(&mut server, URI, range)[0]["result"].clone();
    assert_eq!(actions.as_array().unwrap().len(), 1, "{actions:?}");
    assert_eq!(actions[0]["isPreferred"], false);
    assert_eq!(actions[0]["diagnostics"][0]["code"], "V0403");
    let all = json!({"start": {"line": 0, "character": 0}, "end": {"line": 3, "character": 0}});
    let actions = code_actions(&mut server, URI, all.clone())[0]["result"].clone();
    assert_eq!(actions.as_array().unwrap().len(), 3, "{actions:?}");
    let unknown = code_actions(&mut server, "file:///tmp/other.vibe", all);
    assert_eq!(unknown[0]["result"], json!([]));
}

#[test]
fn the_capability_and_the_request_need_static_types() {
    let initialize = message("initialize", Some("1"), Some(json!({})));
    let plain = handle(&mut server(), &initialize);
    assert!(
        plain[0]["result"]["capabilities"]
            .get("codeActionProvider")
            .is_none()
    );
    let typed = handle(&mut static_server(), &initialize);
    assert_eq!(
        typed[0]["result"]["capabilities"]["codeActionProvider"],
        json!({"codeActionKinds": ["quickfix"]})
    );
    let mut plain = server();
    let published = open(&mut plain, URI, SOURCE);
    assert!(
        published[0]["params"]["diagnostics"][0]
            .get("code")
            .is_none()
    );
    let range = json!({"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}});
    let reply = code_actions(&mut plain, URI, range);
    assert_eq!(reply[0]["error"]["code"], -32601);
}

#[test]
fn documents_answer_code_actions_without_the_protocol() {
    let options = Options {
        static_types: true,
        ..Options::default()
    };
    let document = Document::analyze(URI, SOURCE, &options);
    let range = Range {
        start: Position::new(0, 8),
        end: Position::new(0, 8),
    };
    let actions = document.code_actions(range);
    assert_eq!(actions.len(), 1);
    let (diagnostic, fix) = actions[0];
    assert_eq!(diagnostic.code.as_deref(), Some("V0410"));
    assert!(fix.preferred);
    assert_eq!(fix.edits[0].1, "[\"ada\", \"grace\"]");
}
