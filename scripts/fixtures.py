#!/usr/bin/env python3
"""Generate identical inputs and independently computed expected results."""
import json
import hashlib
import random
import sys
from pathlib import Path
from module_fixtures import cases as module_cases, materialize
from capability_fixtures import cases as capability_cases
from block_fixtures import cases as block_cases
from signature_fixtures import cases as signature_cases

UPSTREAM=Path(__file__).resolve().parent.parent/"tests/upstream"
SITE=UPSTREAM.parent/"site"


def upstream_cases():
    manifest=json.loads((UPSTREAM/"sources.json").read_text())
    for entry in manifest["files"]:
        assert hashlib.sha256((UPSTREAM/entry["path"]).read_bytes()).hexdigest()==entry["sha256"],entry["path"]
    out=[]
    for i,case in enumerate(json.loads((UPSTREAM/"cases.json").read_text())):
        out.append({"name":f"upstream/{i:02}/{case['path']}::{case['function']}","source":(UPSTREAM/case["path"]).read_text(),"function":case["function"],"args":case["args"],"expected":case["expected"],"accounting":True})
    return out


def site_cases():
    manifest=json.loads((SITE/"sources.json").read_text())
    for entry in manifest["files"]:
        assert hashlib.sha256((SITE/entry["path"]).read_bytes()).hexdigest()==entry["sha256"],entry["path"]
    settings=json.loads((SITE/"harness.json").read_text())
    return [{**case,**settings.get(case["path"],{}),"name":"site/"+case["path"],"source":(SITE/case["path"]).read_text(),"accounting":True} for case in json.loads((SITE/"cases.json").read_text())]


def site_benchmark_cases():
    """Every site program, metered and unlimited, for timing whole programs.

    Notification programs are left out: each harness accumulates their host
    side effects across repeated calls in its own way.
    """
    out=[]
    for case in site_cases():
        if case.get("notifications"):
            continue
        for accounting in [True,False]:
            out.append({**case,"name":case["name"]+("/metered" if accounting else "/unlimited"),"accounting":accounting,"iterations":100})
    return out


def encoding_cases():
    out=[]
    for case in json.loads((UPSTREAM.parent/"encoding-cases.json").read_text()):
        out.append({"name":"encoding/"+case["name"],"source":case["source"],"args":[None],"expected":case["expected"],"accounting":True,"result_encoding":"typed"})
        if "static_error" in case:
            out[-1]["static_error"]=case["static_error"]
    return out


def function(body, returns=None, param="any"):
    """Wraps a body as the entry function `run(input: param) -> returns`; without `returns` it returns nil."""
    return f"def run(input: {param})" + (f" -> {returns}" if returns else "") + "\n" + body + "\nend"


def json_depth_cases():
    cases = []
    for shape in ["array", "hash", "mixed"]:
        for depth, empty in [(128, False), (129, False), (9999, False), (10000, False), (10000, True), (10001, False)]:
            levels = depth - int(empty)
            opens = ["[" if shape == "array" or (shape == "mixed" and i % 2 == 0) else '{"k":' for i in range(levels)]
            text = "".join(opens) + ("[]" if empty else "0") + "".join("]" if opener == "[" else "}" for opener in reversed(opens))
            body = "JSON.stringify(JSON.parse(input)) == input" if depth <= 10000 else "begin; JSON.parse(input); false; rescue LimitError; true; end"
            cases.append(dict(name=f"json_depth/{shape}/{depth}/{'empty' if empty else 'scalar'}", source=function(body, "bool", "string"), args=[text], expected=True, accounting=True))
            if depth == 10000 and not empty:
                for target, source in [
                    ("instance", "class Box; property value: any; @flag: int = 0; def initialize(@value: any); @flag=0; end; def touch; @flag+=1; end; end; def run(input: string) -> bool; box=Box.new(JSON.parse(input)); box.touch; box.value=box.value; JSON.stringify(box.value)==input; end"),
                    ("module", "module Box; @@value: any = nil; @@flag: int = 0; def self.put(v: any); @@value=v; @@flag=1; end; def self.get -> any; @@value; end; end; def run(input: string) -> bool; Box.put(JSON.parse(input)); JSON.stringify(Box.get)==input; end"),
                ]:
                    cases.append(dict(name=f"json_depth/{shape}/{depth}/{target}", source=source, args=[text], expected=True, accounting=True))
    return cases


def benchmark_cases():
    cases = []

    def add(name, body, argument, expected, source=None, returns=None, param=None):
        cases.append(dict(name=name, source=source or function(body, returns, param), args=[argument], expected=expected))

    add("numeric_loop", "i=0\ntotal=0\nwhile i<input\n total+=i\n i+=1\nend\ntotal", 1000, 499500, returns="int", param="int")
    add("function_calls", "", 500, 125250, source="def add(a: int,b: int) -> int\n a+b\nend\n"+function("i=0\ntotal=0\nwhile i<input\n total=add(total,i+1)\n i+=1\nend\ntotal", "int", "int"))
    add("array_sum", "input.sum", list(range(1000)), 499500, returns="int", param="array<int>")
    add("array_growth", "a: array<int> = []\ni=0\nwhile i<input\n a.push(i)\n i+=1\nend\na.sum", 128, 8128, returns="int", param="int")
    add("hash_lookup", "i=0\ntotal=0\nwhile i<1000\n total+=input.fetch(\"k31\")\n i+=1\nend\ntotal", {f"k{i:02}": i for i in range(64)}, 31000, returns="int", param="hash<string, int>")
    for size in [8, 512, 2048]:
        obj={f"k{i:05}":i for i in range(size)}
        key=f"k{size-1:05}"
        add(f"hash_lookup_{size}",f'i=0\ntotal=0\nwhile i<128\n total+=input.fetch("{key}")\n i+=1\nend\ntotal',obj,128*(size-1),returns="int",param="hash<string, int>")
        add(f"json_object_{size}","JSON.parse(input)",json.dumps(obj,separators=(",",":")),obj,returns="any",param="string")
    keys=[f"k{i:05}" for i in range(512)]
    obj=dict(zip(keys,range(512)))
    add("hash_build_512",'h: hash<string, int> = {}\ni=0\nwhile i<input.length\n h[input.fetch(i)]=i\n i+=1\nend\nh["k00511"]',keys,511,returns="int?",param="array<string>")
    add("hash_replace_512",'h=input\ni=0\nwhile i<128\n h["k00511"]=i\n i+=1\nend\nh["k00511"]',obj,127,returns="int?",param="hash<string, int>")
    add("hash_equal_512","input[0]==input[1]",[obj,dict(reversed(list(obj.items())))],True,returns="bool",param="array<hash<string, int>>")
    duplicates="{"+",".join(json.dumps(k)+":"+str(v) for k,v in [*obj.items(),*((k,1024+i) for i,k in enumerate(reversed(keys)))])+"}"
    expected={k:1024+511-i for i,k in enumerate(keys)}
    add("json_duplicates_512","JSON.parse(input)",duplicates,expected,returns="any",param="string")
    for size in [16, 4096, 65536]:
        text=("aBcD9_! "*((size+7)//8))[:size]
        add(f"length_{size}", "input.length", text, len(text), returns="int", param="string")
        add(f"upcase_{size}", "input.upcase(:ascii)", text, text.upper(), returns="string", param="string")
    text="a"*65536
    add("strip_65536", "input.strip", "  "+text+"  ", text, returns="string", param="string")
    unicode_text="é界🙂"*4096
    add("length_unicode", "input.length", unicode_text, len(unicode_text), returns="int", param="string")
    add("length_mixed_65536", "input.length", text[:-1]+"é", len(text), returns="int", param="string")
    for name,text in [("ascii_64k",text),("escaped_4k",'a\n\t"\\'*820),("unicode_4k","é界🙂"*456)]:
        obj={"id":7,"payload":text}
        raw=json.dumps(obj,ensure_ascii=False,separators=(",",":"),sort_keys=True)
        add("json_parse_"+name,"JSON.parse(input)",raw,obj,returns="any",param="string")
        add("json_stringify_"+name,"JSON.stringify(input)",obj,raw,returns="string",param="{ id: int, payload: string }")
    rows=[{"id":i,"name":f"record-{i}","score":i*3,"active":i%2==0} for i in range(64)]
    raw=json.dumps(rows,ensure_ascii=False,separators=(",",":"),sort_keys=True)
    expected=json.dumps({"total":sum(row["score"] for row in rows),"count":len(rows)},separators=(",",":"))
    add("json_transform",'rows=JSON.parse_as(input, array<{ active: bool, id: int, name: string, score: int }>)\ni=0\ntotal=0\nwhile i<rows.length\n total+=rows.fetch(i)["score"]\n i+=1\nend\nJSON.stringify({total:total,count:rows.length})',raw,expected,returns="string",param="string")
    # Loops: numeric work, blocks, member calls and string building.
    add("loop_float","x=0.0\ni=0\nwhile i<input\n x=x*0.5+1.25\n i+=1\nend\nx",1000,2.5,returns="float",param="int")
    add("loop_range","total=0\nfor i in 1..input\n total+=i*i%7\nend\ntotal",1000,sum(i*i%7 for i in range(1,1001)),returns="int",param="int")
    add("loop_branches","i=0\nhits=0\nwhile i<input\n if i%3==0 && i != 9\n  hits+=1\n elsif i>900\n  hits+=2\n end\n i+=1\nend\nhits",1000,sum(1 if i%3==0 and i!=9 else 2 if i>900 else 0 for i in range(1000)),returns="int",param="int")
    add("array_each","total=0\ninput.each { |n| total+=n }\ntotal",list(range(1000)),499500,returns="int",param="array<int>")
    add("array_map_select","input.map { |n| n*3 }.select { |n| n%2==0 }.length",list(range(1000)),500,returns="int",param="array<int>")
    add("nested_blocks","total=0\ninput.each { |a|\n input.each { |b| total+=a*b }\n}\ntotal",list(range(32)),sum(range(32))**2,returns="int",param="array<int>")
    # Service loops, with expectations computed independently of the script.
    sales=[{"price":100+i*7,"qty":i%5+1,"active":i%7!=0,"bucket":f"region-{i%8}"} for i in range(256)]
    sale_type="array<{ price: int, qty: int, active: bool, bucket: string }>"
    active=[row for row in sales if row["active"]]
    add("loop_record_totals",'total=0\nunits=0\ninput.each { |row|\n next if !row["active"]\n total+=row["price"]*row["qty"]\n units+=row["qty"]\n}\n[total,units]',sales,[sum(row["price"]*row["qty"] for row in active),sum(row["qty"] for row in active)],returns="[int, int]",param=sale_type)
    counts={}
    for row in active:
        counts[row["bucket"]]=counts.get(row["bucket"],0)+1
    add("loop_bucket_counts",'counts: hash<string, int> = {}\ninput.each { |row|\n next if !row["active"]\n key=row["bucket"]\n counts[key]=counts.fetch(key,0)+1\n}\ncounts',sales,counts,returns="hash<string, int>",param=sale_type)
    nested_total=0
    for a in range(32):
        if a%3==0:
            continue
        for b in range(32):
            if b>a+4:
                break
            if (a+b)%2==0:
                continue
            nested_total+=a*b
    add("loop_nested_control","total=0\nfor a in 0...input\n next if a%3==0\n for b in 0...input\n  break if b>a+4\n  next if (a+b)%2==0\n  total+=a*b\n end\nend\ntotal",32,nested_total,returns="int",param="int")
    add("block_nested_control","total=0\ninput.each { |a|\n next if a%3==0\n input.each { |b|\n  break if b>a+4\n  next if (a+b)%2==0\n  total+=a*b\n }\n}\ntotal",list(range(32)),nested_total,returns="int",param="array<int>")
    add("loop_times","total=0\ninput.times { |i| total+=i*i%7 }\ntotal",1000,sum(i*i%7 for i in range(1000)),returns="int",param="int")
    add("array_each_index","total=0\ninput.each_with_index { |n,i| total+=n*(i%5) }\ntotal",list(range(1000)),sum(n*(i%5) for i,n in enumerate(range(1000))),returns="int",param="array<int>")
    for collection,argument,param in [("array",list(range(1000)),"array<int>"),("range",1000,"int")]:
        receiver="input" if collection=="array" else "(0...input)"
        if collection=="range":
            add("range_each",f"total=0\n{receiver}.each {{ |n| total+=n }}\ntotal",argument,499500,returns="int",param=param)
        add(collection+"_map",f"{receiver}.map {{ |n| n*3+1 }}",argument,[n*3+1 for n in range(1000)],returns="array<int>",param=param)
        add(collection+"_select",f"{receiver}.select {{ |n| n%3==0 }}",argument,[n for n in range(1000) if n%3==0],returns="array<int>",param=param)
        add(collection+"_reduce",f"{receiver}.reduce(0) {{ |total,n| total+n*n%7 }}",argument,sum(n*n%7 for n in range(1000)),returns="int",param=param)
    add("loop_array_build","out: array<int> = []\nfor i in 0...input\n next if i%3==0\n out << i*2\nend\nout",1000,[i*2 for i in range(1000) if i%3!=0],returns="array<int>",param="int")
    labels=[f"key-{i}" for i in range(256)]
    add("loop_string_build",'out=""\ninput.each_with_index { |label,i|\n out+=label\n out+=":"\n out+="#{i}"\n out+=";"\n}\nout',labels,"".join(f"{label}:{i};" for i,label in enumerate(labels)),returns="string",param="array<string>")
    add("block_yield","",500,500000,source="def twice(n: int, &block: int -> int) -> int\n yield(n)+yield(n+1)\nend\n"+function("i=0\ntotal=0\nwhile i<input\n total+=twice(i) { |x| x*2 }\n i+=1\nend\ntotal","int","int"))
    add("method_calls","",500,124750,source="class Counter\n @count: int = 0\n def add(n: int) -> int\n  @count+=n\n  @count\n end\nend\n"+function("c=Counter.new\ni=0\nwhile i<input\n c.add(i)\n i+=1\nend\nc.add(0)","int","int"))
    words=[("a" if i%2==0 else "b")+f"word{i}" for i in range(256)]
    add("member_calls",'total=0\ninput.each { |w|\n if w.start_with?("a") && !w.empty?\n  total+=w.length\n end\n}\ntotal',words,sum(len(w) for w in words if w.startswith("a")),returns="int",param="array<string>")
    add("string_build",'parts: array<string> = []\ni=0\nwhile i<input\n parts << "item-#{i}"\n i+=1\nend\nparts.join(",").length',500,len(",".join(f"item-{i}" for i in range(500))),returns="int",param="int")
    add("string_concat",'s=""\ni=0\nwhile i<input\n s+="ab"\n i+=1\nend\ns.length',500,1000,returns="int",param="int")
    # Records: field reads and writes, building and retaining shapes.
    records=[{"id":i,"name":f"r{i}","score":i*3,"active":i%2==0} for i in range(256)]
    add("record_fields",'total=0\ninput.each { |r|\n if r["active"]\n  total+=r["score"]\n end\n}\ntotal',records,sum(r["score"] for r in records if r["active"]),returns="int",param="array<{ id: int, name: string, score: int, active: bool }>")
    add("record_update",'acc={ count: 0, total: 0 }\ni=0\nwhile i<input\n acc["count"]+=1\n acc["total"]+=i\n i+=1\nend\nacc["total"]+acc["count"]',1000,499500+1000,returns="int",param="int")
    add("record_build",'rows: array<{ id: int, score: int, label: string }> = []\ni=0\nwhile i<input\n rows << { id: i, score: i*3, label: "r" }\n i+=1\nend\ntotal=0\nrows.each { |r| total+=r["score"] }\ntotal',1000,3*499500,returns="int",param="int")
    add("records_retained",'rows: array<{ id: int, name: string, active: bool }> = []\ni=0\nwhile i<input\n rows << { id: i, name: "row", active: i%2==0 }\n i+=1\nend\nrows',512,[{"id":i,"name":"row","active":i%2==0} for i in range(512)],returns="array<{ id: int, name: string, active: bool }>",param="int")
    # Glue: an API response parsed into records, transformed and serialized.
    orders=[{"id":i,"customer":{"name":f"customer-{i}","tier":"gold" if i%3==0 else "silver"},
             "items":[{"sku":f"sku-{i}-{j}","qty":j+1,"price":100+i+j} for j in range(3)]} for i in range(64)]
    raw=json.dumps(orders,separators=(",",":"))
    order_type='array<{ id: int, customer: { name: string, tier: string }, items: array<{ sku: string, qty: int, price: int }> }>'
    def glue(seq):
        return ('orders=JSON.parse_as(input, '+order_type+')\n'
                'out: array<{ id: int, name: string, total: int, seq: int }> = []\n'
                'n=0\n'
                'orders.each { |order|\n'
                ' total=0\n'
                ' order["items"].each { |item| total+=item["qty"]*item["price"] }\n'
                ' if order["customer"]["tier"] == "gold"\n'
                '  total=total*90//100\n'
                ' end\n'
                ' n+=1\n'
                ' out << { id: order["id"], name: order["customer"]["name"], total: total, seq: '+seq+' }\n'
                '}\n'
                'JSON.stringify(out)')
    def glue_total(order):
        total=sum(item["qty"]*item["price"] for item in order["items"])
        return total*90//100 if order["customer"]["tier"]=="gold" else total
    glued=json.dumps([{"id":o["id"],"name":o["customer"]["name"],"total":glue_total(o),"seq":i+1} for i,o in enumerate(orders)],separators=(",",":"))
    add("glue_orders",glue("n"),raw,glued,returns="string",param="string")
    add("glue_orders_cap",glue("host.next.as(int)"),raw,glued,returns="string",param="string")
    cases[-1]["capability_probe"]=True
    # The JSON and record workloads again with a host capability bound, as
    # glue services run them.
    for base in ["json_transform","json_object_512","hash_lookup","member_calls","record_fields","record_build","records_retained"]:
        case=next(case for case in cases if case["name"]==base)
        cases.append({**case,"name":base+"_cap","capability_probe":True})
    for name,path,function_name,arg,expected in [
        ("upstream_fibonacci","examples/control_flow/recursion.vibe","fibonacci",12,144),
        ("upstream_countdown","examples/control_flow/while_loop.vibe","countdown",50,list(range(50,0,-1))),
        ("upstream_greeting","examples/basics/functions_and_calls.vibe","decorated_greeting","Ada","[hello Ada]"),
    ]:
        cases.append(dict(name=name,source=(UPSTREAM/path).read_text(),function=function_name,args=[arg],expected=expected))
    for case in site_cases():
        filename=Path(case["name"]).stem
        if filename in ["top_rank_per_group","word_wrap","sieve_of_eratosthenes"]:
            cases.append({**case,"name":"site_"+filename})
    out=[]
    for case in cases:
        for accounting in [True,False]:
            out.append({**case,"name":case["name"]+("/metered" if accounting else "/unlimited"),"accounting":accounting,"iterations":100})
    return out


def text_benchmark_cases():
    """Validation, extraction and message building typical of service glue."""
    cases=[]

    def add(name, body, argument, expected, returns="string", param="string"):
        cases.append(dict(name="text/"+name, source=function(body, returns, param), args=[argument], expected=expected))

    emails=["ada@example.com", "first.last+tag@service.example", "bad address", "missing@", "@example.com"]*16
    add("regex_email", r'input.map { |s| s.match?(/\A[A-Za-z0-9._+%-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\z/) }', emails, [True,True,False,False,False]*16, "array<bool>", "array<string>")
    ids=["ID-12345678", "ID-00000000", "id-12345678", "ID-123", "ID-123456789"]*16
    add("regex_id", r'input.map { |s| s.match?(/\AID-[0-9]{8}\z/) }', ids, [True,True,False,False,False]*16, "array<bool>", "array<string>")
    fields="user=ada id=123 status=ready"
    add("regex_captures", r'm=input.match(/user=(?<user>[a-z]+) id=([0-9]+) status=([a-z]+)/); m == nil ? [] : m.captures', fields, ["ada","123","ready"], "array<string?>")
    for anchored,pattern in [("anchored", r"\AID-[0-9]+\z"),("unanchored", "ID-[0-9]+")]:
        for outcome,subject,expected in [("hit", "ID-12345678", True),("miss", "x"*16384, False)]:
            add(f"regex_{anchored}_{outcome}", f"input.match?(/{pattern}/)", subject, expected, "bool")
    add("regex_literal_miss", 'input.match?(/request-id/)', "x"*16384, False, "bool")
    add("regex_dynamic", 'input.match?("ID-[0-9]+")', "prefix ID-123 suffix", True, "bool")
    rows=" ".join(f"ID-{i}" for i in range(64))
    for method in ["sub", "gsub"]:
        expected=rows.replace("ID-", "id-", 1 if method=="sub" else -1)
        add(f"regex_{method}_string", f'input.{method}(/ID-([0-9]+)/, "id-\\\\1")', rows, expected)
        add(f"regex_{method}_block", f'input.{method}(/ID-[0-9]+/) {{ |s| s.downcase }}', rows, expected)
    add("regex_scan", 'input.scan(/ID-([0-9]+)/)', rows, [[str(i)] for i in range(64)], "array<string | array<string?>>")
    parts=[f"field-{i}" for i in range(64)]
    add("split", 'input.split(",")', ",".join(parts), parts, "array<string>")
    add("join", 'input.join(", ")', parts, ", ".join(parts), param="array<string>")
    add("strip", "input.strip", " \t"+"message "*1024+"\r\n", ("message "*1024).strip())
    ascii_text="aBcD9_! "*2048
    for method in ["upcase", "downcase"]:
        add(method, "input."+method, ascii_text, ascii_text.upper() if method=="upcase" else ascii_text.lower())
    for name,expression,expected,returns in [
        ("start_with", 'input.start_with?("request:")', True, "bool"),
        ("end_with", 'input.end_with?(":done")', True, "bool"),
        ("include", 'input.include?("missing")', False, "bool"),
        ("index", 'input.index(":done")', 8200, "int?"),
    ]:
        add(name, expression, "request:"+"x"*8192+":done", expected, returns)
    add("index_short_hit", 'input.index(":")', "request:ok", 7, "int?")
    add("index_short_miss", 'input.index("missing")', "request:ok", None, "int?")
    add("index_unicode", 'input.index(":done")', "é界🙂"*2048+":done", 6144, "int?")
    add("index_overlap", 'input.index("aaaaab")', "a"*8192+"b", 8187, "int?")
    repeated="request:"+"x"*8192+":done"+"x"*8192+":done"
    add("rindex", 'input.rindex(":done")', repeated, repeated.rindex(":done"), "int?")
    add("format", 'format("user=%s id=%08d ratio=%.2f", input, 42, 1.25)', "ada", "user=ada id=00000042 ratio=1.25")
    add("interpolation", '"user=#{input} id=#{42} ok=#{true} ratio=#{1.25} tags=#{[1, 2]}"', "ada", "user=ada id=42 ok=true ratio=1.25 tags=[1, 2]")
    add("concat_loop", 's=""; i=0; while i<input; s+="item-"; i+=1; end; s', 256, "item-"*256, param="int")
    add("length_unicode", "input.length", "é界🙂"*8192, 24576, "int")
    add("length_mixed", "input.length", "abé界🙂cd"*4096, 28672, "int")
    return [{**case, "name":case["name"]+("/metered" if accounting else "/unlimited"), "accounting":accounting, "iterations":100}
            for case in cases for accounting in [True,False]]


def host_global_cases():
    cases=[]

    def add(name, body, globals, expected, returns=None, source=None, args=None, difference=None, static_error=None):
        for strict in [False, True]:
            case=dict(name="host_globals/"+name+("/strict" if strict else "/ordinary"),
                source=source or function(body, returns), args=[None] if args is None else args,
                globals=globals, strict_effects=strict, expected=expected, accounting=True)
            if difference:
                case.update(go="host-global-binding-error", policy="consistent_bindings", reason=difference)
            if static_error:
                case["static_error"]=static_error
            cases.append(case)

    add("collection", 'settings["items"].push(2);settings', {"settings":{"items":[1]}}, {"items":[1,2]}, "{ items: array<int> }")
    add("nil", "helper", {"helper":None}, None,
        source="def helper -> int;99;end;"+function("helper", "int?"))
    add("function", "helper+1", {"helper":41}, 42,
        source="def helper -> int;99;end;"+function("helper+1", "int"))
    add("declaration", "Box", {"Box":[7]}, [7],
        source="class Box;end;"+function("Box", "array<int>"), static_error=None)
    add("enum", "State", {"State":7}, 7,
        source="enum State;Ready;end;"+function("State", "int"), static_error=None)
    add("builtin", "Math+1", {"Math":41}, 42, "int", static_error=None)
    add("parameter", "input", {"input":99}, 4, args=[4], source=function("input", "int", "int"))
    add("block_parameter", "[1].map{|helper|helper+1}", {"helper":99}, [2], "array<int>")
    add("block_write", 'begin;[1].each{count+=1};count;rescue;"host-global-binding-error";end', {"count":9}, 10, "int | string",
        difference="A block assignment retains an existing host binding instead of creating an uninitialized local.")
    add("overwrite", "settings=7;settings", {"settings":{"items":[1]}}, 7, "int")
    add("nested_address", "rows[-1]&.push(2);rows", {"rows":[[1]]}, [[1,2]], "array<array<int>>")
    add("rescued_call", "begin;helper(1);rescue;7;end", {"helper":None}, 7,
        source="def helper(x: int) -> int;99;end;"+function("begin;helper(1);rescue;7;end", "int"),
        static_error={"code": "V0310", "at": [2, 7]})
    for name in ["Parser", "Box", "Math"]:
        for index, expression in enumerate([
            f'{name}("3")', f'({name})("3")', f'{name}(*["3"])', f'{name} "3"',
        ]):
            # A constant can no longer hold a builtin function: indexing JSON by name is rejected.
            prefix=f'class Box;end;module M;{name}=JSON["parse"];'
            source=prefix+f'def self.apply -> any;{expression};end;end;'+function('begin;M.apply;rescue;"host-global-binding-error";end', "any")
            for supplied in [False, True]:
                difference="A module constant retains precedence over root declarations, builtins and host globals when called." if supplied or name != "Parser" else None
                add(f"module_constant/{name}/{index}/{supplied}", "", {name:None} if supplied else {}, 3, source=source, difference=difference,
                    static_error={"code": "V0102", "at": [1, 24]} if name in {"Box", "Math"} else
                        {"code": "V0112", "at": [1, len(prefix) - len('JSON["parse"];') + 1]})
    return cases


def conformance_cases(include_language=True):
    cases=[]

    def add(name,body,expected,arg=None,source=None,returns="any",param="any",static_error=None):
        cases.append(dict(name=name,source=source or function(body,returns,param),args=[arg],expected=expected,accounting=True))
        if static_error:
            cases[-1]["static_error"]=static_error

    # Conditions and logical operators take bool only (ADR-008), so this case is a static rejection.
    add("truthiness",'[nil || 7, false || 8, 0 && 9, "" && 10, false && (1/0)]',[7,8,9,10,False],returns="array<int | bool>",
        static_error={"code":"V0105","at":[2,2]})
    add("precedence","[2+3*4,-2**2,2**3**2]",[14,-4,512],returns="array<int>")
    add("float_arithmetic","[1.5+2,5.0/2,1e2+0.5]",[3.5,2.5,100.5],returns="array<float>")
    add("numeric_literals","[0xff,0b101,0o17,1_000]",[255,5,15,1000],returns="array<int>")
    add("if_elsif","x=2\nif x==1\n 1\nelsif x==2\n 2\nelse\n 3\nend",2,returns="int")
    add("early_return","if input\n return 42\nend\n0",42,True,returns="int",param="bool")
    add("loop_control","i=0\ns=0\nwhile i<10\n i+=1\n if i==3\n next\n end\n if i==8\n break\n end\n s+=i\nend\ns",25,returns="int")
    add("value_semantics","a=[1,2]\nb=a\na.push(3)\na[-1]=9\na << 4\n[a,b]",[[1,2,9,4],[1,2]],returns="array<array<int>>")
    add("index_assignment_result","a=[1]\na[0]=7",7,returns="int")
    add("negative_array_write","a=[1,2]\na[-1]=7\na",[1,7],returns="array<int>")
    add("array_self_push","a: array<any> = [1]\nb=a\na.push(a)\na[0]=9\n[a,b]",[[9,[1]],[1]],returns="array<array<any>>")
    add("array_self_append","a: array<any> = [1]\na << a\na",[1,[1]],returns="array<any>")
    add("array_self_assignment","a: array<any> = [1]\na[0]=a\na",[[1]],returns="array<any>")
    add("array_arguments_before_update","a=[1]\na.push(a.length,a.fetch(0))\na",[1,1,1],returns="array<int>")
    add("array_nested_alias","a=[1]\nb=[a]\na.push(2)\n[b,a]",[[[1]],[1,2]],returns="[array<array<int>>, array<int>]")
    add("array_result_alias","a=[1]\nb=a.push(2)\na[0]=9\n[a,b]",[[9,2],[1,2]],returns="array<array<int>>")
    add("array_add_assignment","a=[1]\nb=a\na=a+[a.length]\na+=[3]\n[a,b]",[[1,1,3],[1]],returns="array<array<int>>")
    add("array_add_self","a=[1]\na=a+a\na",[1,1],returns="array<int>")
    add("array_add_branch","a=[1]\nb=a\na=(false ? b : a)+[a.fetch(0)]\n[a,b]",[[1,1],[1]],returns="array<array<int>>")
    add("hash_order_and_alias",'h={b:1,a:2}\nx=h\nh["b"]=7\n[h.keys,h.values,x["b"]]',[["b","a"],[7,2],1],returns="[array<string>, array<int>, int]")
    add("hash_equality",'[{a:1,b:2}=={b:2,a:1},:a=="a",{a:1,a:2}["a"]]',[True,False,2],returns="[bool, bool, int]")
    for size in [15,16,17,24,25,511,512]:
        obj={f"k{i:05}":i for i in range(size)}
        add(f"hash_snapshot_{size}",'h=input\nold=h\nh["snapshot"]=h\nh["k00000"]=-1\n[h.length,old["k00000"],h.fetch("snapshot").as(hash<string, any>).length,h["k00000"],old["snapshot"]]',[size+1,0,size,-1,None],obj,
            returns="[int, any, int, any, any]",param="hash<string, any>")
        add(f"hash_byte_keys_{size}",'h=input\nh[""]="empty"\nh["é"]="unicode"\nh["\\xff"]="invalid"\nh["k00000"]=7\n[h[""],h["é"],h["\\xff"],h["k00000"],h.keys[-1]]',["empty","unicode","invalid",7,"\ufffd"],obj,
            returns="array<int | string | nil>",param="hash<string, int | string>")
    add("unicode_index",'[input.length,input.bytesize,input[1],input[-1],input.index("界"),input.rindex("é")]',[4,10,"é","🙂",2,1],"aé界🙂",
        returns="[int, int, string?, string?, int?, int?]",param="string")
    add("invalid_bytes",'s="a\\xff\\xfe"\n[s.length,s.bytesize]',[3,3],returns="array<int>")
    add("ascii_case_unicode",'input.upcase(:ascii)',"AéΣ🙂Z","aéΣ🙂z",returns="string",param="string")
    add("strip_unicode","input.strip","\u2003 \thello\n\u00a0","\u2003 \thello\n\u00a0",returns="string",param="string")
    add("split_join",'[input.split, "a,b,,".split(","), [1,2,3].join("-")] ',[["a","b"],["a","b"],"1-2-3"]," a  b ",
        returns="[array<string>, array<string>, string]",param="string")
    add("strict_integer",'" -42 ".to_i',-42,returns="int")
    add("json_unicode_escape","JSON.parse(input)",["🙂","�","a\n"],r'["\ud83d\ude42","\ud800","a\n"]',param="string")
    add("json_duplicates","JSON.parse(input)",{"a":2,"b":3},'{"a":1,"b":3,"a":2}',param="string")
    add("json_html",'JSON.stringify(input)',r'"\u003c\u003e\u0026\u2028\u2029"',"<>&\u2028\u2029",returns="string",param="string")
    for padding in [15,16,17,4093,4094,4095,4096,4097]:
        text="a"*padding+"é界🙂\u2028\u2029<>&"
        encoded=json.dumps(text+"\ufffd"*3,ensure_ascii=False,separators=(",",":"))
        for char,escape in [("<",r"\u003c"),(">",r"\u003e"),("&",r"\u0026"),("\u2028",r"\u2028"),("\u2029",r"\u2029"),("\ufffd",r"\ufffd")]:
            encoded=encoded.replace(char,escape)
        add(f"unicode_boundary_{padding}",'input=input+"\\xff\\xc0\\x80"\n[input.length,JSON.stringify(input)]',[len(text)+3,encoded],text,
            returns="[int, string]",param="string")
    rng=random.Random(1309)
    for i in range(30):
        a=rng.randrange(-10000,10000);b=rng.choice([n for n in range(-97,98) if n])
        add(f"arithmetic_{i}",f"[{a}+({b}),{a}-({b}),{a}*({b}),{a}//({b}),{a}%({b})]",[a+b,a-b,a*b,a//b,a%b],returns="array<int>")
    for case in json.loads((UPSTREAM.parent/"language.json").read_text()) if include_language else []:
        add("language/"+case["name"],case.get("body",""),case["expected"],source=case.get("source"))
        for field in ["entropy_byte","function","stdout","stderr","stdout_hex","stderr_hex"]:
            if field in case:
                cases[-1][field]=case[field]
        if case.get("function")=="__main__":
            cases[-1]["args"]=[]
    return cases+upstream_cases()+site_cases()+encoding_cases()+json_depth_cases()+[case for case in host_global_cases()+module_cases()+capability_cases()+block_cases()+signature_cases() if "policy" not in case]


if __name__ == "__main__":
    directory=Path(sys.argv[1]);directory.mkdir(parents=True,exist_ok=True)
    for name,cases in [("benchmarks",benchmark_cases()),("conformance",conformance_cases())]:
        cases=materialize(cases,directory/name)
        (directory/f"{name}.json").write_text(json.dumps(cases,ensure_ascii=False,sort_keys=True,indent=2)+"\n")
        print(f"{name}: {len(cases)} cases")
