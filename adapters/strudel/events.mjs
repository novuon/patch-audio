// Strudel haps → Patch events (patch_pattern::event::Event as JSON).
// Strudel's value objects ({s, n, note, gain, …}) stop here; nothing
// Strudel-shaped crosses into Patch. MIT, part of Patch.

const frac = (f) => f.toFraction();

const PARAMS = ['gain', 'velocity', 'pan', 'speed', 'begin', 'end', 'cut', 'legato'];

// Counted per load, reported in the reply: what Strudel produced that Patch
// had no target for (a finding for the native engine, not an error).
export function translator(stats) {
  const plain = (v) => {
    if (typeof v === 'number' || typeof v === 'string') return v;
    if (typeof v === 'boolean') return v ? 1 : 0;
    return v == null ? 0 : String(v);
  };
  const params = (v) => {
    const out = {};
    for (const k of PARAMS) if (typeof v[k] === 'number') out[k] = v[k];
    return out;
  };
  return function toEvent(hap, layer) {
    const v = hap.value;
    const base = {
      layer,
      whole: { begin: frac(hap.whole.begin), end: frac(hap.whole.end) },
    };
    const loc = hap.context?.locations?.[0];
    if (loc) base.loc = [loc.start, loc.end];
    if (v && typeof v === 'object' && v._patch) {
      const inner = v._v;
      if (inner && typeof inner === 'object') {
        // A tagged object pattern (e.g. visual.x(n("1 2").gain(…))): its
        // first numeric/word field is the value, the rest are params.
        const key = ['value', 'n', 'note', 's'].find((k) => k in inner) ?? Object.keys(inner)[0];
        return { ...base, target: v._patch, value: plain(inner[key]), params: params(inner) };
      }
      return { ...base, target: v._patch, value: plain(inner) };
    }
    if (v && typeof v === 'object') {
      if ('s' in v) {
        const s = String(v.s) + (v.n !== undefined ? `:${v.n}` : '');
        if (/^dm-[0-9a-f]{16}$/.test(String(v.s))) {
          return { ...base, target: { to: 'sample', source: { asset: String(v.s) } }, value: typeof v.gain === 'number' ? v.gain : 1, params: params(v) };
        }
        return { ...base, target: { to: 'sound' }, value: s, params: params(v) };
      }
      if ('note' in v || 'n' in v) {
        return { ...base, target: { to: 'note' }, value: plain(v.note ?? v.n), params: params(v) };
      }
      const keys = Object.keys(v);
      stats.unmapped[keys.join('+')] = (stats.unmapped[keys.join('+')] ?? 0) + 1;
      return null;
    }
    // A bare value with no control: a note if it looks like one, else a sound.
    if (typeof v === 'number' || /^[a-g][#sb]?-?\d*$/i.test(String(v))) {
      return { ...base, target: { to: 'note' }, value: plain(v) };
    }
    return { ...base, target: { to: 'sound' }, value: plain(v) };
  };
}
