#!/usr/bin/env python3
"""Rewrites indexing that Adamantium's indexing_slicing rule reports into
Checked accessors (src/checked.rs): `x[i]` to `x.at(i)?`, `&mut x[i]` to
`x.at_mut(i)?`, `x[a..]` to `x.tail(a)?` and `x[a..b]` to `x.span(a, b)?`,
dropping a Buffer's `.data`. It is a first pass for the VIBE-134 follow-up
cards: it cannot tell `at` from `at_mut`, add `?` only where the function
returns Result, or restructure Option-returning code, so compile, read the
errors and finish by hand. The closing card removes this script.

    sed -i '' 's/"indexing_slicing", //' Cargo.toml
    ./scripts/adamantium --format json -- --all-features > findings.json
    git checkout Cargo.toml
    python3 scripts/indexing_slicing.py findings.json src/typing/calls.rs ...
    ./scripts/cargo fmt

A finding's column marks the start of the indexed expression; nested
indexing that starts at the same column is reported once per level and
rewritten innermost first.
"""
import json, sys, re

def find_open(s, i):
    """From offset i (start of EXPR[IDX]), return offset of the '[' that indexes EXPR."""
    depth = 0
    j = i
    while j < len(s):
        c = s[j]
        if c in '([{':
            if c == '[' and depth == 0:
                return j
            depth += 1
        elif c in ')]}':
            depth -= 1
            if depth < 0:
                return None
        j += 1
    return None

def find_close(s, i):
    """i is at '['; return offset of matching ']'."""
    depth = 0
    j = i
    while j < len(s):
        c = s[j]
        if c in '([{':
            depth += 1
        elif c in ')]}':
            depth -= 1
            if depth == 0:
                return j
        j += 1
    return None

def split_range(idx):
    depth = 0
    for k in range(len(idx) - 1):
        c = idx[k]
        if c in '([{': depth += 1
        elif c in ')]}': depth -= 1
        elif depth == 0 and idx[k:k+2] == '..' and idx[k:k+3] != '..=':
            return idx[:k].strip(), idx[k+2:].strip()
    return None

def convert(path, findings):
    s = open(path).read()
    lines = s.split('\n')
    starts = [0]
    for l in lines:
        starts.append(starts[-1] + len(l) + 1)
    from collections import Counter
    counts = Counter((f['line'], f['column']) for f in findings)
    sites = [site for site in sorted(counts, reverse=True) for _ in range(counts[site])]
    done = 0
    skipped = []
    for line, col in sites:
        i = starts[line - 1] + col - 1
        o = find_open(s, i)
        if o is None:
            skipped.append((line, col, 'no [')); continue
        c = find_close(s, o)
        if c is None:
            skipped.append((line, col, 'no ]')); continue
        expr = s[i:o].rstrip()
        idx = s[o+1:c].strip()
        # Drop the Buffer's `.data` where the expression is a Buffer field.
        if expr.endswith('.data'):
            expr = expr[:-5]
        before = s[:i]
        after = s[c+1:]
        # Is the whole index expression (or its field) the target of an assignment?
        rest_line = after.split('\n', 1)[0]
        m_assign = re.match(r'(\s*(\.\w+)*\s*)(=(?!=)|\+=|-=|\|=|&=)', rest_line)
        pre = before.rstrip()
        rng = split_range(idx)
        if rng is not None:
            lo, hi = rng
            if hi == '':
                call = f'{expr}.tail({lo or "0"})?'
            else:
                call = f'{expr}.span({lo or "0"}, {hi})?'
            chained = after.lstrip().startswith('.') and not after.lstrip().startswith('..')
            if pre.endswith('&mut'):
                if not chained: before = pre[:-4]
                call = f'{expr}.tail_mut({lo or "0"})?' if hi == '' else call
            elif pre.endswith('&'):
                if not chained: before = pre[:-1]
            s = before + call + after
            done += 1
            continue
        chained = after.lstrip().startswith('.') and not after.lstrip().startswith('..')
        by_value = (pre.endswith(' match') or pre.endswith('(match')) or (
            re.search(r'\blet\s+[^=;]+=$', pre) is not None and after.lstrip().startswith(';'))
        if pre.endswith('&mut'):
            if chained:
                call = f'{expr}.at_mut({idx})?'
            else:
                before = pre[:-4]
                call = f'{expr}.at_mut({idx})?'
        elif pre.endswith('&'):
            if chained:
                call = f'{expr}.at({idx})?'
            else:
                before = pre[:-1]
                call = f'{expr}.at({idx})?'
        elif by_value and not chained:
            call = f'*{expr}.at({idx})?'
        elif m_assign:
            if m_assign.group(1).strip() == '':
                call = f'*{expr}.at_mut({idx})?'
            else:
                call = f'{expr}.at_mut({idx})?'
        else:
            call = f'{expr}.at({idx})?'
        s = before + call + after
        done += 1
    open(path, 'w').write(s)
    print(f'{path}: {done} rewritten, {len(skipped)} skipped {skipped}')

findings = json.load(open(sys.argv[1]))
if isinstance(findings, dict):
    findings = findings['findings']
findings = [f for f in findings if f.get('rule', '').endswith('indexing_slicing')]
for path in sys.argv[2:]:
    convert(path, [f for f in findings if f['file'] == path])
