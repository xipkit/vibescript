"""Required-file conformance inputs and isolated fixture materialization."""
from pathlib import Path


def cases():
    result = []

    def add(name, body, expected, files, prefix="", expected_by_strict=None, returns=None, static_error=None, **options):
        static_error = {
            'argument_error_class/existing_alias': {'code': 'V0102', 'at': [3, 8]},
            'scoped': {'code': 'V0416', 'at': [3, 22]},
        }.get(name, static_error)
        for development in [False, True]:
            for strict in [False, True]:
                result.append({
                    "name": f"required_files/{name}/{'development' if development else 'production'}/{'strict' if strict else 'ordinary'}",
                    "source": prefix + "\ndef run(input: any)" + (f" -> {returns}" if returns else "") + "\n" + body + "\nend",
                    "args": [None], "expected": expected_by_strict[int(strict)] if expected_by_strict is not None else expected, "accounting": True,
                    "files": files, "module_development": development,
                    "strict_effects": strict, "allow_require": True, **options,
                })
                if static_error:
                    result[-1]["static_error"] = static_error

    answer = {"answer.vibe": "def answer -> int;42;end"}
    # A required file cannot statically depend on undeclared receiving-script functions.
    FILE_LOCAL = {"code": "V0201", "at": None}
    # `require` takes string literals (ADR-008), so a computed name or alias is a static rejection.
    for name, expression, rejected in [
        ("missing", "require()", None),
        ("nil", "require(nil)", "nil"),
        ("integer", "require(1)", "1"),
        ("extra", 'require("answer", "extra")', None),
        ("block", 'require("answer") { 1 }', None),
        ("keyword", 'require("answer", unknown: true)', "true"),
        ("nil_alias", 'require("answer", as: nil)', "nil"),
        ("boolean_alias", 'require("answer", as: true)', "true"),
        ("invalid_alias", "require(\"answer\", as: 'bad-name')", None),
        ("empty_alias", "require(\"answer\", as: '')", None),
        ("invalid_utf8_alias", 'require("answer", as: "\\xFF")', None),
        ("builtin_alias", 'require("answer", as: "Math")', None),
        ("existing_alias", 'Taken=1; require("answer", as: "Taken")', None),
    ]:
        add("argument_error_class/"+name,
            "begin; "+expression+"; nil; rescue ArgumentError; 'wrong handler'; rescue RuntimeError => e; [e.class.to_s,e.message.start_with?('require')]; end",
            ["RuntimeError", True], {"answer.vibe":"raise 'initializer ran'; def answer -> int;42;end"},
            returns="string | [string, bool] | nil",
            static_error={"code": "V0309", "at": [3, 8 + expression.rindex(rejected)]} if rejected else None)
    for name, body, static_error in [
        ("direct", 'require("answer").answer', None),
        ("symbol", "require(:answer).answer", {"code": "V0309", "at": [3, 9]}),
        ("extension", 'require("answer.vibe").answer', None),
        ("alias", 'require("answer",as: "Answers");Answers.answer', None),
        ("root_export", 'require("answer");answer', None),
        ("scoped", 'm=require("answer");m::answer()', None),
        ("indexed", 'm=require("answer");m["answer"]()', {"code": "V0112", "at": [3, 21]}),
        ("symbolic", 'm=require("answer");m.send(:answer)', {"code": "V0405", "at": [3, 23]}),
    ]:
        add(name, body, 42, answer, returns="int", static_error=static_error)
    add("root_conflict", 'm=require("answer");[answer,m.answer]', [7,42], answer,
        prefix="def answer -> int;7;end", returns="array<int>")
    add("global_conflict", 'm=require("answer");[answer,m.answer]', [9,42], answer,
        globals={"answer":9}, returns="array<int>")
    add("alias_conflict", 'begin;require("answer",as: "Taken");rescue;nil;end;Taken', 9, answer,
        globals={"Taken":9}, returns="int")
    add("block", 'require("apply").apply(3){|n|n*2}', 8,
        {"apply.vibe":"def apply(n: int, &block: int -> int) -> int;yield(n+1);end"}, returns="int")
    add("arguments", 'm=require("args");m.combine(1,*[2,3],**{extra:4})', [1,[2,3],4],
        {"args.vibe":"def combine(first: int,*rest: array<int>,extra: int = 0) -> [int, array<int>, int];[first,rest,extra];end"},
        returns="[int, array<int>, int]")
    add("private_helper", 'require("answer").answer', 42,
        {"answer.vibe":"private def hidden -> int;41;end;def answer -> int;hidden+1;end"}, returns="int")
    add("private_not_exported", 'begin;probe;rescue;"outer lookup error";end', 7,
        {"answer.vibe":"private def hidden -> int;41;end;def answer -> int;hidden+1;end"},
        prefix='def probe -> int;m=require("answer");begin;m.hidden;rescue;7;end;end', returns="int | string",
        static_error={"code": "V0203", "at": [1, 46]},
        go="outer lookup error", policy="catch_lookup_errors",
        reason="A missing required-module member raises at its lookup so the local rescue can catch it.")
    add("private_state", 'm=require("counter");[m.bump,m.bump]', [1,2],
        {"counter.vibe":"count=0;def bump -> int;count+=1;count;end"}, returns="array<int>")
    add("same_module", 'a=require("counter");b=require("counter.vibe");[a.bump,b.bump]', [1,2],
        {"counter.vibe":"count=0;def bump -> int;count+=1;count;end"}, returns="array<int>")
    add("collection_state", 'm=require("rows");[m.bump,m.bump]', [[1],[1,1]],
        {"rows.vibe":"rows: array<int> = [];def bump -> array<int>;rows.push(1);rows;end"}, returns="array<array<int>>",
        go=[[1,1],[1,1]], policy="documented_value_semantics",
        reason="A returned collection remains a logical value when a later file function mutates its private binding.")
    add("relative", 'require("pkg/main").answer', 42,
        {"pkg/main.vibe":"def answer -> int;require(\"./helper\").value;end",
         "pkg/helper.vibe":"def value -> int;42;end"}, returns="int")
    add("relative_parent", 'require("pkg/nested/main").answer', 42,
        {"pkg/nested/main.vibe":"def answer -> int;require(\"../helper\").value;end",
         "pkg/helper.vibe":"def value -> int;42;end"}, returns="int")
    add("nested", 'require("outer").answer', 42,
        {"outer.vibe":"def answer -> int;require(\"inner\").value;end",
         "inner.vibe":"def value -> int;42;end"}, returns="int")
    add("cycle", 'begin;require("left");rescue;7;end', 7,
        {"left.vibe":"require(\"right\")", "right.vibe":"require(\"left\")"}, returns="any", static_error={'code': 'V0201', 'at': None})
    add("missing", 'begin;require("missing");rescue;7;end', 7, {}, returns="any", static_error={'code': 'V0201', 'at': [3, 7]})
    add("syntax_error", 'begin;require("broken");rescue;7;end', 7,
        {"broken.vibe":"def answer("}, returns="any", static_error={'code': 'V0201', 'at': [3, 7]})
    add("failed_initializer_retry", '[1,2].map{begin;require("broken");rescue;7;end}', [7,7],
        {"broken.vibe":"raise \"broken\";def answer -> int;42;end"}, returns="array<any>")
    add("allow", 'require("answer").answer', 42, answer, module_allow=["answer"], returns="int")
    add("deny", 'begin;require("answer");rescue;7;end', 7, answer,
        module_allow=["*"], module_deny=["answer"], returns="any", static_error={'code': 'V0201', 'at': [3, 7]})
    add("not_allowed", 'begin;require("answer");rescue;7;end', 7, answer,
        module_allow=["other"], returns="any", static_error={'code': 'V0201', 'at': [3, 7]})
    add("require_permission", 'begin;require("answer").answer;rescue;7;end', 42, answer,
        allow_require=False, expected_by_strict=(42,7), returns="int")
    add("host_global", 'require("answer").answer', 42,
        {"answer.vibe":"def answer -> int;setting+1;end"}, globals={"setting":41}, returns="int")
    add("file_assignment", 'm=require("answer");[m.answer,setting]', [7,41],
        {"answer.vibe":"setting=7;def answer -> int;setting;end"}, globals={"setting":41}, returns="array<int>")
    add("receiving_function", 'require("answer").answer', 42,
        {"answer.vibe":"def answer -> int;helper(21);end"}, prefix="def helper(n: int) -> int;n*2;end", returns="int",
        static_error=FILE_LOCAL)
    add("same_name_call_skips_file_scope",
        'begin;require("m");nil;rescue => e;[e.class.to_s,e.message];end',
        ["RuntimeError","undefined variable helper"],
        {"m.vibe":"def helper -> int;1;end;helper=helper()"}, returns="array<string>?")
    add("same_name_call_forms_skip_file_scope",
        '[:plus,:both,:args,:block,:nested,:func,:body].map{|m| begin;require(m);nil;rescue => e;e.message;end}',
        ["undefined variable helper"]*6+["unknown class member helper"],
        {"plus.vibe":"def helper;1;end;helper+=helper()",
         "both.vibe":"def helper;1;end;helper&&=helper()",
         "args.vibe":"def helper(x);x;end;helper=helper 2",
         "block.vibe":"def helper;yield;end;helper=helper { 3 }",
         "nested.vibe":"def helper;1;end;[1].each{|i| helper=[helper()]}",
         "func.vibe":"def helper;1;end;def other;helper=helper();helper;end;other()",
         "body.vibe":"def helper;1;end;class K;helper=helper();end"},
        returns="array<string?>", static_error={"code": "V0309", "at": [3, 70]})
    add("same_name_call_other_forms_keep_file_scope",
        '[:bare,:other,:either,:param,:block_param].map{|m| require(m).peek}', [1,1,1,1,[1]],
        {"bare.vibe":"def helper;1;end;helper=helper;def peek;helper;end",
         "other.vibe":"def helper;1;end;x=helper();def peek;x;end",
         "either.vibe":"def helper;1;end;helper||=helper();def peek;helper;end",
         "param.vibe":"def helper;1;end;def f(helper);helper=helper();helper;end;def peek;f(3);end",
         "block_param.vibe":"def helper;1;end;def peek;[5].map{|helper| helper=helper()};end"},
        returns="array<any>", static_error={"code": "V0309", "at": [3, 60]})
    add("same_name_call_reaches_root_bindings",
        '[require("m").peek,require("outer").peek,begin;require("late");other;end]', [2,7,1],
        {"m.vibe":"def helper -> int;1;end;helper=helper();def peek -> int;helper;end",
         "inner.vibe":"def value -> int;7;end",
         "outer.vibe":"require(\"inner\");def value -> int;1;end;value=value();def peek -> int;value;end",
         "late.vibe":"def value2 -> int;1;end;def other -> int;value2=value2();value2;end"},
        prefix="def helper -> int;2;end", returns="array<int>")
    member = lambda name, member="to_s": f"a function has no member {member}; call {name}(...) directly"
    add("function_member_receivers",
        '[:top,:safe,:paren,:args,:bare_call,:empty_call,:argument_call,:block,:func,:method,:body,:arity,:private,:same]'
        '.map{|m| begin;require(m);nil;rescue => e;e.message;end}',
        [None]*4+[member("helper", "call")]*2+["unknown int method call"]
        +[None]*4+["helper is a function and cannot be used as a value; call it with helper(...)"]+[None]*2,
        {"top.vibe":"def helper;1;end;x=helper.to_s",
         "safe.vibe":"def helper;1;end;x=helper&.to_s",
         "paren.vibe":"def helper;1;end;x=helper.to_s()",
         "args.vibe":"def helper;[1];end;x=helper.fetch(0)",
         "bare_call.vibe":"def helper;1;end;x=helper.call",
         "empty_call.vibe":"def helper;1;end;x=helper.call()",
         "argument_call.vibe":"def helper;1;end;x=helper.call(1)",
         "block.vibe":"def helper;1;end;[1].each{|i| helper.to_s}",
         "func.vibe":"def helper;1;end;def peek;helper.to_s;end;peek()",
         "method.vibe":"def helper;1;end;class K;def go;helper.to_s;end;end;K.new.go",
         "body.vibe":"def helper;1;end;class K;X=helper.to_s;end",
         "arity.vibe":"def helper(a);1;end;x=helper.to_s",
         "private.vibe":"def _helper;1;end;x=_helper.to_s",
         "same.vibe":"def helper;1;end;helper=helper.to_s"},
        returns="array<string?>", static_error={"code": "V0309", "at": [3, 136]})
    add("function_member_receiver_exports",
        'require("m");[helper,helper[0],begin;helper.to_s;rescue => e;e.message;end,begin;helper.call(1);rescue => e;e.message;end]',
        [[7], 7, "[7]", "unknown array method call"],
        {"m.vibe":"def helper -> array<int>;[7];end"}, returns="array<any>", static_error={"code": "V0203", "at": [3, 89]})
    add("function_member_receiver_values",
        '[require("m").peek,require("other").peek]',
        [[[1], 1, [1, 2], 2], ["7", member("value", "call"), "unknown int method call"]],
        {"m.vibe":"def helper -> array<int>;[1];end;def peek -> array<any>;[helper,helper[0],helper+[2],helper[0]+1];end",
         "other.vibe":"def peek -> array<string>;[value.to_s,begin;value.call;rescue => e;e.message;end,"
                      "begin;value.call(1);rescue => e;e.message;end];end"},
        prefix="def value -> int;7;end", returns="array<array<any>>", static_error=FILE_LOCAL)
    add("function_member_mutators",
        'require("m");a=m_peek;b=begin;helper.pop;rescue => e;e.message;end;helper[0]=5;helper<<3;[a,b,helper]',
        [[1, [1, 2], [1]], 1, [1]],
        {"m.vibe":"def helper -> array<int>;[1];end;def m_peek -> array<any>;a=begin;helper.pop;rescue => e;e.message;end;"
                  "b=begin;helper.push(2);rescue => e;e.message;end;helper[0]=5;helper<<3;[a,b,helper];end"},
        returns="array<any>")
    add("enum", 'm=require("state");[m.State::Ready.name,m.name(:ready)]', ["Ready","Ready"],
        {"state.vibe":"enum State;Ready;Done;end;def name(state: State) -> string;state.name;end"}, returns="array<string>")
    add("class", 'm=require("box");[m.make(7).value,m.make(9).value]', [7,9],
        {"box.vibe":"class Box;property value: int;def initialize(@value: int);end;end;def make(n: int) -> Box;Box.new(n);end"},
        returns="array<int>")
    add("initializer_order", 'm=require("state");[m.seen,m.seen]', [["class","file"],["class","file"]],
        {"state.vibe":"class State;events.push(\"class\");end;events.push(\"file\");def seen -> array<any>;events;end"},
        globals={"events":[]}, returns="array<array<any>>")
    return result


def materialize(cases, directory):
    directory = Path(directory).resolve()
    prepared = []
    for index, original in enumerate(cases):
        case = dict(original)
        files = case.pop("files", None)
        if files is not None:
            root = directory / "module-files" / f"{index:06}"
            root.mkdir(parents=True, exist_ok=False)
            for name, source in files.items():
                relative = Path(name)
                if relative.is_absolute() or not relative.parts or ".." in relative.parts:
                    raise ValueError(f"invalid fixture path: {name!r}")
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                with path.open("x", encoding="utf-8") as target:
                    target.write(source)
            case["module_paths"] = [str(root)]
        prepared.append(case)
    return prepared
