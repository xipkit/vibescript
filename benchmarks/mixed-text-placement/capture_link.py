#!/usr/bin/env python3
"""Capture persistent linker inputs for the mixed-text placement experiment."""
import json, os, pathlib, shutil, subprocess, sys
root = pathlib.Path(os.environ['VIBE_LINK_CAPTURE'])
root.mkdir(parents=True, exist_ok=True)
args = sys.argv[1:]
saved = []
for i, arg in enumerate(args):
    path = pathlib.Path(arg)
    if path.is_file() and path.suffix in ('.o', '.a', '.rlib'):
        dest = root / f'{i}-{path.name}'
        # Toolchain archives can be read-only after an earlier capture.
        dest.unlink(missing_ok=True)
        shutil.copy2(path, dest)
        saved.append(str(dest))
    else:
        saved.append(arg)
(root / 'link.json').write_text(json.dumps(saved, indent=2) + '\n')
raise SystemExit(subprocess.call(['cc', *args]))
