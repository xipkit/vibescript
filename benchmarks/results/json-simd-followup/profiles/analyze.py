"""Attribute CPU samples exclusively to the innermost matching JSON cost category.

Usage: python3 analyze.py DIRECTORY ...
Each profile.json.gz needs its profile.json.pcs.json.gz from symbolicate.py.
"""
import gzip,json,sys
from pathlib import Path
from collections import Counter,defaultdict

def analyze(path):
 """Write exclusive category counts and top functions for one Samply profile."""
 p=json.loads(gzip.decompress(path.read_bytes()))
 pcs={(r['lib'],r['address']):r['frames'][0] for r in json.loads(gzip.decompress(path.with_suffix('.pcs.json.gz').read_bytes())) if r['frames']}
 counts=Counter();top=defaultdict(Counter);total=0;raw=Counter();samples=[]
 def classification(frames):
  for frame in frames:
   n=frame.get('function','');f=frame.get('file','')
   if frame.get('module')=='libsystem_malloc.dylib': return 'allocation',n or ('libsystem_malloc.dylib+'+frame['module_offset'])
   if any(x in n for x in ['malloc','calloc','realloc','_free','free_','free(', 'free_small','free_large','free_tiny','__rust_alloc','__rust_dealloc','alloc::alloc::','alloc::raw_vec::','malloc_zone','nanov2_']) or n=='free': return 'allocation',n
   if '/src/budget.rs' in f and ('CallContext' in n or 'Charge' in n or 'Memory' in n): return 'accounting',n
   if 'vibescript::budget::' in n and any(x in n for x in ['::charge','::work','::reserve','::check_memory','::allocated','::Charge','::Memory']): return 'accounting',n
   if any(x in n for x in ['unicode_span','vector_unicode','rune_width','scan::rune','utf8']): return 'utf8',n
   if 'vibescript::json::scan::' in n or n.endswith('scan::prefix') or '::vector_clean' in n: return 'structural_scanning',n
   if any(x in n for x in ['json::parse_float','::number','::digits','dec2flt','flt2dec','json::parser::integer','integer::parse_digits']): return 'number_decoding',n
   if 'json::typed::' in n or 'vibescript::types::' in n or '/src/types' in f: return 'type_validation',n
   if '::read_string' in n or '::hex' in n or 'scan::text_span' in n or 'budget::Buffer<u8>' in n: return 'string_decoding',n
   if 'vibescript::hash::' in n or 'vibescript::value::' in n or 'vibescript::budget::Buffer' in n or 'alloc::sync::Arc' in n: return 'value_construction_and_release',n
   if 'json::parser::' in n or 'json::document' in n: return 'parser_control',n
   if n.startswith('vibescript::') or '<vibescript::' in n: return 'other_runtime',n
  return 'other',(frames[0].get('function') or (frames[0].get('module','unknown')+'+'+frames[0].get('module_offset','0'))) if frames else 'unknown'
 for t in p['threads']:
  stack=t['stackTable'];ft=t['frameTable'];func=t['funcTable'];resources=t['resourceTable'];cache={}
  def trace(s):
   if s in cache:return cache[s]
   frames=[]
   while s is not None:
    i=stack['frame'][s];r=func['resource'][ft['func'][i]]
    lib=resources['lib'][r] if r is not None and r>=0 else None
    symbol=pcs.get((lib,ft['address'][i]),{})
    frames.extend(symbol.get('inlines',[]));frames.append(symbol)
    s=stack['prefix'][s]
   return frames
  for i,s in enumerate(t['samples']['stack']):
   if s is None:continue
   if s not in cache:
    frames=trace(s);cache[s]=(classification(frames),(frames[0].get('function') or (frames[0].get('module','unknown')+'+'+frames[0].get('module_offset','0'))) if frames else 'unknown')
   (category,name),leaf=cache[s]
   weight=t['samples']['weight'][i] if t['samples'].get('weight') else 1
   counts[category]+=weight;top[category][name]+=weight;raw[leaf]+=weight;total+=weight
 result=dict(samples=total,categories={k:dict(percent=100*v/total,samples=v,top_functions=[dict(function=n,percent=100*w/total,samples=w) for n,w in top[k].most_common(8)]) for k,v in counts.most_common()},top_leaf_functions=[dict(function=n,percent=100*w/total,samples=w) for n,w in raw.most_common(25)])
 path.with_suffix('.summary.json').write_text(json.dumps(result,indent=2)+'\n')
 print(path.parent.name,path.name,total,[(k,round(100*v/total,2)) for k,v in counts.most_common()]);print('Top leaf:',[(n,round(100*w/total,2)) for n,w in raw.most_common(12)])
for root in sys.argv[1:]:
 for path in Path(root).glob('*.json.gz'):
  if not path.name.endswith('.pcs.json.gz'):analyze(path)
