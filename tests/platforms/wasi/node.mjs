// Runs a WASI command under Node, which must not be relied on for confinement:
// node node.mjs [--dir HOST::GUEST]... [--] MODULE.wasm [ARGS]...
import { readFileSync } from 'node:fs';
import { WASI } from 'node:wasi';

const argv = process.argv.slice(2);
const preopens = {};
while (argv[0] === '--dir') {
  const [host, guest = host] = argv[1].split('::');
  preopens[guest] = host;
  argv.splice(0, 2);
}
if (argv[0] === '--') argv.shift();
const [binary, ...args] = argv;
if (!binary) throw new Error('expected a WASI module');
const env = process.env.TZ ? { TZ: process.env.TZ } : {};
const wasi = new WASI({ version: 'preview1', args: [binary, ...args], env, preopens, returnOnExit: true });
const module = await WebAssembly.compile(readFileSync(binary));
const instance = await WebAssembly.instantiate(module, wasi.getImportObject());
process.exitCode = wasi.start(instance);
