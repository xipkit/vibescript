from pathlib import Path
import json
import os
import shutil
import subprocess

from simd_embedding_probe import prepare


root = Path.cwd()
out = Path(os.environ['RUNNER_TEMP']) / 'embedding-results'
out.mkdir()
revisions = {
    'base': '7a3ba6469b50c5fa7b43764a70f6fc32d7fb3efa',
    'ascii': '892bc640542acba2d8db7ba93b4ec9b49cd29e76',
    'head': '3c1891543e6fc02cefb1548d0d04af3dde0d7d55',
}
env = dict(os.environ, GOTOOLCHAIN='go1.27.1', GOAMD64='v1', GOMAXPROCS='1')
for revision, sha in revisions.items():
    actual = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root / revision, text=True).strip()
    assert actual == sha, (revision, actual)
prepare(root / 'head', root / 'head/cmd/simd-embedding-probe')
for revision in ['base', 'ascii']:
    shutil.copytree(root / 'head/cmd/simd-embedding-probe', root / revision / 'cmd/simd-embedding-probe')
shutil.copytree(root / 'head/cmd/simd-embedding-probe', out / 'driver')
manifest = {'revisions': revisions, 'rounds': 6, 'benchtime': '100ms', 'affinity_cpu': min(os.sched_getaffinity(0)), 'binaries': []}
with (out / 'environment.txt').open('w') as log:
    for command in [['uname', '-a'], ['lscpu'], ['go', 'version']]:
        subprocess.run(command, env=env, stdout=log, check=True)
binaries = Path(os.environ['RUNNER_TEMP']) / 'embedding-binaries'
binaries.mkdir()
for revision in revisions:
    for mode in ['nosimd', 'simd']:
        binary = binaries / f'{revision}-{mode}'
        print('Building ordinary executable', binary.name, flush=True)
        subprocess.run(['go', 'build', '-o', str(binary), './cmd/simd-embedding-probe'], cwd=root / revision, env=dict(env, GOEXPERIMENT=mode), check=True)
        manifest['binaries'].append({'revision': revision, 'mode': mode, 'bytes': binary.stat().st_size})
        with (out / f'{revision}-{mode}.asm').open('w') as log:
            subprocess.run(['go', 'tool', 'objdump', '-s', '(stringMemberQuery.func2|stringRuneLen|stringRuneIndex|stringRuneSlice)', str(binary)], env=env, stdout=log, check=True)
expected = None
for trial in range(6):
    order = list(revisions) if trial % 2 == 0 else list(reversed(revisions))
    for mode in ['nosimd', 'simd', 'simd-avx2-disabled']:
        experiment = 'nosimd' if mode == 'nosimd' else 'simd'
        for revision in order:
            print('trial', trial, revision, mode, flush=True)
            command = ['taskset', '-c', str(manifest['affinity_cpu']), str(binaries / f'{revision}-{experiment}'), '-test.run=^$', '-test.bench=.', '-test.cpu=1', '-test.count=1', '-test.benchtime=100ms', '-test.benchmem']
            sample_env = dict(env, GOEXPERIMENT=experiment)
            if mode.endswith('avx2-disabled'):
                sample_env['GODEBUG'] = 'cpu.avx2=off'
            sample = subprocess.check_output(command, cwd=root / revision / 'internal/runtime', env=sample_env, text=True)
            names = [line.split()[0] for line in sample.splitlines() if line.startswith('Benchmark') and 'ns/op' in line]
            assert len(names) == 168 and len(set(names)) == 168, (revision, mode, len(names))
            if expected is None:
                expected = set(names)
            assert set(names) == expected
            with (out / f'{revision}-{mode}.txt').open('a') as log:
                log.write(sample)
            manifest.setdefault('samples', []).append({'trial': trial, 'revision': revision, 'mode': mode, 'cases': len(names)})
            (out / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
print('All 54 embedding samples passed with 168 matching public-call workloads.', flush=True)
