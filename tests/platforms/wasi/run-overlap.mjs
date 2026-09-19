import { readFileSync } from 'node:fs';
import { WASI } from 'node:wasi';

const [binary, root, other] = process.argv.slice(2);
if (!binary || !root || !other) throw new Error('expected wasm binary and two fixture directories');
const wasi = new WASI({
  version: 'preview1',
  args: ['witness', '--overlap'],
  preopens: { '/sandbox': root, '/sandbox/real': other },
});
const module = await WebAssembly.compile(readFileSync(binary));
const instance = await WebAssembly.instantiate(module, wasi.getImportObject());
process.exitCode = wasi.start(instance);
