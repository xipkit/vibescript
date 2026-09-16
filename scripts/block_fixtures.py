"""Host-driven block conformance and explicit control-flow differences."""


def cases():
    result = []

    def add(name, body, expected, prefix="", **options):
        for strict in [False, True]:
            for accounting in [False, True]:
                result.append({
                    "name": f"host_blocks/{name}/{'strict' if strict else 'ordinary'}/{'metered' if accounting else 'unlimited'}",
                    "source": prefix + "\ndef run(input)\n" + body + "\nend",
                    "args": [None], "expected": expected, "accounting": accounting,
                    "strict_effects": strict, "block_probe": True, **options,
                })

    for name, call in [
        ("direct", "blocks.once(3)"), ("scoped", "blocks::once(3)"),
        ("indexed", "blocks[:once](3)"), ("computed", "(blocks[:once])(3)"),
        ("symbolic", "blocks.send(:once,3)"), ("public", "blocks.public_send(:once,3)"),
        ("copy", "blocks.dup.once(3)"), ("safe", "blocks&.once(3)"),
        ("nested", "[blocks][0].once(3)"),
    ]:
        add(name, call + " { |n| n+1 }", 4)
    for name, body, expected in [
        ("missing_parameter", "blocks.once(1,2) { |a,b,c| [a,b,c] }", [1,2,None]),
        ("extra_parameter", "blocks.once(1,2,3) { |a,b| [a,b] }", [1,2]),
        ("autosplat", "blocks.once([2,3]) { |a,b| a+b }", 5),
        ("destructure", "blocks.once([[2,3],4]) { |(a,b),c| a+b+c }", 9),
        ("implicit", "blocks.once(3) { _1+1 }", 4),
        ("typed", "blocks.once(3) { |n: int| n+1 }", 4),
        ("invalid_type", 'begin; blocks.once("bad") { |n: int| n }; rescue => e; e.class.to_s; end', "RuntimeError"),
        ("next", "blocks.once(3) { next 4 }", 4),
        ("break", "blocks.once(3) { break 4 }", 4),
        ("return", "blocks.once(3) { return 4 }; 99", 4),
        ("lexical_block", "blocks.once { block_given? }", False),
        ("no_block", "blocks.optional()", False),
        ("optional", 'blocks.optional { raise "unused" }', True),
        ("block_required", "begin; blocks.once(); rescue => e; [e.class.to_s,e.message]; end", ["RuntimeError","block required"]),
        ("each", "blocks.each([1,2,3]) { |n| n*2 }", [2,4,6]),
        ("each_break", "blocks.each([1,2,3]) { |n| break 7 if n==2; n }", 7),
        ("each_next", "blocks.each([1,2,3]) { |n| next 7 if n==2; n }", [1,7,3]),
        ("nested_each", "blocks.each([1,2]) { |i| blocks.each([3,4]) { |j| i+j } }", [[4,5],[5,6]]),
        ("captured_write", "a=[]; blocks.each([1,2,3]) { |n| a.push(n) }; a", [1,2,3]),
        ("outer_loop", "a=[]; for i in [1,2]; a.push(blocks.once(i) { break 7 }); end; a", [7,7]),
        ("local_rescue", 'blocks.once { begin; raise "bad"; rescue; 7; end }', 7),
        ("host_rescue", 'begin; blocks.recover { raise "bad" }; rescue; "script recovered"; end', "recovered"),
        ("retry", 'blocks.once { n=0; begin; n+=1; raise "again" if n<3; n; rescue; retry; end }', 3),
        ("ensure", 'a=[]; begin; blocks.once { begin; raise "bad"; ensure; a.push(1); end }; rescue; a.push(2); ensure; a.push(3); end; a', [1,2,3]),
        ("ensure_override", "blocks.once { begin; return 7; ensure; return 8; end }; 99", 8),
        ("break_contract", "blocks.checked(1) { break 7 }", 7),
        ("bad_break_contract", 'begin; blocks.checked(1) { break "bad" }; rescue => e; e.message; end', "integer result required"),
        ("nonlocal_contract", 'blocks.checked(1) { return "ok" }; 99', "ok"),
        ("presence_contract", 'begin; blocks.checked(1); rescue => e; e.message; end', "one argument and block required"),
        ("keywords", "blocks.keywords(1,tag: 2) { |args,kw| [args,kw.tag] }", [[1],2]),
    ]:
        add(name, body, expected)
    for name, driver, expected in [
        ("relay_host", "blocks.once { yield }; 99", 99),
        ("relay_loop", "for i in [1,2]; yield; end; 99", 99),
        ("relay_each", "[1].each { yield }; 99", 99),
        ("relay_direct", "yield; 99", 7),
    ]:
        add(name, "relay { break 7 }", expected, prefix=f"def relay; {driver}; end")
    add("relay_return", "relay { return 7 }; 99", 7, prefix="def relay; blocks.once { yield }; 99; end")
    add("file", "require(:worker).work(3)", 4, allow_require=True,
        files={"worker.vibe": "def work(n); blocks.once(n) { |x| return x+1 }; 99; end"})
    for name, body in [
        ("ignored_break", "a=[]; r=blocks.ignore { a.push(1); break 7 }; [r,a]"),
        ("ignored_return", "a=[]; blocks.ignore { a.push(1); return [7,a] }; [99,a]"),
    ]:
        add(name, body, [7,[1]], go=[99,[1,1]], policy="preserve_host_block_control",
            reason="Selected control-flow policy: a host cannot erase a pending block break or return by ignoring its error; further block invocations do not rerun script code.")
    for name, member in [("indexed", "blocks[:once]"), ("scoped", "blocks::once")]:
        add("detached_"+name, f'begin; method={member}; method(3) {{ |n| n+1 }}; rescue; "attached-method-required"; end',
            "attached-method-required", go=4, policy="attached_capability_methods",
            reason="ADR-006 keeps block-capable host methods attached too; immediate calls remain available.")
    return result
