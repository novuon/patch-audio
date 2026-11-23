// A Patch pattern definition (patch_pattern::program::Program as JSON) as
// Strudel patterns, built by calling Strudel's functions: no source text,
// no eval. Nodes Strudel can't express exactly are reported, not faked.
// MIT, part of Patch.

export function makeTranslator(core, notes) {
  const S = core;
  const F = (t) => {
    // Patch times are exact text ("1/3", "2").
    const s = String(t);
    return s.includes('/') ? S.Fraction(s) : Number(s);
  };
  const value = (v) => v;
  const SIGNALS = { sine: S.sine, cosine: S.cosine, saw: S.saw, isaw: S.isaw, tri: S.tri, square: S.square, rand: S.rand };

  function pat(p) {
    if (p === 'silence') return S.silence;
    const [k, x] = Object.entries(p)[0];
    switch (k) {
      case 'atom':
        return S.pure(value(x));
      case 'seq': {
        if (x.every((s) => s.weight === undefined || F(s.weight) === 1)) return S.fastcat(...x.map((s) => pat(s.pat)));
        return S.timeCat(...x.map((s) => [F(s.weight ?? 1), pat(s.pat)]));
      }
      case 'alt':
        return S.slowcat(...x.map(pat));
      case 'stack':
        return S.stack(...x.map(pat));
      case 'choose':
        notes.add('choose: Strudel picks with its own randomness (Patch seed ignored)');
        return S.chooseCycles(...x.pats.map(pat));
      case 'poly': {
        const seqs = x.pats.filter((s) => s.length);
        const steps = x.steps !== undefined && x.steps !== null ? F(x.steps) : seqs[0].length;
        return S.stack(...seqs.map((s) => S.fastcat(...s.map(pat)).fast(Number(steps) / s.length)));
      }
      case 'signal':
        return SIGNALS[x];
      case 'apply':
        return transform(x.transform)(pat(x.pat));
      default:
        throw new Error(`unknown pattern node '${k}'`);
    }
  }

  function transform(t) {
    if (t === 'rev') return (p) => p.rev();
    if (t === 'palindrome') return (p) => p.palindrome();
    if (t === 'id') return (p) => p;
    const [k, x] = Object.entries(t)[0];
    switch (k) {
      case 'fast':
        return (p) => p.fast(F(x));
      case 'slow':
        return (p) => p.slow(F(x));
      case 'early':
        return (p) => p.early(F(x));
      case 'late':
        return (p) => p.late(F(x));
      case 'ply':
        return (p) => p.ply(x);
      case 'segment':
        return (p) => p.segment(x);
      case 'iter':
        return (p) => p.iter(x);
      case 'euclid':
        return (p) => p.euclidRot(x.pulses, x.steps, x.rotate ?? 0);
      case 'degrade':
        if (x.seed) notes.add('degrade: Strudel randomness (Patch seed ignored)');
        return (p) => p.degradeBy(x.prob);
      case 'every': {
        const f = transform(x.then);
        if (x.offset) {
          notes.add('every with an offset: approximated by shifting the cycle count');
          return (p) => p.early(x.offset).every(x.n, f).late(x.offset);
        }
        return (p) => p.every(x.n, f);
      }
      case 'sometimes': {
        if (x.seed) notes.add('sometimes: Strudel randomness (Patch seed ignored)');
        const f = transform(x.then);
        return (p) => p.sometimesBy(x.prob, f);
      }
      case 'some_cycles': {
        const f = transform(x.then);
        return (p) => p.someCyclesBy(x.prob, f);
      }
      case 'within': {
        const f = transform(x.then);
        return (p) => p.within(F(x.begin), F(x.end), f);
      }
      case 'chunk': {
        const f = transform(x.then);
        return (p) => p.chunk(x.n, f);
      }
      case 'off': {
        const f = transform(x.then);
        return (p) => p.off(F(x.by), f);
      }
      case 'superimpose': {
        const f = transform(x);
        return (p) => p.superimpose(f);
      }
      case 'add':
        return (p) => p.fmap((v) => (typeof v === 'number' ? v + x : v));
      case 'mul':
        return (p) => p.fmap((v) => (typeof v === 'number' ? v * x : v));
      case 'range':
        return (p) => p.fmap((v) => (typeof v === 'number' ? x.lo + v * (x.hi - x.lo) : v));
      case 'set':
        return (p) => p.fmap(() => x);
      case 'chain': {
        const fs = x.map(transform);
        return (p) => fs.reduce((q, f) => f(q), p);
      }
      default:
        throw new Error(`unknown transformation '${k}'`);
    }
  }

  // Layers: each pattern tagged with its Patch target, in order.
  return function program(prog) {
    return prog.layers
      .filter((l) => !l.muted)
      .map((l) => ({
        name: l.name,
        target: l.target,
        pattern: pat(l.pat).fmap((v) => ({ _patch: l.target, _v: v })),
      }));
  };
}
