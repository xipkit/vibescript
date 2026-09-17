"""Declarative host boundary contracts, normalization and source resolution."""


def cases():
    result = []

    def add(name, body, expected, *, params=None, returns="", callback="echo", block=False,
            registration="capability", prefix="", contract=False, **options):
        if params is None:
            params = [{"name": "value", "type": "int"}]
        for strict in ([False] if registration == "global" else [False, True]):
            for accounting in [False, True]:
                result.append({
                    "name": f"host_signatures/{name}/{registration}/{'strict' if strict else 'ordinary'}/{'metered' if accounting else 'unlimited'}",
                    "source": prefix + "\ndef run(input)\nbegin\n" + body + '\nrescue => e; ["unexpected", e.class.to_s, e.message]; end\nend',
                    "args": [None], "expected": expected, "accounting": accounting,
                    "strict_effects": strict,
                    "signature_probe": {"params": params, "result": returns, "callback": callback,
                        "accepts_block": block, "registration": registration, "contract": contract},
                    **options,
                })

    for registration, call in [("capability", "typed.echo"), ("registered", "echo"), ("global", "echo")]:
        add("basic", f"{call}(7)", 7, returns="int", registration=registration)
        add("expanded", f"{call}(*[7])", 7, returns="int", registration=registration)
        add("block", f"{call}(7) {{ |n| n+1 }}", 8, returns="int", registration=registration, callback="block", block=True)
        add("module_default", "require(:worker).run()", 7, registration=registration, allow_require=True,
            params=[{"name":"value", "type":"Widget"}], returns="Widget",

            files={"worker.vibe":f"class Widget; def id; 7; end; end; def run(x={call}(Widget.new)); {call}(x).id; end"})
        add("module_enum", "require(:worker).run()", True, registration=registration, allow_require=True,
            params=[{"name":"value", "type":"Status"}], returns="Status",

            files={"worker.vibe":f"enum Status; Draft; end; def run(x={call}(:draft)); x==Status::Draft; end"})

    for name, call in [("scoped", "typed::echo(7)"), ("indexed", "typed[:echo](7)"),
        ("computed", "(typed[:echo])(7)"), ("symbolic", "typed.send(:echo,7)"),
        ("public", "typed.public_send(:echo,7)"), ("copy", "typed.dup.echo(7)"),
        ("safe", "typed&.echo(7)"), ("nested_receiver", "[typed][0].echo(7)")]:
        add(name, call, 7, returns="int")

    for name, ty, value, expected in [
        ("int", "int", "7", 7), ("float", "float", "1.5", 1.5),
        ("number", "number", "7", 7), ("bool", "bool", "true", True),
        ("string", "string", '"text"', "text"), ("nil", "nil", "nil", None),
        ("nullable", "int?", "nil", None), ("union", "int | string", '"yes"', "yes"),
        ("array", "array<int>", "[1,2]", [1,2]), ("hash", "hash<string,int>", "{a: 1}", {"a":1}),
        ("shape", "{ a: int, b?: string }", "{a: 1}", {"a":1}),
        ("open_shape", "{ a: int, ... }", "{a: 1,b: 2}", {"a":1,"b":2}),
        ("unknown", "", "[1,nil]", [1,None]), ("any", "any", "{a: [1,nil]}", {"a":[1,None]}),
    ]:
        add("type_"+name, f"typed.echo({value})", expected, params=[{"name":"value","type":ty}], returns=ty)

    for name, body, message, params in [
        ("too_few", "typed.echo()", "typed.echo expects at least 1 arguments, got 0", [{"name":"value","type":"int"}]),
        ("too_many", "typed.echo(1,2)", "typed.echo expects at most 1 arguments, got 2", [{"name":"value","type":"int"}]),
        ("keywords", "typed.echo(1, key:2)", "typed.echo does not take keyword arguments", [{"name":"value","type":"int"}]),
        ("no_block", "typed.echo(1) { 2 }", "typed.echo does not take a block", [{"name":"value","type":"int"}]),
        ("wrong_type", 'typed.echo("bad")', "typed.echo argument value expected int, got string", [{"name":"value","type":"int"}]),
        ("unnamed", "typed.echo(nil)", "typed.echo argument 1 expected int, got nil", [{"type":"int"}]),
    ]:
        add(name, f"begin; {body}; rescue => e; [e.class.to_s, e.message]; end", ["RuntimeError", message], params=params)
    add("bad_return", "begin; typed.echo(1); rescue => e; [e.class.to_s,e.message]; end",
        ["RuntimeError", "return value for typed.echo expected int, got string"], returns="int", callback="bad")
    add("optional_absent", "typed.echo(1)", [1], callback="list", returns="array",
        params=[{"name":"value","type":"int"},{"name":"flag","type":"bool","optional":True}])
    add("optional_present", "typed.echo(1,true)", [1,True], callback="list", returns="array",
        params=[{"name":"value","type":"int"},{"name":"flag","type":"bool","optional":True}])
    add("optional_unknown", "typed.echo()", [], callback="list", returns="array",
        params=[{"name":"value","optional":True}])
    add("root_fallback", "require(:worker).run()", True, allow_require=True,
        params=[{"name":"value", "type":"Status"}], returns="Status",
        prefix="enum Status; Root; end", files={"worker.vibe":"def run; typed.echo(:root)==Status::Root; end"})
    for name, ty, expr in [("unknown", "Missing", "7"), ("empty_unknown", "array<Missing>", "[]"), ("union_unknown", "int | Missing", "7")]:
        add(name, f"begin; typed.echo({expr}); rescue => e; e.message; end",
            "typed.echo argument value type check failed: unknown type Missing", params=[{"name":"value","type":ty}])
    add("unknown_result", "begin; typed.echo(7); rescue => e; e.message; end",
        "return type check failed for typed.echo: unknown type Missing", returns="Missing")
    add("qualified_type", "require(:worker).run()", True, allow_require=True,
        params=[{"name":"value", "type":"types.Status"}], returns="types.Status",
        files={"types.vibe":"enum Status; Draft; end", "worker.vibe":"types=require(:types); def run; typed.echo(:draft)==types.Status::Draft; end"},
        go=["unexpected", "RuntimeError", "typed.echo argument value type check failed: unknown type types.Status"],
        policy="consistent_signature_type_scope",
        reason="Resolve qualified type aliases in the active file environment, consistently with other source bindings, instead of consulting only the call root.")
    enum = "enum Status; Draft; Sent; end"
    for callback, contract in [("echo",False),("echo",True),("symbol",False)]:
        add(f"enum_{callback}_{contract}", "typed.echo(:draft)==Status::Draft", True,
            prefix=enum, params=[{"name":"status","type":"Status"}], returns="Status", callback=callback, contract=contract)
    add("enum_kind", "typed.echo(:draft)", "enum value", prefix=enum,
        params=[{"name":"status","type":"Status"}], returns="string", callback="kind")
    add("enum_nested", "a=[{state: :draft}]; b=typed.echo(a); [a[0].state.is_type?(:symbol), b[0].state==Status::Draft]", [True,True], prefix=enum,
        params=[{"name":"value","type":"array<{ state: Status }>"}], returns="array<{ state: Status }>")
    for name, body, expected in [
        ("next", "typed.echo(1) { next 7 }", 7),
        ("break", "typed.echo(1) { break 7 }", 7),
        ("return", 'typed.echo(1) { return "ok" }; 99', "ok"),
        ("nested", "typed.echo(1) { |n| typed.echo(n+1) { |m| m+1 } }", 3),
        ("ensure", "a=[]; r=typed.echo(1) { begin; break 7; ensure; a.push(1); end }; [r,a]", [7,[1]]),
    ]:
        add(name, body, expected, block=True, callback="block", returns="int")
    for registration, call in [("capability", "typed.echo"), ("registered", "echo"), ("global", "echo")]:
        for name, ty, body, prefix, expected, got in [
            ("class_shadow", "Widget", f"class Widget; def id; 7; end; end; def run(x={call}(Widget.new)); {call}(x).id; end", "class Widget; def id; 99; end; end", 7, "instance"),
            ("enum_shadow", "Status", f"enum Status; Draft; end; def run(x={call}(:draft)); x==Status::Draft; end", "enum Status; Root; end", True, "symbol"),
        ]:
            add(name, "require(:worker).run()", expected, registration=registration,
                params=[{"name":"value", "type":ty}], returns=ty,
                prefix=prefix, files={"worker.vibe":body}, allow_require=True,
                go=["unexpected", "RuntimeError", f"{call} argument value expected {ty}, got {got}"],
                policy="consistent_signature_type_scope",
                reason="Apply the selected consistent binding rule to named host types: declarations in the active required source precede same-named root declarations, including defaults.")
    add("bad_break", 'begin; typed.echo(1) { break "bad" }; rescue => e; e.message; end',
        "return value for typed.echo expected int, got string", block=True, callback="block", returns="int", go="bad",
        policy="signature_break_result",
        reason="Honor the documented invariant host result contract when a block break becomes the call result, as with existing capability return contracts.")
    assert len({case["name"] for case in result}) == len(result)
    return result
