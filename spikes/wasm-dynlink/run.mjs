// A minimal dynamic loader: instantiate MAIN (owns memory), then instantiate SIDE against main's
// memory + table, supplying the bases and GOT globals a PIC module needs.
import fs from 'node:fs';

const mainBytes = fs.readFileSync(process.argv[2]);
const sideBytes = fs.readFileSync(process.argv[3]);

const main = await WebAssembly.instantiate(mainBytes, {});
const M = main.instance.exports;
const memory = M.memory;
const table = M.__indirect_function_table;
console.log('main exports:', Object.keys(M).filter(k => !k.startsWith('_RN')).join(', '));

// Carve the side module's data region out of the host heap so the two never overlap.
const heapBase = M.__heap_base ? M.__heap_base.value : 0;
const SIDE_DATA = 1 << 20;                 // 1 MB window for the side module's data
const memoryBase = (heapBase + 0xffff) & ~0xffff;
const tableBase = table ? table.length : 0;
if (table) table.grow(256);
// GROW the host memory to cover the side module's data window + its stack. The side module's data
// segments are placed at __memory_base, so that region must EXIST before instantiation.
const PAGE = 65536;
const needBytes = memoryBase + SIDE_DATA + (1 << 20);   // data window + 1 MB side stack
const havePages = memory.buffer.byteLength / PAGE;
const needPages = Math.ceil(needBytes / PAGE);
if (needPages > havePages) memory.grow(needPages - havePages);
console.log(`memory: ${havePages} -> ${memory.buffer.byteLength / PAGE} pages; side data at ${memoryBase}`);

// The side module's imports, discovered rather than guessed.
const mod = await WebAssembly.compile(sideBytes);
const wanted = WebAssembly.Module.imports(mod);
const env = {
  memory, __indirect_function_table: table,
  __stack_pointer: new WebAssembly.Global({value:'i32', mutable:true}, memoryBase + SIDE_DATA + (1 << 20)),
  __memory_base: new WebAssembly.Global({value:'i32'}, memoryBase),
  __table_base:  new WebAssembly.Global({value:'i32'}, tableBase),
  host_alloc: M.host_alloc, host_dealloc: M.host_dealloc,
  host_sum: M.host_sum, host_double: M.host_double,
};
const GOT = { mem: {}, func: {} };
let gotCount = 0;
for (const im of wanted) {
  if (im.module === 'GOT.mem' || im.module === 'GOT.func') {
    const bucket = im.module === 'GOT.mem' ? GOT.mem : GOT.func;
    // A real linker resolves these to the symbol's address. Zeroed here: this probe only proves
    // the MECHANISM, and the call under test touches none of them.
    bucket[im.name] = new WebAssembly.Global({value:'i32', mutable:true}, 0);
    gotCount++;
  } else if (im.module === 'env' && !(im.name in env)) {
    console.log('  UNSATISFIED env import:', im.name, im.kind);
  }
}
console.log(`GOT globals stubbed: ${gotCount}`);

const side = await WebAssembly.instantiate(mod, { env, 'GOT.mem': GOT.mem, 'GOT.func': GOT.func });
console.log('side instantiated OK');
if (side.exports.__wasm_apply_data_relocs) side.exports.__wasm_apply_data_relocs();

const n = 10;
const got = side.exports.side_roundtrip(n);
const want = n * (n + 1);   // 2 * sum(1..n)
console.log(`side_roundtrip(${n}) = ${got}, expected ${want} -> ${got === want ? 'PASS' : 'FAIL'}`);
process.exit(got === want ? 0 : 1);
