"""Host-driven block conformance and explicit control-flow differences."""


def cases():
    result = []

    def add(name, body, expected, prefix="", returns="any", static_error=None, **options):
        for strict in [False, True]:
            for accounting in [False, True]:
                result.append({
                    "name": f"host_blocks/{name}/{'strict' if strict else 'ordinary'}/{'metered' if accounting else 'unlimited'}",
                    "source": prefix + "\ndef run(input: any)" + (f" -> {returns}" if returns else "") + "\n" + body + "\nend",
                    "args": [None], "expected": expected, "accounting": accounting,
                    "strict_effects": strict, "block_probe": True, **options,
                })
                if static_error:
                    result[-1]["static_error"] = static_error

    # The probe's methods have no signatures: their blocks take `any` arguments, which the
    # script narrows with `as`, and their results are `any`.
    for name, call, static_error in [
        ("direct", "blocks.once(3)", None), ("scoped", "blocks::once(3)", None),
        ("indexed", 'blocks["once"](3)', {"code": "V0112", "at": [3, 1]}),
        ("computed", '(blocks["once"])(3)', {"code": "V0112", "at": [3, 2]}),
        ("symbolic", "blocks.send(:once,3)", None),
        ("public", "blocks.public_send(:once,3)", None),
        ("copy", "blocks.dup.once(3)", None), ("safe", "blocks&.once(3)", None),
        ("nested", "[blocks].fetch(0).once(3)", None),
    ]:
        add(name, call + " { |n| n.as(int)+1 }", 4, static_error=static_error)
    for name, body, expected, returns, static_error in [
        ("missing_parameter", "blocks.once(1,2) { |a,b,c| [a,b,c] }", [1,2,None], "any", None),
        ("extra_parameter", "blocks.once(1,2,3) { |a,b| [a,b] }", [1,2], "any", None),
        ("autosplat", "blocks.once([2,3]) { |a,b| a.as(int)+b.as(int) }", 5, "any", None),
        ("destructure", "blocks.once([[2,3],4]) { |(a,b),c| a.as(int)+b.as(int)+c.as(int) }", 9, "any", None),
        ("implicit", "blocks.once(3) { _1.as(int)+1 }", 4, "any", None),
        # A block parameter's annotation must match its `any` argument statically.
        ("typed", "blocks.once(3) { |n: int| n+1 }", 4, "any", {"code": "V0106", "at": [3, 19]}),
        ("invalid_type", 'begin; blocks.once("bad") { |n: int| n }; rescue => e; e.class.to_s; end', "RuntimeError", "any",
         {"code": "V0106", "at": [3, 30]}),
        ("next", "blocks.once(3) { next 4 }", 4, "any", None),
        ("break", "blocks.once(3) { break 4 }", 4, "any", None),
        ("return", "blocks.once(3) { return 4 }; 99", 4, "int", None),
        ("lexical_block", "blocks.once { block_given? }", False, "any", None),
        ("no_block", "blocks.optional()", False, "any", None),
        ("optional", 'blocks.optional { raise "unused" }', True, "any", None),
        ("block_required", "begin; blocks.once(); rescue => e; [e.class.to_s,e.message]; end", ["RuntimeError","block required"], "any", None),
        ("each", "blocks.each([1,2,3]) { |n| n.as(int)*2 }", [2,4,6], "any", None),
        ("each_break", "blocks.each([1,2,3]) { |n| break 7 if n==2; n }", 7, "any", None),
        ("each_next", "blocks.each([1,2,3]) { |n| next 7 if n==2; n }", [1,7,3], "any", None),
        ("nested_each", "blocks.each([1,2]) { |i| blocks.each([3,4]) { |j| i.as(int)+j.as(int) } }", [[4,5],[5,6]], "any", None),
        ("captured_write", "a: array<int> = []; blocks.each([1,2,3]) { |n| a.push(n.as(int)) }; a", [1,2,3], "array<int>", None),
        ("outer_loop", "a: array<any> = []; for i in [1,2]; a.push(blocks.once(i) { break 7 }); end; a", [7,7], "array<any>", None),
        ("local_rescue", 'blocks.once { begin; raise "bad"; rescue; 7; end }', 7, "any", None),
        ("host_rescue", 'begin; blocks.recover { raise "bad" }; rescue; "script recovered"; end', "recovered", "any", None),
        ("retry", 'blocks.once { n=0; begin; n+=1; raise "again" if n<3; n; rescue; retry; end }', 3, "any", None),
        ("ensure", 'a: array<int> = []; begin; blocks.once { begin; raise "bad"; ensure; a.push(1); end }; rescue; a.push(2); ensure; a.push(3); end; a', [1,2,3],
         "array<int>", None),
        ("ensure_override", "blocks.once { begin; return 7; ensure; return 8; end }; 99", 8, "int", None),
        ("break_contract", "blocks.checked(1) { break 7 }", 7, "any", None),
        ("bad_break_contract", 'begin; blocks.checked(1) { break "bad" }; rescue => e; e.message; end', "integer result required", "any", None),
        ("nonlocal_contract", 'blocks.checked(1) { return "ok" }; 99', "ok", "int | string", None),
        ("presence_contract", 'begin; blocks.checked(1); rescue => e; e.message; end', "one argument and block required", "any", None),
        ("keywords", 'blocks.keywords(1,tag: 2) { |args,kw| [args,kw.as(hash<string, any>)["tag"]] }', [[1],2], "any", None),
    ]:
        add(name, body, expected, returns=returns, static_error=static_error)
    for name, driver, expected in [
        ("relay_host", "blocks.once { yield }; 99", 99),
        ("relay_loop", "for i in [1,2]; yield; end; 99", 99),
        ("relay_each", "[1].each { yield }; 99", 99),
        ("relay_direct", "yield; 99", 7),
    ]:
        # A host block's value is the value of its `yield`, so that relay declares the block's result.
        block = "() -> int" if name == "relay_host" else "()"
        add(name, "relay { break 7 }", expected, prefix=f"def relay(&block: {block}) -> int; {driver}; end", returns="int")
    add("relay_return", "relay { return 7 }; 99", 7, prefix="def relay(&block: () -> int) -> int; blocks.once { yield }; 99; end", returns="int")
    add("file", 'require("worker").work(3)', 4, allow_require=True, returns="int",
        files={"worker.vibe": "def work(n: int) -> int; blocks.once(n) { |x| return x.as(int)+1 }; 99; end"})
    for name, body, returns in [
        ("ignored_break", "a: array<int> = []; r=blocks.ignore { a.push(1); break 7 }; [r,a]", "[any, array<int>]"),
        ("ignored_return", "a: array<int> = []; blocks.ignore { a.push(1); return [7,a] }; [99,a]", "[int, array<int>]"),
    ]:
        add(name, body, [7,[1]], returns=returns, go=[99,[1,1]], policy="preserve_host_block_control",
            reason="Selected control-flow policy: a host cannot erase a pending block break or return by ignoring its error; further block invocations do not rerun script code.")
    for name, member, static_error in [("indexed", 'blocks["once"]', {"code": "V0112", "at": [3, 15]}),
                                       ("scoped", "blocks::once", {"code": "V0310", "at": [3, 29]})]:
        add("detached_"+name, f'begin; method={member}; method(3) {{ |n| n+1 }}; rescue; "attached-method-required"; end',
            "attached-method-required", static_error=static_error, go=4, policy="attached_capability_methods",
            reason="ADR-006 keeps block-capable host methods attached too; immediate calls remain available.")
    return result
