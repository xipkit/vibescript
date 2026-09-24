#!/usr/bin/env python3
"""Run `vibes check` over every available program and fail on unfinished analysis.

The sweep covers the script corpus, the code blocks in the documentation and,
with --replay, the sources recorded from the Go reference's test suite. Each
program is checked without step or memory quotas under a wall-clock deadline;
a check that reports incomplete analysis or reaches the deadline fails the
sweep. Programs that fail to compile are counted but are not failures, since
many documentation blocks are fragments.
"""
import argparse
import collections
import glob
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT=Path(__file__).resolve().parent.parent
REFERENCE=ROOT/".cache/reference-go-0.70.0"
BLOCK=re.compile(r"```(?:vibe|vibescript|ruby)\n(.*?)```",re.S)
STATS=re.compile(r"steps=(\d+)")


def corpus():
    """Yields (label, source) for every in-tree program and documentation block."""
    roots=[ROOT/"tests/site",ROOT/"tests/upstream",ROOT/"examples",REFERENCE/"examples"]
    for root in roots:
        for path in sorted(root.rglob("*.vibe")):
            yield str(path.relative_to(ROOT)),path.read_text(errors="replace")
    docs=sorted(ROOT.glob("docs/*.md"))+sorted(ROOT.glob("tools/src/lsp/reference/*.md"))
    docs+=sorted(REFERENCE.glob("docs/**/*.md"))
    for path in docs:
        for index,block in enumerate(BLOCK.findall(path.read_text(errors="replace"))):
            yield f"{path.relative_to(ROOT)}#{index}",block


def replay(directory):
    """Yields (label, source) for each recorded program in replay fixture chunks."""
    for chunk in sorted(glob.glob(os.path.join(directory,"chunk-*.json"))):
        for record in json.load(open(chunk)):
            if record.get("source"):
                yield f"replay:{record.get('name')}",record["source"]


def check(binary,path,timeout_ms):
    command=[binary,"check","--steps","0","--memory","0","--timeout-ms",str(timeout_ms),"--stats",path]
    try:
        result=subprocess.run(command,capture_output=True,text=True,timeout=timeout_ms/1000+30)
    except subprocess.TimeoutExpired:
        return "limit",0,[]
    output=result.stdout+result.stderr
    match=STATS.search(result.stderr)
    steps=int(match.group(1)) if match else 0
    if "compile failed" in output:
        return "compile",steps,[]
    incomplete=[line for line in result.stdout.splitlines() if ": incomplete: " in line]
    if incomplete:
        return "incomplete",steps,incomplete
    if "deadline exceeded" in output:
        return "limit",steps,[]
    if "No issues found" in result.stdout:
        return "clean",steps,[]
    return "errors",steps,[]


def main():
    parser=argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--bin",default=str(ROOT/"target/release/vibes"),help="vibes binary (default: target/release/vibes)")
    parser.add_argument("--replay",help="directory of recorded replay fixture chunks")
    parser.add_argument("--timeout-ms",type=int,default=10_000,help="deadline per program")
    parser.add_argument("--jobs",type=int,default=os.cpu_count() or 4)
    parser.add_argument("--top",type=int,default=10,help="list this many of the most expensive checks")
    args=parser.parse_args()
    programs={}
    sources=list(corpus())+(list(replay(args.replay)) if args.replay else [])
    for label,source in sources:
        programs.setdefault(hashlib.sha256(source.encode()).hexdigest()[:16],(label,source))
    with tempfile.TemporaryDirectory() as scratch:
        def one(item):
            digest,(label,source)=item
            path=os.path.join(scratch,f"{digest}.vibe")
            Path(path).write_text(source)
            return (label,*check(args.bin,path,args.timeout_ms))
        with ThreadPoolExecutor(args.jobs) as pool:
            results=list(pool.map(one,programs.items()))
    counts=collections.Counter(result[1] for result in results)
    print(f"{len(results)} programs: "+", ".join(f"{kind} {counts[kind]}" for kind in ["clean","errors","compile","incomplete","limit"]))
    for label,_,steps,_ in sorted(results,key=lambda result:-result[2])[:args.top]:
        print(f"  {steps:>13,} steps  {label}")
    failures=[result for result in results if result[1] in ("incomplete","limit")]
    for label,kind,_,lines in failures:
        print(f"{kind}: {label}")
        for line in lines[:3]:
            print("   ",line.split(".vibe:",1)[-1])
    return 1 if failures else 0


if __name__=="__main__":
    sys.exit(main())
