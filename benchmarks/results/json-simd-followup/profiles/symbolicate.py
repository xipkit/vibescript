"""Resolve each sampled PC and its inline frames through a local Samply server.

Usage: python3 symbolicate.py PROFILE.json.gz SERVER.log
Load a copy without a .syms.json sidecar to retain DWARF inline attribution.
"""
import gzip,json,re,sys,urllib.parse,urllib.request
from pathlib import Path
profile=Path(sys.argv[1]); log=Path(sys.argv[2])
x=json.loads(gzip.decompress(profile.read_bytes()))
server=urllib.parse.unquote(re.search(r'symbolServer=(\S+)',log.read_text()).group(1))
libs=x['libs']; memory=[[l['debugName'],l['breakpadId']] for l in libs]
pcs=set()
for t in x['threads']:
 for i,addr in enumerate(t['frameTable']['address']):
  f=t['frameTable']['func'][i]; resource=t['funcTable']['resource'][f]
  if resource is not None and resource>=0 and addr>=0:
   lib=t['resourceTable']['lib'][resource]
   if lib is not None:pcs.add((lib,addr))
pcs=sorted(pcs);output=[]
for start in range(0,len(pcs),500):
 batch=pcs[start:start+500]
 data=json.dumps({'jobs':[{'memoryMap':memory,'stacks':[[[lib,addr]] for lib,addr in batch]}]}).encode()
 request=urllib.request.Request(server+'/symbolicate/v5',data=data,headers={'Content-Type':'application/json'})
 r=json.load(urllib.request.urlopen(request))
 for (lib,addr),frames in zip(batch,r['results'][0]['stacks']):
  output.append(dict(lib=lib,address=addr,frames=frames))
profile.with_suffix('.pcs.json.gz').write_bytes(gzip.compress((json.dumps(output)+'\n').encode(),mtime=0))
print(len(pcs),'addresses symbolicated')
