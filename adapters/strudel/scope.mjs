// Patch targets inside Strudel code: the functions that let a Strudel
// pattern address Patch units. Each tags its pattern's values with a Patch
// target ({_patch: target, _v: value}); events.mjs turns tagged haps into
// Patch events. Plain Strudel (s("bd"), note("c e g")) needs no tags.
//
//   deck("a").filter("<200 800 1600 400>")   stem("vocals").sometimes(mute)
//   deck("b").stem("drums", "mute ~")        visual.intensity("<0.2 0.8>")
//   scene("<tunnel kaleido>")                xfade(sine.slow(8))
//   kit("t1", "kick ~ snare ~")              sample("dm-0123456789abcdef")
//   control("name", "…")  mixer("music").gain("-6 0")  midi(1).cc(74, saw)
//   ai("<hold build>")
//
// MIT, part of Patch.

export function makeScope(core) {
  const { reify, pure } = core;

  // A name argument: Strudel turns "a" into a mini pattern; take its value.
  const nameOf = (x) => {
    if (x && x._Pattern) {
      const h = x.queryArc(0, 1)[0];
      return h ? String(h.value) : '';
    }
    return String(x);
  };
  const tag = (target, pat) =>
    reify(pat).fmap((v) => (v && v._patch ? v : { _patch: target, _v: v }));
  const set = (word) => (pat) => pat.fmap((v) => (v && v._patch ? { ...v, _v: word } : word));

  const deckParams = ['filter', 'volume', 'low', 'mid', 'high', 'fx', 'transport', 'loop', 'jump', 'cue'];
  const deck = (d) => {
    const id = nameOf(d).toLowerCase();
    if (!['a', 'b', 'c', 'd'].includes(id)) throw new Error(`deck is a, b, c or d (not '${id}')`);
    const o = {};
    for (const p of deckParams) o[p] = (pat) => tag({ to: 'deck', deck: id, param: p }, pat);
    o.stem = (name, pat) => tag({ to: 'stem', deck: id, stem: nameOf(name) }, pat ?? pure('unmute'));
    return o;
  };

  const visual = new Proxy(
    {},
    { get: (_, param) => (pat) => tag({ to: 'visual', param: String(param) }, pat) },
  );

  const asset = (id) => {
    const s = nameOf(id);
    if (!/^dm-[0-9a-f]{16}$/.test(s)) throw new Error(`'${s}' is not a derived-material id (dm-…)`);
    return s;
  };
  const kitName = (k) => {
    const s = nameOf(k);
    if (!/^[A-Za-z0-9_-]{1,48}$/.test(s)) throw new Error('a kit name is letters, digits, - and _');
    return s;
  };

  return {
    deck,
    deckA: deck('a'),
    deckB: deck('b'),
    deckC: deck('c'),
    deckD: deck('d'),
    stem: (name, pat) => tag({ to: 'stem', deck: 'a', stem: nameOf(name) }, pat ?? pure('unmute')),
    visual,
    scene: (pat) => tag({ to: 'scene' }, pat),
    xfade: (pat) => tag({ to: 'crossfader' }, pat),
    crossfader: (pat) => tag({ to: 'crossfader' }, pat),
    control: (name, pat) => tag({ to: 'control', name: nameOf(name) }, pat),
    ai: (pat) => tag({ to: 'ai' }, pat),
    mixer: (strip) => ({
      gain: (pat) => tag({ to: 'mixer', strip: nameOf(strip), param: 'gain' }, pat),
      mute: (pat) => tag({ to: 'mixer', strip: nameOf(strip), param: 'mute' }, pat),
    }),
    midi: (ch) => {
      const channel = Number(nameOf(ch));
      return {
        note: (pat) => tag({ to: 'midi', channel, kind: 'note' }, pat),
        cc: (n, pat) => tag({ to: 'midi', channel, kind: { cc: Number(nameOf(n)) } }, pat),
      };
    },
    kit: (k, pat) => tag({ to: 'sample', source: { kit: kitName(k) } }, pat),
    sample: (id, pat) => tag({ to: 'sample', source: { asset: asset(id) } }, pat ?? pure(1)),
    mute: set('mute'),
    unmute: set('unmute'),
    solo: set('solo'),
    unsolo: set('unsolo'),
  };
}
