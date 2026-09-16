"""Required-file conformance inputs and isolated fixture materialization."""
from pathlib import Path


def cases():
    result = []

    def add(name, body, expected, files, prefix="", expected_by_strict=None, **options):
        for development in [False, True]:
            for strict in [False, True]:
                result.append({
                    "name": f"required_files/{name}/{'development' if development else 'production'}/{'strict' if strict else 'ordinary'}",
                    "source": prefix + "\ndef run(input)\n" + body + "\nend",
                    "args": [None], "expected": expected_by_strict[int(strict)] if expected_by_strict is not None else expected, "accounting": True,
                    "files": files, "module_development": development,
                    "strict_effects": strict, "allow_require": True, **options,
                })

    answer = {"answer.vibe": "def answer;42;end"}
    for name, body in [
        ("direct", 'require("answer").answer'),
        ("symbol", "require(:answer).answer"),
        ("extension", 'require("answer.vibe").answer'),
        ("alias", 'require("answer",as: :Answers);Answers.answer'),
        ("root_export", 'require("answer");answer'),
        ("scoped", 'm=require("answer");m::answer()'),
        ("indexed", 'm=require("answer");m[:answer]()'),
        ("symbolic", 'm=require("answer");m.send(:answer)'),
    ]:
        add(name, body, 42, answer)
    add("root_conflict", 'm=require("answer");[answer,m.answer]', [7,42], answer,
        prefix="def answer;7;end")
    add("global_conflict", 'm=require("answer");[answer,m.answer]', [9,42], answer,
        globals={"answer":9})
    add("alias_conflict", 'begin;require("answer",as: :Taken);rescue;nil;end;Taken', 9, answer,
        globals={"Taken":9})
    add("block", 'require("apply").apply(3){|n|n*2}', 8,
        {"apply.vibe":"def apply(n);yield(n+1);end"})
    add("arguments", 'm=require(:args);m.combine(1,*[2,3],**{extra:4})', [1,[2,3],4],
        {"args.vibe":"def combine(first,*rest,extra:0);[first,rest,extra];end"})
    add("private_helper", 'require(:answer).answer', 42,
        {"answer.vibe":"private def hidden;41;end;def answer;hidden+1;end"})
    add("private_not_exported", 'begin;probe();rescue;"outer lookup error";end', 7,
        {"answer.vibe":"private def hidden;41;end;def answer;hidden+1;end"},
        prefix='def probe;m=require(:answer);begin;m.hidden;rescue;7;end;end',
        go="outer lookup error", policy="catch_lookup_errors",
        reason="A missing required-module member raises at its lookup so the local rescue can catch it.")
    add("private_state", 'm=require(:counter);[m.bump,m.bump]', [1,2],
        {"counter.vibe":"count=0;def bump;count+=1;count;end"})
    add("same_module", 'a=require(:counter);b=require("counter.vibe");[a.bump,b.bump]', [1,2],
        {"counter.vibe":"count=0;def bump;count+=1;count;end"})
    add("collection_state", 'm=require(:rows);[m.bump,m.bump]', [[1],[1,1]],
        {"rows.vibe":"rows=[];def bump;rows.push(1);rows;end"},
        go=[[1,1],[1,1]], policy="documented_value_semantics",
        reason="A returned collection remains a logical value when a later file function mutates its private binding.")
    add("relative", 'require("pkg/main").answer', 42,
        {"pkg/main.vibe":"def answer;require(\"./helper\").value;end",
         "pkg/helper.vibe":"def value;42;end"})
    add("relative_parent", 'require("pkg/nested/main").answer', 42,
        {"pkg/nested/main.vibe":"def answer;require(\"../helper\").value;end",
         "pkg/helper.vibe":"def value;42;end"})
    add("nested", 'require(:outer).answer', 42,
        {"outer.vibe":"inner=require(:inner);def answer;inner.value;end",
         "inner.vibe":"def value;42;end"})
    add("cycle", 'begin;require(:left);rescue;7;end', 7,
        {"left.vibe":"require(:right)", "right.vibe":"require(:left)"})
    add("missing", 'begin;require(:missing);rescue;7;end', 7, {})
    add("syntax_error", 'begin;require(:broken);rescue;7;end', 7,
        {"broken.vibe":"def answer("})
    add("failed_initializer_retry", '[1,2].map{begin;require(:broken);rescue;7;end}', [7,7],
        {"broken.vibe":"raise \"broken\";def answer;42;end"})
    add("allow", 'require(:answer).answer', 42, answer, module_allow=["answer"])
    add("deny", 'begin;require(:answer);rescue;7;end', 7, answer,
        module_allow=["*"], module_deny=["answer"])
    add("not_allowed", 'begin;require(:answer);rescue;7;end', 7, answer,
        module_allow=["other"])
    add("require_permission", 'begin;require(:answer).answer;rescue;7;end', 42, answer,
        allow_require=False, expected_by_strict=(42,7))
    add("host_global", 'require(:answer).answer', 42,
        {"answer.vibe":"def answer;setting+1;end"}, globals={"setting":41})
    add("file_assignment", 'm=require(:answer);[m.answer,setting]', [7,41],
        {"answer.vibe":"setting=7;def answer;setting;end"}, globals={"setting":41})
    add("receiving_function", 'require(:answer).answer', 42,
        {"answer.vibe":"def answer;helper(21);end"}, prefix="def helper(n);n*2;end")
    add("enum", 'm=require(:state);[m.State::Ready.name,m.name(:ready)]', ["Ready","Ready"],
        {"state.vibe":"enum State;Ready;Done;end;def name(state:State);state.name;end"})
    add("class", 'm=require(:box);[m.make(7).value,m.make(9).value]', [7,9],
        {"box.vibe":"class Box;property value;def initialize(@value);end;end;def make(n);Box.new(n);end"})
    add("initializer_order", 'm=require(:state);[m.seen,m.seen]', [["class","file"],["class","file"]],
        {"state.vibe":"class State;events.push(\"class\");end;events.push(\"file\");def seen;events;end"},
        globals={"events":[]})
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
