#!/usr/bin/env python3
"""Generate identical inputs and independently computed expected results."""
import json
import hashlib
import random
import sys
from pathlib import Path

UPSTREAM=Path(__file__).resolve().parent.parent/"tests/upstream"


def upstream_cases():
    manifest=json.loads((UPSTREAM/"sources.json").read_text())
    for entry in manifest["files"]:
        assert hashlib.sha256((UPSTREAM/entry["path"]).read_bytes()).hexdigest()==entry["sha256"],entry["path"]
    out=[]
    for i,case in enumerate(json.loads((UPSTREAM/"cases.json").read_text())):
        out.append({"name":f"upstream/{i:02}/{case['path']}::{case['function']}","source":(UPSTREAM/case["path"]).read_text(),"function":case["function"],"args":case["args"],"expected":case["expected"],"accounting":True})
    return out


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
    out=[]
    for case in cases:
        for accounting in [True,False]:
            out.append({**case,"name":case["name"]+("/metered" if accounting else "/unlimited"),"accounting":accounting,"iterations":100})
    return out


def conformance_cases():
    cases=[]

    def add(name,body,expected,arg=None):
        cases.append(dict(name=name,source=function(body),args=[arg],expected=expected,accounting=True))

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
    add("hash_order_and_alias","h={b:1,a:2}\nx=h\nh[:b]=7\n[h.keys,h.values,x[:b]]",[["b","a"],[7,2],1])
    add("hash_equality",'[{a:1,b:2}=={b:2,a:1},:a=="a",{a:1,a:2}[:a]]',[True,False,2])
    add("unicode_index",'[input.length,input.bytesize,input[1],input[-1],input.index("界"),input.rindex("é")]',[4,10,"é","🙂",2,1],"aé界🙂")
    add("invalid_bytes",'s="a\\xff\\xfe"\n[s.length,s.bytesize]',[3,3])
    add("ascii_case_unicode",'input.upcase(:ascii)',"AéΣ🙂Z","aéΣ🙂z")
    add("strip_unicode","input.strip","\u2003 \thello\n\u00a0","\u2003 \thello\n\u00a0")
    add("split_join",'[input.split, "a,b,,".split(","), [1,2,3].join("-")] ',[["a","b"],["a","b"],"1-2-3"]," a  b ")
    add("strict_integer",'" -42 ".to_i',-42)
    add("json_unicode_escape","JSON.parse(input)",["🙂","�","a\n"],r'["\ud83d\ude42","\ud800","a\n"]')
    add("json_duplicates","JSON.parse(input)",{"a":2,"b":3},'{"a":1,"b":3,"a":2}')
    add("json_html",'JSON.stringify(input)',r'"\u003c\u003e\u0026\u2028\u2029"',"<>&\u2028\u2029")
    rng=random.Random(1309)
    for i in range(30):
        a=rng.randrange(-10000,10000);b=rng.choice([n for n in range(-97,98) if n])
        add(f"arithmetic_{i}",f"[{a}+({b}),{a}-({b}),{a}*({b}),{a}/({b}),{a}%({b})]",[a+b,a-b,a*b,a//b,a%b])
    return cases+upstream_cases()


if __name__ == "__main__":
    directory=Path(sys.argv[1]);directory.mkdir(parents=True,exist_ok=True)
    for name,cases in [("benchmarks",benchmark_cases()),("conformance",conformance_cases())]:
        (directory/f"{name}.json").write_text(json.dumps(cases,ensure_ascii=False,sort_keys=True,indent=2)+"\n")
        print(f"{name}: {len(cases)} cases")
