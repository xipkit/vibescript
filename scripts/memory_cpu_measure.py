from pathlib import Path
import hashlib
import json
import os
import shutil
import subprocess
import sys

from memory_embedding_probe import prepare


root = Path.cwd()
mode = sys.argv[1]
out = Path(os.environ['RUNNER_TEMP']) / f'memory-{mode}-results'
out.mkdir()
binaries = Path(os.environ['RUNNER_TEMP']) / f'memory-{mode}-binaries'
binaries.mkdir()
env = dict(os.environ, GOTOOLCHAIN='go1.27.1', GOAMD64='v1', GOMAXPROCS='1')
cpu = min(os.sched_getaffinity(0))
revisions = ['base', 'head'] if mode != 'embedding' else ['base', 'head', 'merged-base', 'merged-head']
manifest = {
    'mode': mode,
    'affinity_cpu': cpu,
    'revisions': {name: subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root / name, text=True).strip() for name in revisions},
    'samples': [],
    'binaries': [],
}
with (out / 'environment.txt').open('w') as log:
    for command in [['uname', '-a'], ['lscpu'], ['go', 'version']]:
        subprocess.run(command, env=env, stdout=log, check=True)

if mode in ['embedding', 'profile']:
    prepare(root / 'head', root / 'head/cmd/simd-embedding-probe')
    for name in revisions:
        if name != 'head':
            shutil.copytree(root / 'head/cmd/simd-embedding-probe', root / name / 'cmd/simd-embedding-probe')
    shutil.copytree(root / 'head/cmd/simd-embedding-probe', out / 'driver')
else:
    subprocess.run(['python3', str(root / 'head/scripts/simd_profiles.py'), 'prepare', '--head', str(root / 'head'), '--base', str(root / 'base'), '--output', str(out / 'profiles.json')], check=True)

def build(name, experiment, seed=None):
    binary = binaries / f'{name}-{experiment}-{seed if seed is not None else "embedding"}'
    command = ['go', 'test', '-c', './internal/runtime', '-o', str(binary)] if mode == 'layout' else ['go', 'build', '-o', str(binary), './cmd/simd-embedding-probe']
    if seed:
        command.insert(2, f'-ldflags=-randlayout={seed}')
    print('Building', binary.name, flush=True)
    subprocess.run(command, cwd=root / name, env=dict(env, GOEXPERIMENT=experiment), check=True)
    manifest['binaries'].append({'name': binary.name, 'bytes': binary.stat().st_size, 'sha256': hashlib.sha256(binary.read_bytes()).hexdigest()})
    return binary

def measure(name, experiment, binary, trial, pattern, cases, duration, fallback=False):
    variant = experiment + ('-avx2-disabled' if fallback else '')
    options = dict(env, GOEXPERIMENT=experiment)
    if fallback:
        options['GODEBUG'] = 'cpu.avx2=off'
    command = ['taskset', '-c', str(cpu), str(binary), '-test.run=^$', '-test.bench=' + pattern, '-test.cpu=1', '-test.count=1', '-test.benchtime=' + duration, '-test.benchmem']
    sample = subprocess.check_output(command, cwd=root / name / 'internal/runtime', env=options, text=True)
    names = [line.split()[0] for line in sample.splitlines() if line.startswith('Benchmark') and 'ns/op' in line]
    assert len(names) == len(set(names)) == cases, names
    tag = f'-seed-{trial}' if mode == 'layout' else ''
    with (out / f'{name}-{variant}{tag}.txt').open('a') as log:
        log.write(sample)
    manifest['samples'].append({'trial': trial, 'revision': name, 'variant': variant, 'cases': cases, 'names': names})
    (out / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
    print('Measured', name, variant, trial, flush=True)

if mode == 'profile':
    for name in revisions:
        binary = build(name, 'simd')
        profile = out / f'{name}-length.prof'
        subprocess.run(['taskset', '-c', str(cpu), str(binary), '-test.run=^$', '-test.bench=^BenchmarkSIMDStringLengthLoopUnicode$', '-test.benchtime=3s'], cwd=root / name / 'internal/runtime', env=dict(env, GOEXPERIMENT='simd', MEMORY_CPU_PROFILE=str(profile)), check=True)
        assert profile.stat().st_size > 0
        with (out / f'{name}-length-top.txt').open('w') as log:
            subprocess.run(['go', 'tool', 'pprof', '-top', '-nodecount=20', str(binary), str(profile)], env=env, stdout=log, check=True)
    (out / 'manifest.json').write_text(json.dumps(manifest, indent=2) + '\n')
elif mode == 'layout':
    for seed in range(13):
        for experiment in ['nosimd', 'simd']:
            for name in revisions if seed % 2 == 0 else list(reversed(revisions)):
                binary = build(name, experiment, seed)
                measure(name, experiment, binary, seed, '^BenchmarkSIMDString(Length|Index|RIndex|Slice)Loop(ASCII|Unicode)$', 8, '150ms')
                if seed == 0:
                    with (out / f'{name}-{experiment}.asm').open('w') as log:
                        subprocess.run(['go', 'tool', 'objdump', '-s', '(stringMemberQuery.func2|stringRuneLen|stringRuneIndex|stringRuneSlice)', str(binary)], env=env, stdout=log, check=True)
                binary.unlink()
else:
    built = {(name, experiment): build(name, experiment) for name in revisions for experiment in ['nosimd', 'simd']}
    for trial in range(6):
        for experiment, fallback in [('nosimd', False), ('simd', False), ('simd', True)]:
            for name in revisions if trial % 2 == 0 else list(reversed(revisions)):
                measure(name, experiment, built[name, experiment], trial, '^Benchmark(SIMDString.*|JSONSpans(MixedDocument)?)$', 52, '100ms', fallback)
    for name in ['base', 'head']:
        binary = built[name, 'simd']
        profile = out / f'{name}-length.prof'
        subprocess.run(['taskset', '-c', str(cpu), str(binary), '-test.run=^$', '-test.bench=^BenchmarkSIMDStringLengthLoopUnicode$', '-test.benchtime=3s'], cwd=root / name / 'internal/runtime', env=dict(env, GOEXPERIMENT='simd', MEMORY_CPU_PROFILE=str(profile)), check=True)
        with (out / f'{name}-length-top.txt').open('w') as log:
            subprocess.run(['go', 'tool', 'pprof', '-top', '-nodecount=20', str(binary), str(profile)], env=env, stdout=log, check=True)
print('Diagnostic complete:', mode, flush=True)
