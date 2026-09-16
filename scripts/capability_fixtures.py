"""Shared capability-binding and host-contract expectations."""


def cases():
    result = []

    def add(name, body, expected, prefix="", **options):
        for strict in [False, True]:
            for accounting in [False, True]:
                result.append({
                    "name": f"capabilities/{name}/{'strict' if strict else 'ordinary'}/{'metered' if accounting else 'unlimited'}",
                    "source": prefix + "\ndef run(input)\n" + body + "\nend",
                    "args": [None], "expected": expected, "accounting": accounting,
                    "strict_effects": strict, "capability_probe": True, **options,
                })

    for name, body in [
        ("direct", "host.echo(1, 2)"),
        ("bare", "host.echo 1, 2"),
        ("scoped", "host::echo(1, 2)"),
        ("indexed", "host[:echo](1, 2)"),
        ("computed", "(host[:echo])(1, 2)"),
        ("symbolic", "host.send(:echo, 1, 2)"),
        ("public", "host.public_send(:echo, 1, 2)"),
        ("alias", "other=host;other.echo(1, 2)"),
        ("array", "[host][0].echo(1, 2)"),
        ("copy", "host.dup.echo(1, 2)"),
        ("clone", "host.clone.echo(1, 2)"),
        ("iterator_name", "host.map(1, 2)"),
        ("safe", "host&.echo(1, 2)"),
    ]:
        add(name, body, [[1, 2], {}])
    for name, body in [
        ("keywords", "host.echo(1, tag: 2)"),
        ("splats", "host.echo(*[1], **{tag: 2})"),
        ("keyword_override", "host.echo(1, tag: 8, **{tag: 2})"),
        ("keyword_symbolic", "host.public_send(:echo, 1, tag: 2)"),
    ]:
        add(name, body, [[1], {"tag": 2}])
    add("counter", "[host.next(), host.next()]", [1, 2])
    add("argument_order", "host.echo(host.next(), host.next(), tag: host.next())", [[1, 2], {"tag": 3}])
    add("checked", "[host.checked(7), host.next()]", [7, 2])
    add("checked_symbolic", "[host.send(:checked, 7), host.next()]", [7, 2])
    add("factory", "[host.factory().checked(7), host.next()]", [7, 2])
    add("invalid_argument", 'begin;host.checked("bad");rescue;nil;end;host.next()', 1)
    add("invalid_keywords", "begin;host.checked(1, bad: 2);rescue;nil;end;host.next()", 1)
    add("invalid_arity", "begin;host.checked();rescue;nil;end;host.next()", 1)
    add("invalid_result", "begin;host.checked(0);rescue;nil;end;host.next()", 2)
    add("factory_contract", 'begin;host.factory().checked("bad");rescue;nil;end;host.next()', 1)
    add("host_error", "begin;host.fail();rescue;7;end", 7)
    add("ensure", "a=[];begin;host.fail();rescue;a.push(1);ensure;a.push(2);end;a", [1, 2])
    add("initializer", "[M.C, host.next()]", [1, 2], prefix="module M; C=host.next(); end")
    add("default", "fetch()", [[1], {}], prefix="def fetch(n=host.next());host.echo(n);end")
    add("shadow_parameter", "fetch({items: [9]})", [9], prefix="def fetch(host);host.items;end")
    add("override_global", "host", None, globals={"host": None})
    add("nested_script", "[fetch(), host.next()]", [1, 2], prefix="def fetch;host.next();end")
    add("missing_skips_args", "begin;fetch();rescue;host.next();end", 1,
        prefix="def fetch;host.missing(host.next());end")
    add("responds", "[host.respond_to?(:echo),host.respond_to?(:missing),host.respond_to?(:items)]", [True, False, False])
    add("file", "require(:send).deliver", [[7], {}], allow_require=True,
        files={"send.vibe": "def deliver;host.echo(7);end"})
    for name, read in [("indexed", "host[:checked]"), ("scoped", "host::checked")]:
        add(f"detached_{name}", f'begin;method={read};method(7);rescue;"attached-method-required";end',
            "attached-method-required", go=7, policy="attached_capability_methods",
            reason="Capability methods stay attached under ADR-006; immediate calls remain supported.")
    return result
