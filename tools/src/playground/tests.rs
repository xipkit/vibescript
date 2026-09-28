use super::*;

fn request(value: Json) -> Json {
    let mut output = Vec::new();
    serve(serde_json::to_vec(&value).unwrap().as_slice(), &mut output).unwrap();
    serde_json::from_slice(&output).unwrap()
}

#[test]
fn run_captures_streams_and_calls_an_entry_with_json_arguments() {
    let response = request(json!({"op":"run", "source":
        "def greet(name: string, n: int) -> int\n print name\n puts \"!\"\n warn \"warning\"\n n + 1\nend", 
        "entry":"greet", "args":["Ada", 41]}));
    assert_eq!(response["error"], Json::Null, "{response}");
    assert_eq!(response["output"], json!(["Ada!"]));
    assert_eq!(response["stderr"], json!(["warning"]));
    assert_eq!(response["result"], 42);
    assert!(response["stats"]["steps"].as_u64().unwrap() > 0);
    assert!(response["stats"]["peak_memory_bytes"].as_u64().unwrap() > 0);
}

#[test]
fn check_reports_cli_diagnostics_and_fix_repairs_optional_index() {
    let source = "def run -> int\n [1][0] + 1\nend\n";
    let checked = request(json!({"op":"check", "source":source}));
    let diagnostics = Engine::new()
        .compile(source)
        .err()
        .unwrap()
        .diagnostics()
        .to_vec();
    assert_eq!(
        checked["diagnostics"],
        json!(diagnostics_json(&diagnostics, source, &BTreeMap::new()))
    );
    assert!(
        !checked["diagnostics"][0]["fixes"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{checked}"
    );
    let fixed = request(json!({"op":"fix", "source":source}));
    assert_eq!(fixed["ok"], true, "{fixed}");
    assert!(fixed["applied"].as_u64().unwrap() > 0);
    let run = request(json!({"op":"run", "source":fixed["source"], "entry":"run"}));
    assert_eq!(run["result"], 2, "{run}");
}

#[test]
fn check_never_executes_and_format_uses_the_canonical_formatter() {
    let checked = request(json!({"op":"check", "source":"puts \"not run\"\nloop { }"}));
    assert_eq!(checked["ok"], true, "{checked}");
    assert_eq!(checked["output"], json!([]));
    assert_eq!(checked["stats"]["steps"], 0);
    let formatted =
        request(json!({"op":"format", "source":"1 + 2  \r\n\r\n", "files":{"x.vibe":"3  "}}));
    assert_eq!(formatted["source"], "1 + 2\n");
    assert_eq!(formatted["files"]["x.vibe"], "3\n");
}

#[test]
fn preview_capability_is_typed_and_returns_the_arguments() {
    let capability = json!({"name":"email", "members":[{"name":"send", "behavior":"preview", "signature":{
        "params":[{"name":"to","type":"string"},{"name":"subject","type":"string"},{"name":"body","type":"string"}],
        "result":"{ to: string, subject: string, body: string, status: string }"
    }}]});
    let response = request(
        json!({"op":"run", "source":"email.send(\"a@b.c\", \"Hello\", \"Body\")", "capabilities":[capability.clone()]}),
    );
    assert_eq!(
        response["result"],
        json!({"to":"a@b.c","subject":"Hello","body":"Body","status":"preview"}),
        "{response}"
    );
    let rejected = request(
        json!({"op":"check", "source":"email.send(1, \"Hello\", \"Body\")", "capabilities":[capability]}),
    );
    assert_eq!(rejected["ok"], false);
    assert_eq!(rejected["diagnostics"][0]["code"], "V0101");
    let absent = request(json!({"op":"check", "source":"email.send(\"a\", \"b\", \"c\")"}));
    assert_eq!(absent["diagnostics"][0]["code"], "V0201");
}

#[test]
fn memory_modules_resolve_relative_imports_and_fix_the_correct_file() {
    let source = "require(\"lib/helper\").answer";
    let files = json!({"lib/helper.vibe":"def answer -> int\n require('../numbers').value\nend", "numbers.vibe":"def value -> int\n [41][0] + 1\nend"});
    let checked = request(json!({"op":"check", "source":source, "files":files}));
    assert_eq!(
        checked["diagnostics"][0]["file"], "numbers.vibe",
        "{checked}"
    );
    assert_eq!(checked["diagnostics"][0]["span"]["line"], 2);
    let fixed = request(json!({"op":"fix", "source":source, "files":files}));
    assert_eq!(fixed["ok"], true, "{fixed}");
    assert_eq!(fixed["source"], source);
    let response = request(json!({"op":"run", "source":source, "files":fixed["files"]}));
    assert_eq!(response["result"], 42, "{response}");
}

#[test]
fn step_memory_and_recursion_exhaustion_keep_counters() {
    for (source, limits, kind) in [
        ("puts \"started\"\nloop { }", json!({"steps":100}), "Steps"),
        ("\"a\" * 1000000", json!({"memory_bytes":4096}), "Memory"),
        (
            "def recurse -> int\n recurse\nend\nrecurse",
            json!({"recursion":4}),
            "Recursion",
        ),
    ] {
        let response = request(json!({"op":"run", "source":source, "limits":limits}));
        assert_eq!(response["error"]["kind"], kind, "{response}");
        assert!(response["stats"]["steps"].as_u64().unwrap() > 0);
        assert!(response["error"]["location"].is_object(), "{response}");
        if kind == "Steps" {
            assert_eq!(response["output"], json!(["started"]));
        }
    }
}

#[test]
fn malformed_requests_syntax_and_non_json_results_are_structured_errors() {
    for value in [
        json!({}),
        json!({"op":"run","source":"1","limits":{"steps":0}}),
        json!({"op":"run","source":"1","unexpected":true}),
        json!({"op":"run","source":"1","files":{"../secret.vibe":"1"}}),
        json!({"op":"run","source":"1","args":{}}),
    ] {
        let response = request(value);
        assert_eq!(response["error"]["kind"], "Argument", "{response}");
    }
    let syntax = request(json!({"op":"check","source":"def ("}));
    assert_eq!(syntax["diagnostics"][0]["code"], "V0001");
    let result = request(json!({"op":"run","source":"1.seconds"}));
    assert_eq!(result["error"]["kind"], "Json", "{result}");
    let mut output = Vec::new();
    serve(b"{} {}".as_slice(), &mut output).unwrap();
    assert_eq!(
        serde_json::from_slice::<Json>(&output).unwrap()["error"]["kind"],
        "Argument"
    );
}

#[test]
fn no_filesystem_fallback_and_output_is_bounded() {
    let response = request(json!({"op":"run","source":"require(\"Cargo.toml\")"}));
    assert_eq!(response["ok"], false);
    let output = request(json!({"op":"run","source":"100.times { print(\"x\" * 1000) }"}));
    assert_eq!(output["error"]["kind"], "OutputLimit", "{output}");
    assert!(output["output"][0].as_str().unwrap().len() <= OUTPUT_LIMIT);
}

#[test]
fn large_integers_round_trip_without_float_conversion() {
    let input = br#"{"op":"run","source":"def identity(n: int) -> int\n n\nend","entry":"identity","args":[123456789012345678901234567890]}"#;
    let mut output = Vec::new();
    serve(input.as_slice(), &mut output).unwrap();
    assert!(
        String::from_utf8(output)
            .unwrap()
            .contains("\"result\":123456789012345678901234567890")
    );
}
