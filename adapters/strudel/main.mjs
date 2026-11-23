// Patch ⇄ Strudel: a pattern engine sidecar. One JSON object per line on
// stdin, one reply per line on stdout (stderr is a log). Patch owns time:
// this process never schedules or plays anything, it answers "which events
// start in [begin, end)?" for definitions Patch loaded into numbered slots.
//
//   {"id":1,"op":"hello"}
//   {"id":2,"op":"load","slot":3,"definition":{"kind":"source","language":"strudel","code":"…"}}
//   {"id":3,"op":"load","slot":4,"definition":{"kind":"program","program":{…}}}
//   {"id":4,"op":"query","slot":3,"begin":"1/2","end":"3/4"}
//   {"id":5,"op":"unload","slot":3}
//
// Patch starts it under Node's permission model (no files beyond its own
// directory, no processes, no network): live code is JavaScript, so the
// sandbox is the boundary. MIT, part of Patch; Strudel is AGPL-3.0-or-later.

import './quiet.mjs';
import * as core from '@strudel/core';
import * as mini from '@strudel/mini';
import { transpiler } from '@strudel/transpiler';
import { createInterface } from 'node:readline';
import { makeScope } from './scope.mjs';
import { makeTranslator } from './ast.mjs';
import { translator } from './events.mjs';

const say = (o) => process.stdout.write(JSON.stringify(o) + '\n');
// Strudel logs to the console; keep stdout for the protocol.
console.log = (...a) => process.stderr.write(a.join(' ') + '\n');
console.info = console.log;

const patchScope = makeScope(core);
await core.evalScope(core, mini, patchScope);
const VERSION = JSON.parse(
  await (await import('node:fs/promises')).readFile(new URL('./node_modules/@strudel/core/package.json', import.meta.url)),
).version;

const slots = new Map();

function capabilities() {
  return {
    engine: 'strudel',
    version: VERSION,
    languages: ['strudel'],
    program: true,
    euclidean: true,
    polymeter: true,
    probability: true,
    // Strudel's randomness is a function of time: repeatable, but not
    // Patch's seeds.
    deterministic: true,
    source_locations: true,
    sandboxed: process.permission?.has?.('child') === false,
    out_of_process: true,
    unsupported: ['choose.seed', 'degrade.seed', 'sometimes.seed', 'every.offset'],
  };
}

// Strudel source → layers. `name: expr` lines become named layers (the
// transpiler turns labels into .p('name')); an unlabelled program is one
// layer, "d1".
async function evalSource(code) {
  const layers = [];
  const P = core.Pattern.prototype;
  const savedP = P.p;
  P.p = function (name) {
    if (typeof name === 'string' && (name.startsWith('_') || name.endsWith('_'))) return this;
    const n = name === '$' || name === undefined ? `d${layers.length + 1}` : String(name).replace(/\$/g, '');
    layers.push({ name: n, target: null, pattern: this });
    return this;
  };
  try {
    const { pattern } = await core.evaluate(code, transpiler);
    if (!layers.length && pattern?._Pattern) layers.push({ name: 'd1', target: null, pattern });
  } finally {
    P.p = savedP;
  }
  if (!layers.length) throw new Error('nothing to play: the code returned no pattern');
  return layers;
}

function lineCol(code, offset) {
  const before = code.slice(0, offset).split('\n');
  return { line: before.length, col: before[before.length - 1].length + 1 };
}

// Strudel drops the position of a mini-notation error; find it again by
// parsing each double-quoted (mini-notation) string of the source.
function locateMini(code) {
  for (const m of code.matchAll(/"((?:\\.|[^"\\])*)"/g)) {
    try {
      mini.parse(`"${m[1]}"`);
    } catch (e) {
      if (e.location) return lineCol(code, m.index + e.location.start.offset);
    }
  }
  return null;
}

function locate(err, code) {
  const message = String(err?.message ?? err).replace(/\s*\(\d+:\d+\)$/, '');
  // acorn (JavaScript syntax) errors carry their position.
  if (err?.loc) return { line: err.loc.line, col: err.loc.column + 1, message };
  if (code && message.startsWith('[mini]')) {
    const at = locateMini(code);
    if (at) return { ...at, message: message.replace(/^\[mini\] parse error at line \d+: /, 'mini-notation: ') };
  }
  return { line: 0, col: 0, message };
}

async function load({ slot, definition }) {
  const t0 = performance.now();
  const notes = new Set();
  let layers;
  try {
    if (definition.kind === 'program') {
      layers = makeTranslator(core, notes)(definition.program).map((l, i) => ({ ...l }));
    } else if (definition.kind === 'source' && definition.language === 'strudel') {
      layers = await evalSource(definition.code);
    } else {
      return { ok: false, errors: [{ line: 0, col: 0, message: `Strudel can't run '${definition.language ?? definition.kind}'` }] };
    }
    // Every layer must answer a query before it may replace what plays.
    for (const l of layers) l.pattern.queryArc(0, 1);
  } catch (e) {
    return { ok: false, errors: [locate(e, definition.code)] };
  }
  // Layer numbers follow the definition (muted program layers keep theirs).
  const indexed = definition.kind === 'program'
    ? definition.program.layers.map((l, i) => [l, i]).filter(([l]) => !l.muted).map(([, i], k) => ({ ...layers[k], index: i }))
    : layers.map((l, i) => ({ ...l, index: i }));
  const stats = { unmapped: {} };
  slots.set(slot, { layers: indexed, toEvent: translator(stats), stats });
  return {
    ok: true,
    layers: indexed.map((l) => ({ name: l.name, target: l.target ? JSON.stringify(l.target) : 'strudel' })),
    eval_us: Math.round((performance.now() - t0) * 1000),
    notes: [...notes],
  };
}

function query({ slot, begin, end }) {
  const s = slots.get(slot);
  if (!s) return { ok: false, error: 'nothing loaded' };
  const [b, e] = [core.Fraction(begin), core.Fraction(end)];
  const events = [];
  for (const l of s.layers) {
    for (const h of l.pattern.queryArc(b, e)) {
      if (!h.whole || !h.hasOnset()) continue;
      const ev = s.toEvent(h, l.index);
      if (ev) events.push(ev);
    }
  }
  events.sort((x, y) => core.Fraction(x.whole.begin).compare(core.Fraction(y.whole.begin)) || x.layer - y.layer);
  const unmapped = Object.keys(s.stats.unmapped).length ? s.stats.unmapped : undefined;
  return { ok: true, events, unmapped };
}

const rl = createInterface({ input: process.stdin });
for await (const line of rl) {
  if (!line.trim()) continue;
  let req;
  try {
    req = JSON.parse(line);
  } catch {
    say({ ok: false, error: 'not JSON' });
    continue;
  }
  let reply;
  try {
    switch (req.op) {
      case 'hello':
        reply = { ok: true, capabilities: capabilities() };
        break;
      case 'load':
        reply = await load(req);
        break;
      case 'query':
        reply = query(req);
        break;
      case 'unload':
        slots.delete(req.slot);
        reply = { ok: true };
        break;
      default:
        reply = { ok: false, error: `no op '${req.op}'` };
    }
  } catch (e) {
    reply = { ok: false, error: String(e?.message ?? e) };
  }
  say({ id: req.id, ...reply });
}
