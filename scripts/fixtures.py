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


def encoding_cases():
    return [{"name":"encoding/"+case["name"],"source":function(case["body"]),"args":[None],"expected":case["expected"],"accounting":True,"result_encoding":"typed"} for case in json.loads((UPSTREAM.parent/"encoding-cases.json").read_text())]


def function(body):
    return "def run(input)\n" + body + "\nend"


def benchmark_cases():
    cases = []

    def add(name, body, argument, expected, source=None):
        cases.append(dict(name=name, source=source or function(body), args=[argument], expected=expected))

    add("numeric_loop", "i=0\ntotal=0\nwhile i<input\n total+=i\n i+=1\nend\ntotal", 1000, 499500)
    add("function_calls", "", 500, 125250, source="def add(a,b)\n a+b\nend\n"+function("i=0\ntotal=0\nwhile i<input\n total=add(total,i+1)\n i+=1\nend\ntotal"))
    add("array_sum", "input.sum", list(range(1000)), 499500)
    add("array_growth", "a=[]\ni=0\nwhile i<input\n a.push(i)\n i+=1\nend\na.sum", 128, 8128)
    add("hash_lookup", "i=0\ntotal=0\nwhile i<1000\n total+=input[\"k31\"]\n i+=1\nend\ntotal", {f"k{i:02}": i for i in range(64)}, 31000)
    for size in [8, 512, 2048]:
        obj={f"k{i:05}":i for i in range(size)}
        key=f"k{size-1:05}"
        add(f"hash_lookup_{size}",f'i=0\ntotal=0\nwhile i<128\n total+=input["{key}"]\n i+=1\nend\ntotal',obj,128*(size-1))
        add(f"json_object_{size}","JSON.parse(input)",json.dumps(obj,separators=(",",":")),obj)
    keys=[f"k{i:05}" for i in range(512)]
    obj=dict(zip(keys,range(512)))
    add("hash_build_512",'h={}\ni=0\nwhile i<input.length\n h[input[i]]=i\n i+=1\nend\nh["k00511"]',keys,511)
    add("hash_replace_512",'h=input\ni=0\nwhile i<128\n h["k00511"]=i\n i+=1\nend\nh["k00511"]',obj,127)
    add("hash_equal_512","input[0]==input[1]",[obj,dict(reversed(list(obj.items())))],True)
    duplicates="{"+",".join(json.dumps(k)+":"+str(v) for k,v in [*obj.items(),*((k,1024+i) for i,k in enumerate(reversed(keys)))])+"}"
    expected={k:1024+511-i for i,k in enumerate(keys)}
    add("json_duplicates_512","JSON.parse(input)",duplicates,expected)
    for size in [16, 4096, 65536]:
        text=("aBcD9_! "*((size+7)//8))[:size]
        add(f"length_{size}", "input.length", text, len(text))
        add(f"upcase_{size}", "input.upcase(:ascii)", text, text.upper())
    text="a"*65536
    add("strip_65536", "input.strip", "  "+text+"  ", text)
    unicode_text="é界🙂"*4096
    add("length_unicode", "input.length", unicode_text, len(unicode_text))
    add("length_mixed_65536", "input.length", text[:-1]+"é", len(text))
    for name,text in [("ascii_64k",text),("escaped_4k",'a\n\t"\\'*820),("unicode_4k","é界🙂"*456)]:
        obj={"id":7,"payload":text}
        raw=json.dumps(obj,ensure_ascii=False,separators=(",",":"),sort_keys=True)
        add("json_parse_"+name,"JSON.parse(input)",raw,obj)
        add("json_stringify_"+name,"JSON.stringify(input)",obj,raw)
    rows=[{"id":i,"name":f"record-{i}","score":i*3,"active":i%2==0} for i in range(64)]
    raw=json.dumps(rows,ensure_ascii=False,separators=(",",":"),sort_keys=True)
    expected=json.dumps({"total":sum(row["score"] for row in rows),"count":len(rows)},separators=(",",":"))
    add("json_transform",'rows=JSON.parse(input)\ni=0\ntotal=0\nwhile i<rows.length\n total+=rows[i]["score"]\n i+=1\nend\nJSON.stringify({total:total,count:rows.length})',raw,expected)
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


def host_global_cases():
    cases=[]

    def add(name, body, globals, expected, source=None, args=None, difference=None):
        for strict in [False, True]:
            case=dict(name="host_globals/"+name+("/strict" if strict else "/ordinary"),
                source=source or function(body), args=[None] if args is None else args,
                globals=globals, strict_effects=strict, expected=expected, accounting=True)
            if difference:
                case.update(go="host-global-binding-error", policy="consistent_bindings", reason=difference)
            cases.append(case)

    add("collection", "settings.items.push(2);settings", {"settings":{"items":[1]}}, {"items":[1,2]})
    add("nil", "helper", {"helper":None}, None,
        source="def helper;99;end;"+function("helper"))
    add("function", "helper+1", {"helper":41}, 42,
        source="def helper;99;end;"+function("helper+1"))
    add("declaration", "Box", {"Box":[7]}, [7],
        source="class Box;end;"+function("Box"))
    add("enum", "State", {"State":7}, 7,
        source="enum State;Ready;end;"+function("State"))
    add("builtin", "Math+1", {"Math":41}, 42)
    add("parameter", "input", {"input":99}, 4, args=[4])
    add("block_parameter", "[1].map{|helper|helper+1}", {"helper":99}, [2])
    add("block_write", 'begin;[1].each{count+=1};count;rescue;"host-global-binding-error";end', {"count":9}, 10,
        difference="A block assignment retains an existing host binding instead of creating an uninitialized local.")
    add("overwrite", "settings=7;settings", {"settings":{"items":[1]}}, 7)
    add("nested_address", "rows[-1].push(2);rows", {"rows":[[1]]}, [[1,2]])
    add("rescued_call", "begin;helper(1);rescue;7;end", {"helper":None}, 7,
        source="def helper(x);99;end;"+function("begin;helper(1);rescue;7;end"))
    for name in ["Parser", "Box", "Math"]:
        for index, expression in enumerate([
            f'{name}("3")', f'({name})("3")', f'{name}(*["3"])', f'{name} "3"',
        ]):
            source=f'class Box;end;module M;{name}=JSON[:parse];def self.apply;{expression};end;end;'+function('begin;M.apply;rescue;"host-global-binding-error";end')
            for supplied in [False, True]:
                difference="A module constant retains precedence over root declarations, builtins and host globals when called." if supplied or name != "Parser" else None
                add(f"module_constant/{name}/{index}/{supplied}", "", {name:None} if supplied else {}, 3, source=source, difference=difference)
    return cases


def conformance_cases():
    cases=[]

    def add(name,body,expected,arg=None,source=None):
        cases.append(dict(name=name,source=source or function(body),args=[arg],expected=expected,accounting=True))

    add("truthiness",'[nil || 7, false || 8, 0 && 9, "" && 10, false && (1/0)]',[7,8,9,10,False])
    add("precedence","[2+3*4,-2**2,2**3**2]",[14,-4,512])
    add("float_arithmetic","[1.5+2,5.0/2,1e2+0.5]",[3.5,2.5,100.5])
    add("numeric_literals","[0xff,0b101,0o17,1_000]",[255,5,15,1000])
    add("if_elsif","x=2\nif x==1\n 1\nelsif x==2\n 2\nelse\n 3\nend",2)
    add("early_return","if input\n return 42\nend\n0",42,True)
    add("loop_control","i=0\ns=0\nwhile i<10\n i+=1\n if i==3\n next\n end\n if i==8\n break\n end\n s+=i\nend\ns",25)
    add("value_semantics","a=[1,2]\nb=a\na.push(3)\na[-1]=9\na << 4\n[a,b]",[[1,2,9,4],[1,2]])
    add("index_assignment_result","a=[1]\na[0]=7",7)
    add("negative_array_write","a=[1,2]\na[-1]=7\na",[1,7])
    add("array_self_push","a=[1]\nb=a\na.push(a)\na[0]=9\n[a,b]",[[9,[1]],[1]])
    add("array_self_append","a=[1]\na << a\na",[1,[1]])
    add("array_self_assignment","a=[1]\na[0]=a\na",[[1]])
    add("array_arguments_before_update","a=[1]\na.push(a.length,a[0])\na",[1,1,1])
    add("array_nested_alias","a=[1]\nb=[a]\na.push(2)\n[b,a]",[[[1]],[1,2]])
    add("array_result_alias","a=[1]\nb=a.push(2)\na[0]=9\n[a,b]",[[9,2],[1,2]])
    add("array_add_assignment","a=[1]\nb=a\na=a+[a.length]\na+=[3]\n[a,b]",[[1,1,3],[1]])
    add("array_add_self","a=[1]\na=a+a\na",[1,1])
    add("array_add_branch","a=[1]\nb=a\na=(false || a)+[a[0]]\n[a,b]",[[1,1],[1]])
    add("hash_order_and_alias","h={b:1,a:2}\nx=h\nh[:b]=7\n[h.keys,h.values,x[:b]]",[["b","a"],[7,2],1])
    add("hash_equality",'[{a:1,b:2}=={b:2,a:1},:a=="a",{a:1,a:2}[:a]]',[True,False,2])
    for size in [15,16,17,24,25,511,512]:
        obj={f"k{i:05}":i for i in range(size)}
        add(f"hash_snapshot_{size}",'h=input\nold=h\nh["snapshot"]=h\nh[:k00000]=-1\n[h.length,old[:k00000],h["snapshot"].length,h[:k00000],old["snapshot"]]',[size+1,0,size,-1,None],obj)
        add(f"hash_byte_keys_{size}",'h=input\nh[""]="empty"\nh["é"]="unicode"\nh["\\xff"]="invalid"\nh[:k00000]=7\n[h[""],h["é"],h["\\xff"],h[:k00000],h.keys[-1]]',["empty","unicode","invalid",7,"\ufffd"],obj)
    add("unicode_index",'[input.length,input.bytesize,input[1],input[-1],input.index("界"),input.rindex("é")]',[4,10,"é","🙂",2,1],"aé界🙂")
    add("invalid_bytes",'s="a\\xff\\xfe"\n[s.length,s.bytesize]',[3,3])
    add("ascii_case_unicode",'input.upcase(:ascii)',"AéΣ🙂Z","aéΣ🙂z")
    add("strip_unicode","input.strip","\u2003 \thello\n\u00a0","\u2003 \thello\n\u00a0")
    add("split_join",'[input.split, "a,b,,".split(","), [1,2,3].join("-")] ',[["a","b"],["a","b"],"1-2-3"]," a  b ")
    add("strict_integer",'" -42 ".to_i',-42)
    add("json_unicode_escape","JSON.parse(input)",["🙂","�","a\n"],r'["\ud83d\ude42","\ud800","a\n"]')
    add("json_duplicates","JSON.parse(input)",{"a":2,"b":3},'{"a":1,"b":3,"a":2}')
    add("json_html",'JSON.stringify(input)',r'"\u003c\u003e\u0026\u2028\u2029"',"<>&\u2028\u2029")
    for padding in [15,16,17,4093,4094,4095,4096,4097]:
        text="a"*padding+"é界🙂\u2028\u2029<>&"
        encoded=json.dumps(text+"\ufffd"*3,ensure_ascii=False,separators=(",",":"))
        for char,escape in [("<",r"\u003c"),(">",r"\u003e"),("&",r"\u0026"),("\u2028",r"\u2028"),("\u2029",r"\u2029"),("\ufffd",r"\ufffd")]:
            encoded=encoded.replace(char,escape)
        add(f"unicode_boundary_{padding}",'input=input+"\\xff\\xc0\\x80"\n[input.length,JSON.stringify(input)]',[len(text)+3,encoded],text)
    rng=random.Random(1309)
    for i in range(30):
        a=rng.randrange(-10000,10000);b=rng.choice([n for n in range(-97,98) if n])
        add(f"arithmetic_{i}",f"[{a}+({b}),{a}-({b}),{a}*({b}),{a}/({b}),{a}%({b})]",[a+b,a-b,a*b,a//b,a%b])
    for case in json.loads((UPSTREAM.parent/"language.json").read_text()):
        add("language/"+case["name"],case.get("body",""),case["expected"],source=case.get("source"))
        for field in ["entropy_byte","function","stdout","stderr","stdout_hex","stderr_hex"]:
            if field in case:
                cases[-1][field]=case[field]
        if case.get("function")=="__main__":
            cases[-1]["args"]=[]
    return cases+upstream_cases()+site_cases()+encoding_cases()+[case for case in host_global_cases()+module_cases()+capability_cases()+block_cases() if "policy" not in case]


if __name__ == "__main__":
    directory=Path(sys.argv[1]);directory.mkdir(parents=True,exist_ok=True)
    for name,cases in [("benchmarks",benchmark_cases()),("conformance",conformance_cases())]:
        cases=materialize(cases,directory/name)
        (directory/f"{name}.json").write_text(json.dumps(cases,ensure_ascii=False,sort_keys=True,indent=2)+"\n")
        print(f"{name}: {len(cases)} cases")
