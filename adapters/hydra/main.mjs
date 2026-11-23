// Patch ⇄ Hydra: hydra-synth as a *shader compiler*. One JSON object per
// line on stdin, one reply per line on stdout. Patch owns rendering, time,
// feedback and compositing; this process only turns a Patch visual scene
// (nodes of named operations with constant or signal arguments) into one
// fragment shader and says which uniform carries which Patch signal.
//
//   {"id":1,"op":"hello"}
//   {"id":2,"op":"compile","scene":{"nodes":[…],"output":3}}
//
// A scene node: {"op":"osc","args":[{"const":10},{"signal":"s0"}],"inputs":[]}.
// Signals are passed to Hydra as functions; Hydra makes each a uniform
// (`<arg><n>`) and wraps the function, so the mapping is recovered by
// calling every uniform once and noting which signal answers. No eval:
// the chain is built by calling Hydra's generators.
//
// Started under Node's permission model (its own directory only, no
// processes, no network). MIT, part of Patch; hydra-synth is AGPL-3.0.

import { createInterface } from 'node:readline';
import { readFile } from 'node:fs/promises';
import GeneratorFactory from './node_modules/hydra-synth/src/generator-factory.js';

const say = (o) => process.stdout.write(JSON.stringify(o) + '\n');
console.log = (...a) => process.stderr.write(a.join(' ') + '\n');
console.warn = console.log;

const VERSION = JSON.parse(
  await readFile(new URL('./node_modules/hydra-synth/package.json', import.meta.url)),
).version;

const factory = new GeneratorFactory({ defaultUniforms: {}, defaultOutput: { precision: 'highp' } });
const gen = factory.generators;
const kinds = factory.glslTransforms;

function capabilities() {
  return {
    engine: 'hydra',
    version: VERSION,
    ops: Object.fromEntries(
      Object.entries(kinds).map(([name, t]) => [name, { kind: t.type, args: t.inputs.map((i) => i.name) }]),
    ),
    feedback: true,
    blur: false,
    sandboxed: process.permission?.has?.('child') === false,
  };
}

let probe = null;

function arg(a, at) {
  if (a && typeof a === 'object' && 'signal' in a) {
    const id = String(a.signal);
    // Called by the probe below, and never at render time (Patch renders).
    return () => {
      probe = id;
      return 0;
    };
  }
  if (a && typeof a === 'object' && 'const' in a) return Number(a.const);
  if (typeof a === 'number') return a;
  throw new Error(`${at}: an argument is {"const":n} or {"signal":id}`);
}

function build(scene) {
  const nodes = scene.nodes || [];
  const seen = new Set();
  const make = (i) => {
    const n = nodes[i];
    if (!n) throw new Error(`no node ${i}`);
    if (seen.has(i)) throw new Error(`node ${i} is its own input`);
    seen.add(i);
    const t = kinds[n.op];
    if (!t) throw new Error(`hydra has no '${n.op}'`);
    const args = (n.args || []).map((a, j) => arg(a, `${n.op} arg ${j}`));
    const inputs = n.inputs || [];
    let out;
    if (t.type === 'src') {
      if (inputs.length) throw new Error(`${n.op} is a source: no inputs`);
      out = gen[n.op](...args);
    } else {
      if (inputs.length < 1) throw new Error(`${n.op} needs the chain it transforms`);
      const base = make(inputs[0]);
      if (t.type === 'combine' || t.type === 'combineCoord') {
        if (inputs.length !== 2) throw new Error(`${n.op} combines two chains`);
        out = base[n.op](make(inputs[1]), ...args);
      } else {
        out = base[n.op](...args);
      }
    }
    seen.delete(i);
    return out;
  };
  return make(scene.output ?? nodes.length - 1);
}

function compile(scene) {
  const t0 = process.hrtime.bigint();
  const chain = build(scene);
  const [pass] = chain.glsl();
  const uniforms = [];
  for (const [name, value] of Object.entries(pass.uniforms)) {
    if (typeof value !== 'function') continue;
    probe = null;
    value(null, { time: 0, bpm: 120 });
    if (probe === null) throw new Error(`uniform ${name} isn't a Patch signal`);
    uniforms.push({ name, signal: probe });
  }
  const us = Number(process.hrtime.bigint() - t0) / 1000;
  return { frag: pass.frag, uniforms, compile_us: us };
}

const rl = createInterface({ input: process.stdin });
for await (const line of rl) {
  let req;
  try {
    req = JSON.parse(line);
  } catch (e) {
    say({ id: null, error: `bad JSON: ${e.message}` });
    continue;
  }
  try {
    switch (req.op) {
      case 'hello':
        say({ id: req.id, ok: capabilities() });
        break;
      case 'compile':
        say({ id: req.id, ok: compile(req.scene || {}) });
        break;
      default:
        say({ id: req.id, error: `no op '${req.op}'` });
    }
  } catch (e) {
    say({ id: req.id, error: String(e.message || e) });
  }
}
