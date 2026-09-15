use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use vibescript::{CallOptions, CancellationToken, Engine, ErrorKind, Value, stringify_json};

fn result(source: &str) -> serde_json::Value {
    let output = Engine::new()
        .compile(source)
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let json = stringify_json(&output.value, CallOptions::default()).unwrap();
    serde_json::from_slice(json.value.as_bytes().unwrap()).unwrap()
}

#[test]
fn universal_helpers_forward_across_value_and_callable_kinds() {
    assert_eq!(
        result(
            r#"
enum E
A
end
class C
end
[nil.send(:nil?),true.public_send(:nil?),1.send(:is_type?,:int),E::A.send(:is_type?,:E),C.new.send(:is_a?,C),JSON::parse.send(:nil?),JSON::parse.send(:dup).nil?,"a".match(/a/)[:begin].public_send(:itself).nil?]
"#
        ),
        serde_json::json!([true, false, true, true, true, false, false, false])
    );
    assert_eq!(
        result(
            "JSON[:nil?]=3;JSON[:itself]=4;JSON[:dup]=5;[JSON.nil?,JSON.itself.nil?,JSON.dup.nil?,JSON.send(:nil?)]"
        ),
        serde_json::json!([false, false, false, false])
    );
}

#[test]
fn visibility_and_overrides_follow_each_forwarding_hop() {
    assert_eq!(
        result(
            r#"
class C
private def hidden
3
end
protected def guarded
4
end
def check
public_send(:guarded)
end
end
c=C.new
[c.send(:hidden),c.send(:guarded),c.check,c.public_send(:send,:hidden),c.send(:respond_to?,:hidden),c.public_send(:respond_to?,:hidden)]
"#
        ),
        serde_json::json!([3, 4, 4, 3, true, false])
    );
    for call in [
        "public_send(:hidden)",
        "send(:public_send,:hidden)",
        "public_send(:guarded)",
    ] {
        let source = format!(
            "class C\nprivate def hidden\n3\nend\nprotected def guarded\n4\nend\nend\nC.new.{call}"
        );
        assert_eq!(
            Engine::new()
                .compile(&source)
                .unwrap()
                .run(CallOptions::default())
                .unwrap_err()
                .kind,
            ErrorKind::Name
        );
    }
    assert_eq!(
        result(
            "class C\ndef send(x)\nx+7\nend\nend\nJSON[:public_send]=JSON::parse;[C.new.send(2),JSON.public_send(\"[8]\"),{send:JSON::parse}.send(:size)]"
        ),
        serde_json::json!([9, [8], 1])
    );
}

#[test]
fn forwarded_arguments_keywords_options_and_blocks_keep_their_order() {
    assert_eq!(
        result(
            r#"
class C
property value
def initialize(options)
@value=options[:value]
end
def take(options)
options
end
def pack(*values,**keywords)
[values,keywords,yield(values[0])]
end
end
c=C.send(:new,value:7)
[c.value,c.public_send(:take,retries:3),c.send(*[:send,:public_send,:pack],*[1,2,3],z:4) {|x| x+10}]
"#
        ),
        serde_json::json!([7, {"retries":3}, [[1,2,3],{"z":4},11]])
    );
}

#[test]
fn forwarded_blocks_keep_nonlocal_control_and_ignored_block_rules() {
    assert_eq!(
        result(
            r#"
def early
[1,2].send(:public_send,:map) {|x| return x+3}
99
end
[early(),[1,2].send(:map) {|x| break x+3},[1,2].public_send(:map) {|x| next x+3},[1].send(:zip,[2]) {raise "entered"}]
"#
        ),
        serde_json::json!([4, 4, [4, 5], [[1, 2]]])
    );
}

#[test]
fn dynamic_mutators_rebind_only_the_original_addressed_path() {
    assert_eq!(
        result(
            r#"
a={x:[1],y:[2]};snapshot=a
a.x.send(:public_send,:push,a.y.send(:pop))
copy=a.x.send(:dup).push(7)
out=a.x.send(:map) {|x| x+10}
[a,snapshot,copy,out]
"#
        ),
        serde_json::json!([{"x":[1,2],"y":[]},{"x":[1],"y":[2]},[1,2,7],[11,12]])
    );
    assert_eq!(
        result(
            r#"
class C
@@items=[1]
def initialize
@items=[1]
end
def check
before=@items;@items.send(:push,2);[@items,before]
end
def self.check
before=@@items;@@items.public_send(:push,2);[@@items,before]
end
end
JSON[:items]=[1];before=JSON[:items];JSON[:items].send(:push,2)
[C.new.check,C.check,JSON[:items],before]
"#
        ),
        serde_json::json!([[[1, 2], [1]], [[1, 2], [1]], [1, 2], [1]])
    );
    assert_eq!(
        result(
            "a=[1];before=a;begin\na.send(:fill,0,9223372036854775807,1)\nrescue LimitError\nnil\nend;[a,before]"
        ),
        serde_json::json!([[1], [1]])
    );
}

#[test]
fn forwarded_reads_preserve_snapshots_while_arguments_mutate_or_rebind_the_receiver() {
    let cases = [
        (
            "a=[1,2];before=a;out=a.HELPER(:include?,a.pop);[a,before,out]",
            serde_json::json!([[1], [1, 2], true]),
        ),
        (
            "a=[1,2];out=a.HELPER(:fetch,a.clear.size);[a,out]",
            serde_json::json!([[], 1]),
        ),
        (
            "a=[1,2];out=a.HELPER(a.clear.empty? ? :size : :size);[a,out]",
            serde_json::json!([[], 2]),
        ),
        (
            "a=[1,2];out=a.HELPER(:send,:public_send,:include?,a.pop);[a,out]",
            serde_json::json!([[1], true]),
        ),
        (
            "a={x:[1,2]};out=a.x.HELPER(:include?,a.x.pop);[a,out]",
            serde_json::json!([{"x":[1]},true]),
        ),
        (
            "a={x:1};out=a.HELPER(:key?,a.delete(:x)==1 ? :x : :bad);[a,out]",
            serde_json::json!([{}, true]),
        ),
        (
            "a=[1,2];out=a.HELPER(a.clear.empty? ? :map : :map) {|n| n+3};[a,out]",
            serde_json::json!([[], [4, 5]]),
        ),
        (
            "a=[1,2];out=a.HELPER(a.clear.empty? ? :tap : :tap) {|n| n.push(3)};[a,out]",
            serde_json::json!([[], [1, 2]]),
        ),
        (
            "a=[1,2];out=a.HELPER(:send,a.clear.empty? ? :size : :size);[a,out]",
            serde_json::json!([[], 2]),
        ),
        (
            "a=[1,2];out=a.HELPER(:push,a.pop);[a,out]",
            serde_json::json!([[1, 2], [1, 2]]),
        ),
        (
            "a=[1,2];out=a.HELPER(:push,begin a=[4];a end);[a,out]",
            serde_json::json!([[4], [1, 2, [4]]]),
        ),
        (
            "a={x:[1,2]};out=a.x.HELPER(:push,begin a.x=[4];a.x end);[a,out]",
            serde_json::json!([{"x":[4]},[1,2,[4]]]),
        ),
        (
            "h={go:JSON::parse};out=h.HELPER(:go,h.clear.empty? ? \"[3]\" : \"[4]\");[h,out]",
            serde_json::json!([{}, [3]]),
        ),
        (
            "h={go:JSON::parse};h.HELPER(:go,h.replace({go:JSON::stringify}).size==1 ? \"[3]\" : \"[]\")",
            serde_json::json!([3]),
        ),
        (
            "JSON[:work]=JSON::parse;JSON.HELPER(:work,JSON.delete(:work)==nil ? \"[]\" : \"[3]\")",
            serde_json::json!([3]),
        ),
        (
            "JSON[:HELPER]=JSON::stringify;JSON.HELPER(begin JSON[:HELPER]=JSON::parse;[3] end)",
            serde_json::json!("[3]"),
        ),
    ];
    for helper in ["send", "public_send"] {
        for (source, expected) in &cases {
            let source = source.replace("HELPER", helper);
            assert_eq!(result(&source), *expected, "{source}");
        }
    }
}

#[test]
fn forwarded_read_snapshots_enforce_exact_budgets_and_release_their_storage() {
    let script = Engine::new()
        .compile("def run(items)\nitems.send(:include?,items.pop)\nend")
        .unwrap();
    let args = [Value::array((0..1024).map(Value::int).collect())];
    let output = script.call("run", &args, CallOptions::default()).unwrap();
    assert_eq!(output.value.to_string(), "true");
    assert_eq!(output.stats.retained_memory_bytes, 0);
    assert_eq!(args[0].as_array().unwrap().len(), 1024);
    let mut options = CallOptions::default();
    options.limits.steps = Some(output.stats.steps);
    options.limits.memory_bytes = Some(output.stats.peak_memory_bytes);
    script.call("run", &args, options.clone()).unwrap();
    options.limits.steps = Some(output.stats.steps - 1);
    assert_eq!(
        script.call("run", &args, options.clone()).unwrap_err().kind,
        ErrorKind::Steps
    );
    options.limits.steps = Some(output.stats.steps);
    options.limits.memory_bytes = Some(output.stats.peak_memory_bytes - 1);
    assert_eq!(
        script.call("run", &args, options).unwrap_err().kind,
        ErrorKind::Memory
    );
}

#[test]
fn temporary_receiver_members_keep_callable_lookup_rules() {
    assert_eq!(
        result(
            "h={go:JSON::parse};[h.go.send(:nil?),JSON.parse.send(:nil?),JSON::parse.send(:nil?),[h,:nil?].reduce(:send)]"
        ),
        serde_json::json!([false, false, false, false])
    );
    for receiver in [
        "{go:JSON::parse}.go",
        "[ {go:JSON::parse} ][0].go",
        "\"a\".match(/a/).begin",
    ] {
        let error = Engine::new()
            .compile(&format!("({receiver}).send(:nil?)"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{receiver}: {error}");
    }
}

#[test]
fn data_properties_are_read_before_the_non_callable_error() {
    for expression in [
        "1.send(:seconds)",
        "1.seconds.send(:parts)",
        "Time.at(0).send(:year)",
        "money(\"1.00 USD\").send(:currency)",
        "{data:3}.send(:data)",
    ] {
        let error = Engine::new()
            .compile(expression)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Type, "{expression}: {error}");
        assert_eq!(error.message, "attempted to call non-callable value");
    }
    assert_eq!(
        result("[/ab/.send(:source),[1,2].send(:size)]"),
        serde_json::json!(["ab", 2])
    );
}

#[test]
fn protected_receivers_allow_forwarded_reads_and_reject_writes() {
    assert_eq!(
        result(
            "m=\"ab\".match(/(a)(b)/);[m.captures.send(:size),m.send(:dup).captures.send(:size)]"
        ),
        serde_json::json!([2, 2])
    );
    for expression in [
        "m.send(:clear)",
        "m.captures.send(:push,\"x\")",
        "m.dup.send(:public_send,:clear)",
        "m.send(:clone).captures.send(:send,:push,\"x\")",
    ] {
        let error = Engine::new()
            .compile(&format!("m=\"ab\".match(/(a)(b)/);{expression}"))
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(error.message, "cannot modify match data", "{expression}");
    }
    let error = Engine::new()
        .compile("begin\nraise \"x\"\nrescue RuntimeError=>e\ne.public_send(:clear)\nend")
        .unwrap()
        .run(CallOptions::default())
        .unwrap_err();
    assert_eq!(error.message, "cannot modify rescued error");
}

#[test]
fn runtime_type_boundaries_still_reject_forwarded_calls_and_setters() {
    assert_eq!(
        result(
            r#"
class C
property items: array<int>
def initialize
@items=[1]
end
def take(x:int) -> int
x
end
end
c=C.new
begin
c.send("items=",["bad"])
rescue RuntimeError
nil
end
[c.send(:items),c.send(:take,7)]
"#
        ),
        serde_json::json!([[1], 7])
    );
    assert!(
        Engine::new()
            .compile("class C\ndef take(x:int)\nx\nend\nend\nC.new.send(:take,\"7\")")
            .unwrap()
            .run(CallOptions::default())
            .is_err()
    );
}

#[test]
fn forwarded_raw_method_names_preserve_bytes_and_chunk_boundaries() {
    let script=Engine::new().compile("def run(key)\nh={};h[key]=JSON::parse;JSON[key]=JSON::parse;[h.send(key,\"[3]\"),JSON.public_send(:send,key,\"[4]\")]\nend").unwrap();
    for key in [
        vec![0xff],
        vec![0xc3],
        Vec::new(),
        [vec![b'x'; 4095], "é".as_bytes().to_vec(), vec![b'y'; 4096]].concat(),
    ] {
        let output = script
            .call("run", &[Value::bytes(key)], CallOptions::default())
            .unwrap();
        assert_eq!(output.value.to_string(), "[[3], [4]]");
        assert!(output.stats.retained_memory_bytes < 1024);
    }
    let script = Engine::new()
        .compile("def run(key)\n{}.send(key)\nend")
        .unwrap();
    let error = script
        .call("run", &[Value::bytes(vec![0xff])], CallOptions::default())
        .unwrap_err();
    assert_eq!(error.message_bytes(), b"unknown hash method \xff");
}

#[test]
fn wide_forwarding_chains_use_linear_accounted_work_and_bounded_frames() {
    let script=Engine::new().compile("class C\ndef pack(*items,**keywords)\n[items,keywords]\nend\nend\ndef run(names)\nC.new.send(*names,*[1,2,3,4,5],flag:7)\nend").unwrap();
    let mut stats = Vec::new();
    for count in [1, 2, 17, 1024, 4096] {
        let mut names = vec![Value::symbol("send"); count];
        names.push(Value::symbol("pack"));
        let mut options = CallOptions::default();
        options.limits.recursion = 8;
        let output = script.call("run", &[Value::array(names)], options).unwrap();
        assert_eq!(output.value.to_string(), "[[1, 2, 3, 4, 5], {flag: 7}]");
        assert!(output.stats.retained_memory_bytes < 2048);
        stats.push(output.stats);
    }
    assert!(stats[4].steps < stats[3].steps * 5);
    assert!(stats[4].peak_memory_bytes < stats[3].peak_memory_bytes * 5);
}

#[test]
fn forwarded_name_errors_enforce_exact_budgets_and_release_the_input() {
    let script=Engine::new().compile("def run(name)\nbegin\n{}.send(name)\nrescue RuntimeError=>e\ne.message.bytesize\nend\nend").unwrap();
    let args = [Value::bytes(vec![b'x'; 64 << 10])];
    let output = script.call("run", &args, CallOptions::default()).unwrap();
    assert_eq!(output.value.as_int(), Some((64 << 10) + 20));
    assert_eq!(output.stats.retained_memory_bytes, 0);
    let mut options = CallOptions::default();
    options.limits.steps = Some(output.stats.steps);
    options.limits.memory_bytes = Some(output.stats.peak_memory_bytes);
    script.call("run", &args, options.clone()).unwrap();
    options.limits.steps = Some(output.stats.steps - 1);
    assert_eq!(
        script.call("run", &args, options.clone()).unwrap_err().kind,
        ErrorKind::Steps
    );
    options.limits.steps = Some(output.stats.steps);
    options.limits.memory_bytes = Some(output.stats.peak_memory_bytes - 1);
    assert_eq!(
        script.call("run", &args, options).unwrap_err().kind,
        ErrorKind::Memory
    );
}

#[test]
fn rejected_forwarding_evaluates_arguments_and_never_invokes_the_block() {
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut engine = Engine::new();
    engine.register("entered", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    for expression in [
        "1.send(1,entered()) {entered()}",
        "1.send(:missing,entered()) {entered()}",
    ] {
        calls.store(0, Ordering::SeqCst);
        engine
            .compile(expression)
            .unwrap()
            .run(CallOptions::default())
            .unwrap_err();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
    calls.store(0, Ordering::SeqCst);
    let value = engine
        .compile("nil&.send(entered(),entered()) {entered()}")
        .unwrap()
        .run(CallOptions::default())
        .unwrap();
    let encoded = stringify_json(&value.value, CallOptions::default()).unwrap();
    assert_eq!(encoded.value.as_bytes(), Some(b"null".as_slice()));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn cancellation_inside_a_forwarded_method_prevents_cleanup_and_later_effects() {
    let token = CancellationToken::new();
    let cancel = token.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut engine = Engine::new();
    engine.register("stop", move |_, _| {
        cancel.cancel();
        Ok(Value::nil())
    });
    engine.register("after", move |_, _| {
        observed.fetch_add(1, Ordering::SeqCst);
        Ok(Value::nil())
    });
    let script=engine.compile("class C\ndef work\nstop();after()\nend\nend\nbegin\nC.new.send(:public_send,:work);after()\nrescue RuntimeError | LimitError\nafter()\nensure\nafter()\nend").unwrap();
    let error = script
        .run(CallOptions {
            cancellation: token,
            ..CallOptions::default()
        })
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Cancelled);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn repeated_failed_sends_release_abandoned_arguments_and_addresses() {
    let script=Engine::new().compile("def attempt\nbegin\n[1].send(:missing,\"x\"*8192)\nrescue RuntimeError\n42\nend\nend\ndef run(n)\ni=0;while i<n\nattempt();i+=1\nend;42\nend").unwrap();
    let mut options = CallOptions::default();
    options.limits.steps = None;
    let once = script
        .call("run", &[Value::int(1)], options.clone())
        .unwrap();
    let many = script.call("run", &[Value::int(64)], options).unwrap();
    assert_eq!(many.value.as_int(), Some(42));
    assert_eq!(once.stats.retained_memory_bytes, 0);
    assert_eq!(many.stats.retained_memory_bytes, 0);
    assert_eq!(once.stats.peak_memory_bytes, many.stats.peak_memory_bytes);
}
