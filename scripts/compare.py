#!/usr/bin/env python3
"""Build, validate, and measure the same compiled calls in Go and Rust."""
import argparse
import hashlib
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
import time
from pathlib import Path
from fixtures import benchmark_cases, conformance_cases

ROOT=Path(__file__).resolve().parent.parent
BINS=ROOT/"benchmarks/bin"
GO=ROOT/"scripts/go"
CARGO=ROOT/"scripts/cargo"
ENV={**os.environ,"GOMAXPROCS":"1"}
VARIANTS=["go-portable","go-simd","rust-portable","rust-simd"]


def run(cmd,**kwargs):
    return subprocess.run([str(x) for x in cmd],check=True,text=True,**kwargs)


def build(out):
    BINS.mkdir(parents=True,exist_ok=True)
    with (out/"build.log").open("w") as log:
        for name,features in [("rust-portable",[]),("rust-simd",["simd"]),("rust-portable-alloc",["allocation-stats"]),("rust-simd-alloc",["simd","allocation-stats"])]:
            cmd=[CARGO,"build","--release","--locked","--example","compare","--no-default-features"]
            if features: cmd += ["--features",",".join(features)]
            run(cmd,cwd=ROOT,stdout=log,stderr=log)
            shutil.copy2(ROOT/"target/release/examples/compare",BINS/name)
        for name,experiment in [("go-portable",""),("go-simd","simd")]:
            run([GO,"build","-o",BINS/name,"."],cwd=ROOT/"benchmarks/go",env={**ENV,"GOEXPERIMENT":experiment},stdout=log,stderr=log)
    print("Built four timing binaries and two allocation-instrumented Rust binaries",flush=True)


def invoke(variant,fixtures,n,mode,path):
    with path.open("w") as output:
        run([BINS/variant,fixtures,n,mode],cwd=ROOT,env=ENV,stdout=output)
    return [json.loads(line) for line in path.read_text().splitlines()]


def equal_json(actual,expected):
    if isinstance(actual,bool) or isinstance(expected,bool):
        return type(actual) is type(expected) and actual==expected
    if isinstance(actual,list) and isinstance(expected,list):
        return len(actual)==len(expected) and all(equal_json(a,b) for a,b in zip(actual,expected))
    if isinstance(actual,dict) and isinstance(expected,dict):
        return actual.keys()==expected.keys() and all(equal_json(actual[k],expected[k]) for k in actual)
    return actual==expected


def validate(out):
    expected=conformance_cases()+benchmark_cases()
    path=out/"validation-inputs.json"
    path.write_text(json.dumps(expected,ensure_ascii=False,sort_keys=True)+"\n")
    reference={case["name"]:case["expected"] for case in expected}
    digests={}
    for variant in VARIANTS:
        records=invoke(variant,path,1,"validate",out/f"validation-{variant}.jsonl")
        assert len(records)==len(reference),(variant,len(records),len(reference))
        for record in records:
            result=json.loads(record["result_json"])
            assert equal_json(result,reference[record["name"]]),(variant,record["name"],repr(result)[:500],repr(reference[record["name"]])[:500])
        digests[variant]={record["name"]:record["digest"] for record in records}
        print(f"{variant}: {len(records)} shared cases match expected results",flush=True)
    for name in [c["name"] for c in benchmark_cases()]:
        assert len({digests[v][name] for v in VARIANTS})==1,(name,"output digest differs")
    # Both Rust implementations must charge identical work and allocation capacity.
    rust=[]
    for variant in ["rust-portable","rust-simd"]:
        records=[json.loads(line) for line in (out/f"validation-{variant}.jsonl").read_text().splitlines()]
        rust.append({r["name"]:tuple(r[k] for k in ["steps","tracked_peak_bytes","tracked_retained_bytes"]) for r in records})
    assert rust[0]==rust[1],"Rust accounting differs between portable and SIMD"
    validate_rejections(out)
    return digests["go-portable"]


def validate_rejections(out):
    cases=[]
    for filename,phase in [("language-errors.json","runtime"),("syntax-errors.json","syntax")]:
        cases += [{**case,"phase":phase} for case in json.loads((ROOT/"tests"/filename).read_text())]
    records=[]
    for variant in VARIANTS:
        for case in cases:
            fixture={"name":case["name"],"source":case.get("source") or "def run(input)\n"+case["body"]+"\nend","args":[None],"accounting":True}
            path=out/"rejection-input.json"
            path.write_text(json.dumps([fixture])+"\n")
            proc=subprocess.run([str(BINS/variant),str(path),"1","validate"],cwd=ROOT,env=ENV,capture_output=True,text=True,errors="replace",timeout=10)
            assert proc.returncode==1,(variant,case["name"],proc.returncode,proc.stdout,proc.stderr)
            if variant.startswith("go-"):
                assert case["go_error"] in proc.stderr,(variant,case["name"],proc.stderr)
            else:
                assert "Error { kind:" in proc.stderr,(variant,case["name"],proc.stderr)
                assert ("kind: Syntax" in proc.stderr)==(case["phase"]=="syntax"),(variant,case["name"],proc.stderr)
            records.append({"variant":variant,"name":case["name"],"phase":case["phase"],"stderr":proc.stderr})
        counts={phase:sum(c["phase"]==phase for c in cases) for phase in ["runtime","syntax"]}
        print(f"{variant}: {counts['runtime']} runtime errors and {counts['syntax']} syntax errors rejected",flush=True)
    (out/"validation-rejections.json").write_text(json.dumps(records,indent=2)+"\n")


def measure(out,rounds,target_ms,expected):
    cases=[{k:v for k,v in case.items() if k!="expected"} for case in benchmark_cases()]
    path=out/"measurement-inputs.json";path.write_text(json.dumps(cases,ensure_ascii=False,sort_keys=True)+"\n")
    pilot={}
    for variant in VARIANTS:
        pilot[variant]={r["name"]:r["ns_per_call"] for r in invoke(variant,path,20,"timing",out/f"pilot-{variant}.jsonl")}
    for case in cases:
        slowest=max(pilot[v][case["name"]] for v in VARIANTS)
        case["iterations"]=max(20,min(100000,int(target_ms*1_000_000/slowest)))
    path.write_text(json.dumps(cases,ensure_ascii=False,sort_keys=True)+"\n")
    samples={variant:{case["name"]:[] for case in cases} for variant in VARIANTS}
    order=[]
    for round_index in range(rounds):
        width=len(VARIANTS)
        base=VARIANTS if (round_index//width)%2==0 else list(reversed(VARIANTS))
        variants=base[round_index%width:]+base[:round_index%width]
        order.append(variants)
        for variant in variants:
            records=invoke(variant,path,0,"timing",out/f"round-{round_index:02}-{variant}.jsonl")
            for record in records:
                assert record["digest"]==expected[record["name"]],(variant,record["name"],"result changed")
                samples[variant][record["name"]].append(record["ns_per_call"])
        print(f"Measured round {round_index+1}/{rounds}",flush=True)
    allocations={};rss={}
    for variant in VARIANTS:
        binary=variant+"-alloc" if variant.startswith("rust") else variant
        records=invoke(binary,path,100,"alloc",out/f"allocations-{variant}.jsonl")
        allocations[variant]={r["name"]:{k:r[k] for k in ["alloc_bytes","allocations"]} for r in records}
        for record in records: assert record["digest"]==expected[record["name"]]
        rss_file=out/f"rss-{variant}.txt"
        with rss_file.open("w") as err,(out/f"rss-{variant}.jsonl").open("w") as output:
            run(["/usr/bin/time","-l",BINS/variant,path,100,"timing"],cwd=ROOT,env=ENV,stdout=output,stderr=err)
        line=next(line for line in rss_file.read_text().splitlines() if "maximum resident set size" in line)
        rss[variant]=int(line.split()[0])
    summary={"rounds":rounds,"order":order,"peak_rss_bytes":rss,"cases":{}}
    for case in cases:
        name=case["name"];summary["cases"][name]={}
        for variant in VARIANTS:
            times=samples[variant][name]
            summary["cases"][name][variant]={"median_ns":statistics.median(times),"min_ns":min(times),"max_ns":max(times),"samples_ns":times,**allocations[variant][name]}
    (out/"summary.json").write_text(json.dumps(summary,indent=2)+"\n")
    print(f"Timing, allocations, and process RSS saved to {out}",flush=True)


def main():
    parser=argparse.ArgumentParser();parser.add_argument("--out",type=Path,default=ROOT/"benchmarks/results"/time.strftime("%Y-%m-%d-%H%M%S"));parser.add_argument("--skip-build",action="store_true");parser.add_argument("--validate-only",action="store_true");parser.add_argument("--rounds",type=int,default=8);parser.add_argument("--target-ms",type=float,default=75)
    parser.add_argument("--baseline",type=Path,help="Directory containing prior rust-portable/rust-simd timing and allocation binaries, plus a revision file")
    args=parser.parse_args();out=args.out.resolve();out.mkdir(parents=True,exist_ok=False)
    baseline_revision=None
    if args.baseline:
        baseline=args.baseline.resolve()
        baseline_revision=(baseline/"revision").read_text().strip()
        if len(baseline_revision)!=40 or any(c not in "0123456789abcdef" for c in baseline_revision):
            parser.error("baseline revision must contain a full commit hash")
        BINS.mkdir(parents=True,exist_ok=True)
        for flavor in ["portable","simd"]:
            name=f"rust-before-{flavor}"
            for suffix in ["","-alloc"]:
                shutil.copy2(baseline/f"rust-{flavor}{suffix}",BINS/f"{name}{suffix}")
            VARIANTS.append(name)
    if args.target_ms<=0 or (not args.validate_only and (args.rounds<len(VARIANTS) or args.rounds%len(VARIANTS))):
        parser.error("use a positive target time and a round count that is a positive multiple of the variant count")
    if not args.skip_build: build(out)
    metadata={"platform":platform.platform(),"machine":platform.machine(),"cpu":run(["sysctl","-n","machdep.cpu.brand_string"],capture_output=True).stdout.strip(),"rustc":run(["rustc","-Vv"],capture_output=True).stdout,"go":run([GO,"version"],capture_output=True).stdout,"go_module":json.loads(run([GO,"list","-m","-json","github.com/mgomes/vibescript"],cwd=ROOT/"benchmarks/go",capture_output=True).stdout),"source_revision":run(["git","rev-parse","HEAD"],cwd=ROOT,capture_output=True).stdout.strip(),"dirty":run(["git","status","--porcelain"],cwd=ROOT,capture_output=True).stdout,"binary_sha256":{name:hashlib.sha256((BINS/name).read_bytes()).hexdigest() for name in VARIANTS},"RUSTFLAGS":os.environ.get("RUSTFLAGS",""),"GOFLAGS":os.environ.get("GOFLAGS",""),"GOMAXPROCS":1,"target_ms":args.target_ms,"command":sys.argv}
    metadata["baseline_source_revision"]=baseline_revision
    metadata["allocation_binary_sha256"]={name+"-alloc":hashlib.sha256((BINS/(name+"-alloc")).read_bytes()).hexdigest() for name in VARIANTS if name.startswith("rust")}
    (out/"environment.json").write_text(json.dumps(metadata,indent=2)+"\n")
    expected=validate(out)
    if not args.validate_only: measure(out,args.rounds,args.target_ms,expected)


if __name__=="__main__": main()
