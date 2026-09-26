mod common;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, Engine, ErrorKind, Limits, Value, stringify_json};

const ENUMS: &str = "enum Status\nDraft\nDone\nend\nenum Review\nDraft\nend\n";

fn evaluate(source: &str) -> serde_json::Value {
    let result = Engine::new()
        .compile(&format!("{ENUMS}{source}"))
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let json = stringify_json(&result.value, CallOptions::default()).unwrap();
    serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap()
}

/// The code and offset in `source` of each static diagnostic that refuses
/// `source` after the enums.
fn refused(source: &str) -> Vec<(String, usize)> {
    let error = common::static_engine()
        .compile(&format!("{ENUMS}{source}"))
        .err()
        .unwrap_or_else(|| panic!("{source} compiled"));
    error
        .diagnostics()
        .iter()
        .map(|d| (d.code.to_string(), d.span.start - ENUMS.len()))
        .collect()
}

fn at(source: &str, code: &str, text: &str) -> (String, usize) {
    (code.to_owned(), source.find(text).unwrap())
}

#[test]
fn scalar_boundaries_are_strict_and_union_coercion_precedes_any() {
    // Host arguments are checked when the call starts.
    let script = Engine::new()
        .compile(&format!(
            "{ENUMS}def int(x:int)\nx\nend\ndef float(x:float)\nx\nend\n\
             def string(x:string)\nx\nend\ndef status(x:Status)\nx\nend\n\
             def review -> Review\nReview::Draft\nend"
        ))
        .unwrap();
    let review = script
        .call("review", &[], CallOptions::default())
        .unwrap()
        .value;
    for (function, value) in [
        ("int", Value::float(1.0)),
        ("float", Value::int(1)),
        ("string", Value::symbol(b"draft".to_vec())),
        ("status", Value::bytes("draft")),
        ("status", review),
    ] {
        let error = script
            .call(function, &[value], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{function}");
    }
    // Calls inside a program are checked before it runs, and so are the
    // types an annotation names.
    for (source, code, text) in [
        ("def typed(x:int)\nx\nend\ntyped(1.0)", "V0101", "1.0"),
        ("def typed(x:float)\nx\nend\ntyped(1)", "V0101", "1)"),
        (
            "def typed(x:string)\nx\nend\ntyped(:draft)",
            "V0101",
            ":draft",
        ),
        (
            "def typed(x:Status)\nx\nend\ntyped(\"draft\")",
            "V0101",
            "\"draft\"",
        ),
        (
            "def typed(x:Status)\nx\nend\ntyped(Review::Draft)",
            "V0101",
            "Review",
        ),
        (
            "def typed(x:any | Missing)\nx\nend\ntyped(7)",
            "V0116",
            "Missing",
        ),
        (
            "def typed(x:array<Missing>?)\nx\nend\ntyped(nil)",
            "V0116",
            "Missing",
        ),
        (
            "def typed(x:{state?:Missing})\nx\nend\ntyped({})",
            "V0116",
            "Missing",
        ),
        ("def unused(x:Missing)\nx\nend\n7", "V0116", "Missing"),
    ] {
        assert_eq!(refused(source), [at(source, code, text)], "{source}");
    }
    assert_eq!(
        evaluate(
            "def typed(a:any | Status,b:symbol | Status,c:number,d:Status?)->array<any>\n[a.is_type?(:Status) ? a.name : nil,b==:draft,c,d]\nend\ntyped(:draft,:draft,10**30,nil)"
        ),
        serde_json::from_str::<serde_json::Value>(
            "[\"Draft\",true,1000000000000000000000000000000,null]"
        )
        .unwrap()
    );
}

#[test]
fn defaults_keywords_captures_and_block_destructuring_normalize_values() {
    assert_eq!(
        evaluate(
            "def typed(first:Status=:draft,*rest:array<Status>,last:Status,**extra:hash<string,Status>)->array<Status>\n[first]+rest+[last]+extra.values\nend\ntyped(:done,:draft,last: :done,other: :draft).map{|x|x.name}"
        ),
        serde_json::json!(["Done", "Draft", "Done", "Draft"])
    );
    assert_eq!(
        evaluate(
            "def typed(*, first:Status=:draft) -> string\nfirst.name\nend\nrows: array<[Status, Status, Status]> = [[:draft,:done,:draft]]\n[typed(),typed(first: :done),rows.map{|(first:Status,*rest:array<Status>)|[first.name,rest.map{|x|x.name}]}]"
        ),
        serde_json::json!(["Draft", "Done", [["Draft", ["Done", "Draft"]]]])
    );
    assert_eq!(
        evaluate(
            "def typed(*, opts: { previous: int? } = {previous:nil}, empty: hash<string, int> = {}) -> array<any>\n[opts,empty]\nend\npairs: array<[Status, int]> = [[:draft,2]]\n[typed(),pairs.map{|(state:Status,n:int)|[state.name,n]}]"
        ),
        serde_json::json!([[{"previous":null},{}],[["Draft",2]]])
    );
}

#[test]
fn shapes_preserve_optional_fields_extras_and_collection_values() {
    assert_eq!(
        evaluate(
            "type Packet = {before: array<int>, state: Status, after: array<int>, previous: Status?}\n\
             def typed(packet:{state:Status,previous?:Status?,...})->{state:Status,...}\npacket\nend\n\
             a: Packet={before:[1],state: :draft,after:[2],previous:nil};b=typed(a).as(Packet);b[\"before\"].push(3);b[\"after\"].push(4);[a,b,b[\"state\"].name,b.keys]"
        ),
        serde_json::json!([
            {"before":[1],"state":"draft","after":[2],"previous":null},
            {"before":[1,3],"state":"draft","after":[2,4],"previous":null},
            "Draft",["before","state","after","previous"]
        ])
    );
    for (ty, value, host, code, text) in [
        ("{x:int}", "{}", Value::hash(vec![]), "V0101", "{}"),
        (
            "{x:int}",
            "{x:1,y:2}",
            Value::hash(vec![
                (b"x".to_vec(), Value::int(1)),
                (b"y".to_vec(), Value::int(2)),
            ]),
            "V0110",
            "2}",
        ),
        (
            "{x?:int}",
            "{x:nil}",
            Value::hash(vec![(b"x".to_vec(), Value::nil())]),
            "V0101",
            "nil",
        ),
    ] {
        let source = format!("def typed(x:{ty})\nx\nend\ntyped({value})");
        assert_eq!(refused(&source), [at(&source, code, text)], "{source}");
        let error = Engine::new()
            .compile(&format!("def typed(x:{ty})\nx\nend"))
            .unwrap()
            .call("typed", &[host], CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{ty}");
    }
    // A dictionary's keys are strings, so no value has this type.
    let script = Engine::new()
        .compile("def typed(x:hash<int,any>)\nx\nend\ntyped({})")
        .unwrap();
    assert_eq!(
        script.run(CallOptions::default()).unwrap_err().kind,
        ErrorKind::Type
    );
    assert_eq!(
        evaluate(
            "def typed(x:{\"valid?\":bool,optional?:int}) -> {\"valid?\":bool,optional?:int}\nx\nend\ntyped({\"valid?\":true})"
        ),
        serde_json::json!({"valid?":true})
    );
}

#[test]
fn return_annotations_apply_to_all_function_exit_paths() {
    for source in [
        "def typed->Status\n:draft\nend\ntyped.name",
        "def typed->Status\nreturn :draft\nend\ntyped.name",
        "def typed->Status\n[1].each{return (:draft)}\n:done\nend\ntyped.name",
        "def typed(&block: () -> Status)->Status\nyield\nend\ntyped{break (:draft)}.name",
        "def typed(x:Status=([1].map{return (:draft)}))->Status\nx\nend\ntyped().name",
    ] {
        assert_eq!(evaluate(source), serde_json::json!("Draft"), "{source}");
        let missing = source.replace(":draft", ":missing");
        // A member the enum lacks is refused before running where the
        // checker sees the value leave the function; a `return` in a
        // default is still checked when it runs.
        if source.contains("x:Status=") {
            let error = Engine::new()
                .compile(&format!("{ENUMS}{missing}"))
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err();
            assert_eq!(error.kind, ErrorKind::Type, "{missing}");
        } else {
            assert_eq!(
                refused(&missing),
                [at(&missing, "V0206", ":missing")],
                "{missing}"
            );
        }
    }
}

#[test]
fn named_types_use_definition_scopes_and_nominal_host_identity() {
    // A type is named by its declaration or a `type` alias; a local holding
    // a type does not name one.
    for (source, expected) in [
        (
            "T=Status;[1].map{[[Review,:draft]].map{|T,x:T|x.enum.name}}",
            vec![("V0116", "T|")],
        ),
        (
            "Alias=JSON;Alias.Status=Status;[:draft].map{|x:Alias.status|x.name}",
            vec![("V0203", "Status="), ("V0116", "x:Alias")],
        ),
        (
            "T=Status;t=Status;[:draft].map{|x:t|x.name}",
            vec![("V0116", "t|")],
        ),
        (
            "def typed(T,x:T)\nx\nend\ntyped(Status,:draft)",
            vec![("V0118", "T,"), ("V0116", "T,")],
        ),
        (
            "def typed->T\nT=Status;:draft\nend\ntyped",
            vec![("V0116", "T\n")],
        ),
        (
            "STatus=Status;StAtUs=Review;[:draft].map{|x:status|x}",
            vec![("V0116", "status|")],
        ),
    ] {
        let expected: Vec<(String, usize)> = expected
            .into_iter()
            .map(|(code, text)| at(source, code, text))
            .collect();
        assert_eq!(refused(source), expected, "{source}");
    }
    let source = format!(
        "{ENUMS}def member -> Status\nStatus::Draft\nend\ndef typed(x:Status)->Status\nx\nend"
    );
    let script = Engine::new().compile(&source).unwrap();
    let member = script.call("member", &[], CallOptions::default()).unwrap();
    let result = script
        .call("typed", &[member.value], CallOptions::default())
        .unwrap();
    assert_eq!(
        result.value.as_enum_member(),
        Some(("Status", "Draft", "draft"))
    );
    assert!(result.stats.retained_memory_bytes > 0);
    let foreign = Engine::new()
        .compile(&source)
        .unwrap()
        .call("member", &[], CallOptions::default())
        .unwrap();
    assert_eq!(
        script
            .call("typed", &[foreign.value], CallOptions::default())
            .unwrap_err()
            .kind,
        ErrorKind::Type
    );
}

#[test]
fn invalid_typed_values_stop_later_defaults_and_host_effects() {
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = calls.clone();
    let mut engine = Engine::new();
    engine.register("effect", move |_, _| {
        seen.fetch_add(1, Ordering::SeqCst);
        Ok(Value::int(1))
    });
    // A host argument of the wrong type stops the call before its defaults.
    let error = engine
        .compile("def typed(x:int,y:int=effect().as(int))\neffect()\nend")
        .unwrap()
        .call("typed", &[Value::bytes("bad")], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // A default of the wrong type fails when it is used, before the later
    // defaults run; the checker does not check defaults yet.
    let error = engine
        .compile("def typed(x:int=\"bad\",y:int=effect().as(int))\neffect()\nend\ntyped()")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Type);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // Inside a program, the others are refused before anything runs.
    let mut engine = common::static_engine();
    engine.register("effect", |_, _| panic!("effect ran"));
    for (source, texts) in [
        (
            "def typed(x:int,y:int=effect().as(int))\neffect()\nend\ntyped(\"bad\")",
            vec!["\"bad\")"],
        ),
        (
            "def typed->int\n\"bad\"\nend\ntyped;effect()",
            vec!["\"bad\""],
        ),
        ("[\"bad\"].map{|x:int|effect()}", vec!["x:int"]),
        (
            "[[\"bad\",1]].map{|(x:int,y:int)|effect()}",
            vec!["x:int", "y:int"],
        ),
    ] {
        let error = engine.compile(source).err().unwrap();
        let found: Vec<(String, usize)> = error
            .diagnostics()
            .iter()
            .map(|d| (d.code.to_string(), d.span.start))
            .collect();
        let expected: Vec<(String, usize)> = texts
            .into_iter()
            .map(|text| at(source, "V0101", text))
            .collect();
        assert_eq!(found, expected, "{source}");
    }
}

#[test]
fn repeated_coercions_release_storage_and_preserve_host_inputs() {
    let script = Engine::new()
        .compile(&format!("{ENUMS}def typed(x:array<Status>)->array<Status>\nx\nend\ndef run(input: array<Status>) -> array<Status>\ni=0;output=input;while i<500;output=typed(input);i+=1;end;output\nend"))
        .unwrap();
    let input = Value::array(vec![Value::symbol(b"draft".to_vec()); 8]);
    for _ in 0..3 {
        let result = script
            .call(
                "run",
                std::slice::from_ref(&input),
                CallOptions {
                    limits: Limits {
                        memory_bytes: Some(16384),
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                },
            )
            .unwrap();
        assert!(result.stats.peak_memory_bytes < 16384);
        assert!(result.stats.retained_memory_bytes < 4096);
        assert!(
            result
                .value
                .as_array()
                .unwrap()
                .iter()
                .all(|x| x.as_enum_member() == Some(("Status", "Draft", "draft")))
        );
    }
    assert!(
        input
            .as_array()
            .unwrap()
            .iter()
            .all(|x| x.type_name() == "symbol")
    );
}
