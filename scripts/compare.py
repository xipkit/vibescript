#!/usr/bin/env python3
"""Build, validate, and measure compiled calls in the portable and SIMD Rust builds.

Validation checks every build against the golden corpora and the fixture
expectations. --baseline adds preserved builds of an earlier revision to
validation and timing, for before-and-after comparisons.
"""
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
from fixtures import benchmark_cases, conformance_cases, site_benchmark_cases
from module_fixtures import materialize
import golden

ROOT=Path(__file__).resolve().parent.parent
BINS=ROOT/"benchmarks/bin"
CARGO=ROOT/"scripts/cargo"
VARIANTS=["rust-portable","rust-simd"]
# The golden corpora each Rust build is validated against.
GOLDEN_CORPORA="conformance,language,rejections,compatibility"


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
        for name,features in [("golden-portable",[]),("golden-simd",["simd"])]:
            cmd=[CARGO,"build","--release","--locked","--example","golden","--no-default-features"]
            if features: cmd += ["--features",",".join(features)]
            run(cmd,cwd=ROOT,stdout=log,stderr=log)
            shutil.copy2(ROOT/"target/release/examples/golden",BINS/name)
    print("Built portable and SIMD timing, allocation-instrumented and golden binaries",flush=True)


def invoke(variant,fixtures,n,mode,path):
    with path.open("w") as output:
        run([BINS/variant,fixtures,n,mode],cwd=ROOT,stdout=output)
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
    # A case whose purpose is a static rejection does not run; golden.py checks it.
    cases=[case for case in conformance_cases()+benchmark_cases() if "static_error" not in case]
    expected=materialize(cases,out)
    path=out/"validation-inputs.json"
    path.write_text(json.dumps(expected,ensure_ascii=False,sort_keys=True)+"\n")
    reference={case["name"]:case["expected"] for case in expected}
    output_reference={case["name"]:case for case in expected}
    digests={}
    for variant in VARIANTS:
        records=invoke(variant,path,1,"validate",out/f"validation-{variant}.jsonl")
        assert len(records)==len(reference),(variant,len(records),len(reference))
        for record in records:
            result=json.loads(record["result_json"])
            assert equal_json(result,reference[record["name"]]),(variant,record["name"],repr(result)[:500],repr(reference[record["name"]])[:500])
            case=output_reference[record["name"]]
            for field in ["stdout_hex","stderr_hex"]:
                if case.get(field.removesuffix("_hex")):
                    assert record[field]==case[field],(variant,record["name"],field,record[field][:500],case[field][:500])
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
    for flavor in ["portable","simd"]:
        print(f"golden-{flavor}:",flush=True)
        assert golden.main(["--no-build","--harness",str(BINS/f"golden-{flavor}"),"--corpus",GOLDEN_CORPORA])==0,f"golden-{flavor} differs from the goldens"
    return digests[VARIANTS[0]]


def measure(out,rounds,target_ms,expected,suite):
    source=site_benchmark_cases() if suite=="site" else benchmark_cases()
    cases=[{k:v for k,v in case.items() if k!="expected"} for case in source]
    path=out/"measurement-inputs.json";path.write_text(json.dumps(cases,ensure_ascii=False,sort_keys=True)+"\n")
    pilot={};pilot_digests={}
    for variant in VARIANTS:
        records=invoke(variant,path,20,"timing",out/f"pilot-{variant}.jsonl")
        pilot[variant]={r["name"]:r["ns_per_call"] for r in records}
        pilot_digests[variant]={r["name"]:r["digest"] for r in records}
    if suite=="site":
        # Site programs are validated elsewhere; here every build must agree with the first.
        expected=pilot_digests[VARIANTS[0]]
        for variant in VARIANTS:
            for name,digest in pilot_digests[variant].items():
                assert digest==expected[name],(variant,name,"output differs from "+VARIANTS[0])
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
        records=invoke(variant+"-alloc",path,100,"alloc",out/f"allocations-{variant}.jsonl")
        allocations[variant]={r["name"]:{k:r[k] for k in ["alloc_bytes","allocations"]} for r in records}
        for record in records: assert record["digest"]==expected[record["name"]]
        rss_file=out/f"rss-{variant}.txt"
        with rss_file.open("w") as err,(out/f"rss-{variant}.jsonl").open("w") as output:
            run(["/usr/bin/time","-l",BINS/variant,path,100,"timing"],cwd=ROOT,stdout=output,stderr=err)
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


def cpu_name():
    if sys.platform=="darwin":
        return run(["sysctl","-n","machdep.cpu.brand_string"],capture_output=True).stdout.strip()
    try:
        for line in Path("/proc/cpuinfo").read_text().splitlines():
            if line.startswith("model name"):
                return line.split(":",1)[1].strip()
    except OSError:
        pass
    return platform.processor()


def main():
    parser=argparse.ArgumentParser();parser.add_argument("--out",type=Path,default=ROOT/"benchmarks/results"/time.strftime("%Y-%m-%d-%H%M%S"));parser.add_argument("--skip-build",action="store_true");parser.add_argument("--validate-only",action="store_true");parser.add_argument("--rounds",type=int,default=8);parser.add_argument("--target-ms",type=float,default=75);parser.add_argument("--suite",choices=["core","site"],default="core",help="core micro-benchmarks, or every site program")
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
    metadata={"platform":platform.platform(),"machine":platform.machine(),"cpu":cpu_name(),"rustc":run(["rustc","-Vv"],capture_output=True).stdout,"source_revision":run(["git","rev-parse","HEAD"],cwd=ROOT,capture_output=True).stdout.strip(),"dirty":run(["git","status","--porcelain"],cwd=ROOT,capture_output=True).stdout,"binary_sha256":{name:hashlib.sha256((BINS/name).read_bytes()).hexdigest() for name in VARIANTS},"RUSTFLAGS":os.environ.get("RUSTFLAGS",""),"target_ms":args.target_ms,"command":sys.argv}
    metadata["baseline_source_revision"]=baseline_revision
    metadata["allocation_binary_sha256"]={name+"-alloc":hashlib.sha256((BINS/(name+"-alloc")).read_bytes()).hexdigest() for name in VARIANTS}
    (out/"environment.json").write_text(json.dumps(metadata,indent=2)+"\n")
    expected=validate(out)
    if not args.validate_only: measure(out,args.rounds,args.target_ms,expected,args.suite)


if __name__=="__main__": main()
