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

#[test]
fn scalar_boundaries_are_strict_and_union_coercion_precedes_any() {
    for source in [
        "def typed(x:int)\nx\nend\ntyped(1.0)",
        "def typed(x:float)\nx\nend\ntyped(1)",
        "def typed(x:string)\nx\nend\ntyped(:draft)",
        "def typed(x:Status)\nx\nend\ntyped(\"draft\")",
        "def typed(x:Status)\nx\nend\ntyped(Review::Draft)",
        "def typed(x:any | Missing)\nx\nend\ntyped(7)",
        "def typed(x:array<Missing>?)\nx\nend\ntyped(nil)",
        "def typed(x:{state?:Missing})\nx\nend\ntyped({})",
    ] {
        let error = Engine::new()
            .compile(&format!("{ENUMS}{source}"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{source}");
    }
    assert_eq!(
        evaluate(
            "def typed(a:any | Status,b:symbol | Status,c:number,d:Status?)->array\n[a.name,b==:draft,c,d]\nend\ntyped(:draft,:draft,10**30,nil)"
        ),
        serde_json::from_str::<serde_json::Value>(
            "[\"Draft\",true,1000000000000000000000000000000,null]"
        )
        .unwrap()
    );
    assert_eq!(
        evaluate("def unused(x:Missing)\nx\nend\n7"),
        serde_json::json!(7)
    );
}

#[test]
fn defaults_keywords_captures_and_block_destructuring_normalize_values() {
    assert_eq!(
        evaluate(
            "def typed(first:Status=:draft,*rest:array<Status>,last:Status:,**extra:hash<symbol,Status>)->array<Status>\n[first]+rest+[last]+extra.values\nend\ntyped(:done,:draft,last: :done,other: :draft).map{|x|x.name}"
        ),
        serde_json::json!(["Done", "Draft", "Done", "Draft"])
    );
    assert_eq!(
        evaluate(
            "def typed(first:Status=:draft)\nfirst.name\nend\n[typed(),typed(first: :done),[[:draft,:done,:draft]].map{|(first:Status,*rest:array<Status>)|[first.name,rest.map{|x|x.name}]}]"
        ),
        serde_json::json!(["Draft", "Done", [["Draft", ["Done", "Draft"]]]])
    );
    assert_eq!(
        evaluate(
            "def typed(opts:{previous:nil},empty:{})\n[opts,empty]\nend\n[typed(),[[:draft,2]].map{|(state:Status,n:int)|[state.name,n]}]"
        ),
        serde_json::json!([[{"previous":null},{}],[["Draft",2]]])
    );
}

#[test]
fn shapes_preserve_optional_fields_extras_and_collection_values() {
    assert_eq!(
        evaluate(
            "def typed(packet:{state:Status,previous?:Status?,...})->{state:Status,...}\npacket\nend\na={before:[1],state: :draft,after:[2],previous:nil};b=typed(a);b.before.push(3);b.after.push(4);[a,b,b.state.name,b.keys]"
        ),
        serde_json::json!([
            {"before":[1],"state":"draft","after":[2],"previous":null},
            {"before":[1,3],"state":"draft","after":[2,4],"previous":null},
            "Draft",["before","state","after","previous"]
        ])
    );
    for (ty, value) in [
        ("{x:int}", "{}"),
        ("{x:int}", "{x:1,y:2}"),
        ("{x?:int}", "{x:nil}"),
        ("hash<int,any>", "{}"),
    ] {
        let script = Engine::new()
            .compile(&format!("def typed(x:{ty})\nx\nend\ntyped({value})"))
            .unwrap();
        assert_eq!(
            script.run(CallOptions::default()).unwrap_err().kind,
            ErrorKind::Type
        );
    }
    assert_eq!(
        evaluate("def typed(x:{\"valid?\":bool,optional?:int})\nx\nend\ntyped({\"valid?\":true})"),
        serde_json::json!({"valid?":true})
    );
}

#[test]
fn return_annotations_apply_to_all_function_exit_paths() {
    for source in [
        "def typed->Status\n:draft\nend\ntyped().name",
        "def typed->Status\nv=:draft;return v\nend\ntyped().name",
        "def typed->Status\n[1].map{v=:draft;return v}\nend\ntyped().name",
        "def typed->Status\nyield\nend\ntyped{v=:draft;break v}.name",
        "def typed(x:Status=([1].map{v=:draft;return v}))->Status\nx\nend\ntyped().name",
    ] {
        assert_eq!(evaluate(source), serde_json::json!("Draft"), "{source}");
        let error = Engine::new()
            .compile(&format!("{ENUMS}{}", source.replace(":draft", ":missing")))
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{source}");
    }
}

#[test]
fn named_types_use_definition_scopes_and_nominal_host_identity() {
    assert_eq!(
        evaluate("T=Status;[1].map{[[Review,:draft]].map{|T,x:T|x.enum.name}}"),
        serde_json::json!([["Status"]])
    );
    assert_eq!(
        evaluate("Alias=JSON;Alias.Status=Status;[:draft].map{|x:Alias.status|x.name}"),
        serde_json::json!(["Draft"])
    );
    assert_eq!(
        evaluate("T=Status;t=Status;[:draft].map{|x:t|x.name}"),
        serde_json::json!(["Draft"])
    );
    for source in [
        "def typed(T,x:T)\nx\nend\ntyped(Status,:draft)",
        "def typed->T\nT=Status;:draft\nend\ntyped()",
        "STatus=Status;StAtUs=Review;[:draft].map{|x:status|x}",
    ] {
        let error = Engine::new()
            .compile(&format!("{ENUMS}{source}"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{source}");
    }
    let source =
        format!("{ENUMS}def member\nStatus::Draft\nend\ndef typed(x:Status)->Status\nx\nend");
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
    for source in [
        "def typed(x:int,y=effect())\neffect()\nend\ntyped(\"bad\")",
        "def typed(x:int=\"bad\",y=effect())\neffect()\nend\ntyped()",
        "def typed->int\n\"bad\"\nend\ntyped();effect()",
        "[\"bad\"].map{|x:int|effect()}",
        "[[\"bad\",1]].map{|(x:int,y:int)|effect()}",
    ] {
        assert_eq!(
            engine
                .compile(source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Type,
            "{source}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn repeated_coercions_release_storage_and_preserve_host_inputs() {
    let script = Engine::new()
        .compile(&format!("{ENUMS}def typed(x:array<Status>)->array<Status>\nx\nend\ndef run(input)\ni=0;while i<500;output=typed(input);i+=1;end;output\nend"))
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
