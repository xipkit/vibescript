import { readFileSync } from 'node:fs';
import { WASI } from 'node:wasi';

const [binary, fixture, guest = '/sandbox', ...args] = process.argv.slice(2);
if (!binary || !fixture) throw new Error('expected wasm binary and fixture directory');
const wasi = new WASI({
  version: 'preview1',
  args: ['witness', ...(args.length ? args : [`${guest}/allowed`])],
  preopens: { [guest]: fixture },
});
const module = await WebAssembly.compile(readFileSync(binary));
const instance = await WebAssembly.instantiate(module, wasi.getImportObject());
process.exitCode = wasi.start(instance);
