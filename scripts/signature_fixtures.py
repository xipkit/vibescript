"""Declarative host boundary contracts, normalization and source resolution."""


def cases():
    result = []

    def add(name, body, expected, *, params=None, returns="", callback="echo", block=False,
            registration="capability", prefix="", contract=False, value="any", run_type=None, static_error=None, **options):
        static_error = {
            'public': {'code': 'V0203', 'at': [4, 7]},
            'scoped': {'code': 'V0416', 'at': [4, 6]},
            'symbolic': {'code': 'V0203', 'at': [4, 7]},
        }.get(name, static_error)
        if params is None:
            params = [{"name": "value", "type": "int"}]
        # `value` is the type of `body`; the rescue clause adds the error report's array<string>.
        result_type = run_type or ("any" if value == "any" else f"array<string> | {value}")
        for strict in ([False] if registration == "global" else [False, True]):
            for accounting in [False, True]:
                result.append({
                    "name": f"host_signatures/{name}/{registration}/{'strict' if strict else 'ordinary'}/{'metered' if accounting else 'unlimited'}",
                    "source": prefix + f"\ndef run(input: any) -> {result_type}\nbegin\n" + body + '\nrescue => e; ["unexpected", e.class.to_s, e.message]; end\nend',
                    "args": [None], "expected": expected, "accounting": accounting,
                    "strict_effects": strict,
                    "signature_probe": {"params": params, "result": returns, "callback": callback,
                        "accepts_block": block, "registration": registration, "contract": contract},
                    **options,
                })
                if static_error:
                    result[-1]["static_error"] = static_error

    for registration, call in [("capability", "typed.echo"), ("registered", "echo"), ("global", "echo")]:
        add("basic", f"{call}(7)", 7, returns="int", registration=registration, value="int")
        add("expanded", f"{call}(*[7])", 7, returns="int", registration=registration, value="int")
        add("block", f"{call}(7) {{ |n| n.as(int)+1 }}", 8, returns="int", registration=registration, callback="block", block=True,
            value="int")
        add("module_default", 'require("worker").run', 7, registration=registration, allow_require=True,
            params=[{"name":"value", "type":"Widget"}], returns="Widget", value="int",

            files={"worker.vibe":f"class Widget; def id -> int; 7; end; end; def run(x: Widget = {call}(Widget.new).as(Widget)) -> int; {call}(x).as(Widget).id; end"})
        add("module_enum", 'require("worker").run', True, registration=registration, allow_require=True,
            params=[{"name":"value", "type":"Status"}], returns="Status", value="bool",

            files={"worker.vibe":f"enum Status; Draft; end; def run(x: Status = {call}(:draft).as(Status)) -> bool; x==Status::Draft; end"})

    # Only declared capability methods are callable with static types.
    for name, call, value, static_error in [
        ("scoped", "typed::echo(7)", "int", None),
        ("indexed", 'typed["echo"](7)', "any", {"code": "V0112", "at": [4, 1]}),
        ("computed", '(typed["echo"])(7)', "any", {"code": "V0112", "at": [4, 2]}),
        ("symbolic", "typed.send(:echo,7)", "any", None),
        ("public", "typed.public_send(:echo,7)", "any", None),
        ("copy", "typed.dup.echo(7)", "int", None),
        ("safe", "typed&.echo(7)", "int?", None), ("nested_receiver", "[typed].fetch(0).echo(7)", "int", None)]:
        add(name, call, 7, returns="int", value=value, static_error=static_error)

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
        add("type_"+name, f"typed.echo({value})", expected, params=[{"name":"value","type":ty}], returns=ty,
            value=ty if ty and ty != "any" else "any")

    # Calls that contradict the signature are type errors now.
    for name, body, message, params, static_error in [
        ("too_few", "typed.echo()", "typed.echo expects at least 1 arguments, got 0", [{"name":"value","type":"int"}],
         {"code": "V0301", "at": [4, 14]}),
        ("too_many", "typed.echo(1,2)", "typed.echo expects at most 1 arguments, got 2", [{"name":"value","type":"int"}],
         {"code": "V0301", "at": [4, 14]}),
        ("keywords", "typed.echo(1, key:2)", "typed.echo does not take keyword arguments", [{"name":"value","type":"int"}],
         {"code": "V0302", "at": [4, 22]}),
        ("no_block", "typed.echo(1) { 2 }", "typed.echo does not take a block", [{"name":"value","type":"int"}],
         {"code": "V0305", "at": [4, 22]}),
        ("wrong_type", 'typed.echo("bad")', "typed.echo argument value expected int, got string", [{"name":"value","type":"int"}],
         {"code": "V0101", "at": [4, 19]}),
        ("unnamed", "typed.echo(nil)", "typed.echo argument 1 expected int, got nil", [{"type":"int"}],
         {"code": "V0101", "at": [4, 19]}),
    ]:
        add(name, f"begin; {body}; rescue => e; [e.class.to_s, e.message]; end", ["RuntimeError", message], params=params,
            static_error=static_error)
    add("bad_return", "begin; typed.echo(1); rescue => e; [e.class.to_s,e.message]; end",
        ["RuntimeError", "return value for typed.echo expected int, got string"], returns="int", callback="bad",
        value="int")
    add("optional_absent", "typed.echo(1)", [1], callback="list", returns="array",
        params=[{"name":"value","type":"int"},{"name":"flag","type":"bool","optional":True}], value="array<any>")
    add("optional_present", "typed.echo(1,true)", [1,True], callback="list", returns="array",
        params=[{"name":"value","type":"int"},{"name":"flag","type":"bool","optional":True}], value="array<any>")
    add("optional_unknown", "typed.echo()", [], callback="list", returns="array",
        params=[{"name":"value","optional":True}], value="array<any>")
    add("root_fallback", 'require("worker").run', True, allow_require=True,
        params=[{"name":"value", "type":"Status"}], returns="Status", value="bool",
        prefix="enum Status; Root; end", files={"worker.vibe":"def run -> bool; typed.echo(:root)==Status::Root; end"},
        # A required file does not see the receiving script's declarations under static types.
        static_error={"code": "V0201", "at": None})
    for name, ty, expr in [("unknown", "Missing", "7"), ("empty_unknown", "array<Missing>", "[]"), ("union_unknown", "int | Missing", "7")]:
        add(name, f"begin; typed.echo({expr}); rescue => e; e.message; end",
            "typed.echo argument value type check failed: unknown type Missing", params=[{"name":"value","type":ty}])
    add("unknown_result", "begin; typed.echo(7); rescue => e; e.message; end",
        "return type check failed for typed.echo: unknown type Missing", returns="Missing")
    add("qualified_type", 'require("worker").run', True, allow_require=True,
        params=[{"name":"value", "type":"types.Status"}], returns="types.Status", value="bool",
        files={"types.vibe":"enum Status; Draft; end", "worker.vibe":"types=require(\"types\"); def run -> bool; typed.echo(:draft)==types.Status::Draft; end"},
        static_error=None,
        go=["unexpected", "RuntimeError", "typed.echo argument value type check failed: unknown type types.Status"],
        policy="consistent_signature_type_scope",
        reason="Resolve qualified type aliases in the active file environment, consistently with other source bindings, instead of consulting only the call root.")
    enum = "enum Status; Draft; Sent; end"
    for callback, contract in [("echo",False),("echo",True),("symbol",False)]:
        add(f"enum_{callback}_{contract}", "typed.echo(:draft)==Status::Draft", True,
            prefix=enum, params=[{"name":"status","type":"Status"}], returns="Status", callback=callback, contract=contract,
            value="bool")
    add("enum_kind", "typed.echo(:draft)", "enum value", prefix=enum,
        params=[{"name":"status","type":"Status"}], returns="string", callback="kind", value="string")
    add("enum_nested", 'a=[{state: :draft}]; b=typed.echo(a); [a.fetch(0)["state"].is_type?(:symbol), b.fetch(0)["state"]==Status::Draft]',
        [True,True], prefix=enum,
        params=[{"name":"value","type":"array<{ state: Status }>"}], returns="array<{ state: Status }>", run_type="array<bool | string>")
    for name, body, expected, run_type in [
        ("next", "typed.echo(1) { next 7 }", 7, "array<string> | int"),
        ("break", "typed.echo(1) { break 7 }", 7, "array<string> | int"),
        ("return", 'typed.echo(1) { return "ok" }; 99', "ok", "array<string> | int | string"),
        ("nested", "typed.echo(1) { |n| typed.echo(n.as(int)+1) { |m| m.as(int)+1 } }", 3, "array<string> | int"),
        ("ensure", "a: array<int> = []; r=typed.echo(1) { begin; break 7; ensure; a.push(1); end }; [r,a]", [7,[1]], "array<int | array<int> | string>"),
    ]:
        add(name, body, expected, block=True, callback="block", returns="int", run_type=run_type)
    for registration, call in [("capability", "typed.echo"), ("registered", "echo"), ("global", "echo")]:
        for name, ty, body, prefix, expected, got, value in [
            ("class_shadow", "Widget", f"class Widget; def id -> int; 7; end; end; def run(x: Widget = {call}(Widget.new).as(Widget)) -> int; {call}(x).as(Widget).id; end",
             "class Widget; def id -> int; 99; end; end", 7, "instance", "int"),
            ("enum_shadow", "Status", f"enum Status; Draft; end; def run(x: Status = {call}(:draft).as(Status)) -> bool; x==Status::Draft; end",
             "enum Status; Root; end", True, "symbol", "bool"),
        ]:
            add(name, 'require("worker").run', expected, registration=registration,
                params=[{"name":"value", "type":ty}], returns=ty, value=value,
                prefix=prefix, files={"worker.vibe":body}, allow_require=True,
                go=["unexpected", "RuntimeError", f"{call} argument value expected {ty}, got {got}"],
                policy="consistent_signature_type_scope",
                reason="Apply the selected consistent binding rule to named host types: declarations in the active required source precede same-named root declarations, including defaults.")
    # The checker now rejects the break value the result contract refuses.
    add("bad_break", 'begin; typed.echo(1) { break "bad" }; rescue => e; e.message; end',
        "return value for typed.echo expected int, got string", block=True, callback="block", returns="int", go="bad",
        static_error={"code": "V0101", "at": [4, 30]},
        policy="signature_break_result",
        reason="Honor the documented invariant host result contract when a block break becomes the call result, as with existing capability return contracts.")
    assert len({case["name"] for case in result}) == len(result)
    return result
