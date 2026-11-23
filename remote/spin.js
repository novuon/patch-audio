// The browser sends human intent. Patch owns transport, timing and the mix.
// The platters turn from each deck's own reported position and pitch: a
// 33⅓ rpm record at tempo t turns at (1+t) × 33⅓, and the angle follows the
// deck's reported position, not a browser clock. Nothing here schedules a mix.
const $ = id => document.getElementById(id);
let token = '', control = false, connected = false, busy = false, state, latestDJ = null;
let generation = 0, stateSequence = -1, pollTimer, candidateKey = '';
const cards = new Map(), buttonStates = [];
function el(tag, content = '', cls = '') { const e = document.createElement(tag); e.textContent = content; if (cls) e.className = cls; return e; }
function commandId() { return Array.from(crypto.getRandomValues(new Uint8Array(16)),b=>b.toString(16).padStart(2,'0')).join(''); }
function ack(message) { $('ack').textContent = message; }
function time(s) { s = Math.max(0, Math.floor(Number(s) || 0)); return Math.floor(s/60)+':'+String(s%60).padStart(2,'0'); }
function shortTitle(title) { return (title || '').replace(/\s*\([^)]*(?:mix|version|edit)\)\s*$/i,'') || title; }
async function request(path, options = {}) {
  const abort = new AbortController(), timer = setTimeout(() => abort.abort(), 4000);
  try {
    const r = await patchFetch(path, {cache:'no-store', ...options, signal: abort.signal});
    const value = r.headers.get('content-type')?.includes('application/json') ? await r.json() : {error: await r.text()};
    if (!r.ok) { const error = Error(value.error || 'Patch request failed'); error.status = r.status; throw error; }
    return value;
  } finally { clearTimeout(timer); }
}
function available() {
  const live = control && connected && !document.hidden;
  document.querySelectorAll('button').forEach(b => {
    const deck = b.dataset.deck;
    const loaded = !deck || state?.decks?.[deck === 'a' ? 0 : 1]?.loaded;
    b.disabled = !live || !loaded || busy;
    if (b.dataset.action === 'sync') b.disabled ||= !state?.decks?.[deck === 'a' ? 1 : 0]?.loaded;
  });
  if (currentSet) {
    $('set-start').disabled = !live || busy || currentSet.on === true;
    $('set-resume').disabled = !live || busy || currentSet.on !== true || currentSet.paused !== true;
  }
}
// A 33⅓ record turns 33⅓ times a minute; the deck's pitch scales that, so the
// platter's period is real music time rather than a decorative animation.
const RPM = 100/3;
function spinPeriod(deck) {
  const tempo = Number(deck?.tempo) || 0;
  return Math.max(0.35, 60 / (RPM * (1 + tempo)));
}
for (const deck of ['a','b']) {
  const card = el('section','','deck');
  const head = el('div','','deck-head');
  head.append(el('span','DECK '+deck.toUpperCase(),'deck-mark'), el('span','Waiting','deck-state'));
  const platter = el('div','','platter');
  const ticks = el('div','','platter-ticks');
  // 48 hairlines about the platter's centre; every fourth is the long mark.
  for (let i = 0; i < 48; i++) {
    const tick = document.createElement('i');
    tick.style.transform = `rotate(${(i * 360 / 48).toFixed(3)}deg)`;
    ticks.append(tick);
  }
  ticks.append(el('div','','platter-index'));
  const label = el('div','','platter-label');
  label.append(el('b',deck.toUpperCase()), el('span','IDLE'));
  const arm = el('div','','tonearm');
  arm.append(el('div','','tonearm-shaft'), el('div','','tonearm-head'), el('div','','tonearm-pivot'), el('div','','tonearm-weight'));
  platter.append(el('div','','platter-ring'), ticks, el('div','','platter-wobble'), label, el('div','','platter-spindle'), arm);
  const title = el('h3','No track loaded','deck-title');
  const sub = el('p','','deck-sub');
  const clock = el('div','','deck-clock'); clock.append(el('span','—'), el('b','—'));
  const progress = document.createElement('progress');
  progress.max = 1; progress.value = 0; progress.setAttribute('aria-label','Track position');
  const cue = el('div','','deck-cue');
  for (const [action, text] of [['transport','Play'],['cue','Cue'],['sync','Sync']]) {
    const button = el('button', text, action === 'transport' ? 'primary' : '');
    button.dataset.action = action; button.dataset.deck = deck; button.disabled = true;
    if (action === 'cue') button.title = 'Pause and return to the saved cue point';
    button.onclick = () => {
      const d = state?.decks?.[deck === 'a' ? 0 : 1];
      const intent = {action, deck}; if (action === 'transport') intent.value = d?.playing ? 'pause' : 'play';
      send(intent);
    };
    cue.append(button);
  }
  card.append(head, platter, title, sub, clock, progress, cue);
  $('decks').append(card);
  cards.set(deck, {card, platter, ticks, label, title, sub, clock, progress, cue});
}
$('mix-now').onclick = () => send({action:'mix_now'});
$('change-next').onclick = () => send({action:'change_next'});
$('set-start').onclick = () => send({action:'set_start'});
$('set-resume').onclick = () => send({action:'set_resume'});
for (const [id, values, action] of [['modes',['observe','suggest','prepare','act'],'mode'],['directions',['build','hold','release'],'direction']]) {
  for (const value of values) {
    const b = el('button', value); b.type = 'button'; b.dataset.value = value;
    b.setAttribute('aria-pressed','false'); b.onclick = () => send({action, value});
    $(id).append(b); buttonStates.push(b);
  }
}
$('candidate-panel').open = matchMedia('(min-width: 900px)').matches;
function render(booth, dj) {
  state = booth;
  for (const [deck, c] of cards) {
    const d = booth.decks?.[deck === 'a' ? 0 : 1] || {};
    const index = deck === 'a' ? 0 : 1;
    c.card.classList.toggle('playing', !!d.playing);
    c.card.classList.toggle('leading', !!d.playing && index === 0);
    c.card.classList.toggle('incoming', !!d.playing && index === 1);
    c.card.children[0].lastChild.textContent = d.loaded ? (d.playing ? 'PLAYING' : 'PAUSED') : 'EMPTY';
    c.title.textContent = d.loaded ? shortTitle(d.title || 'Untitled track') : 'No track loaded';
    c.title.title = d.title || '';
    c.sub.textContent = [d.artist, d.grid?.bpm ? (d.grid.bpm*(1+(d.tempo || 0))).toFixed(1)+' BPM' : '', d.key ? 'KEY '+d.key : ''].filter(Boolean).join(' · ');
    c.clock.firstChild.textContent = d.loaded ? time(d.position) : '—';
    c.clock.lastChild.textContent = d.loaded ? '−'+time(Math.max(0,(d.duration || 0)-(d.position || 0))) : '—';
    c.progress.max = Math.max(1, d.duration || 0); c.progress.value = d.position || 0;
    c.cue.firstChild.textContent = d.playing ? 'Pause' : 'Play';
    c.platter.style.setProperty('--spin', spinPeriod(d)+'s');
    c.ticks.style.animationPlayState = d.playing && !document.hidden ? 'running' : 'paused';
    c.label.lastChild.textContent = d.loaded ? (d.grid?.bpm ? (d.grid.bpm*(1+(d.tempo||0))).toFixed(0) : '—')+' BPM' : 'IDLE';
  }
  if (booth.xfader != null) {
    const x = Math.max(0, Math.min(1, Number(booth.xfader) || 0));
    $('xfader-knob').style.left = (x*100).toFixed(1)+'%';
    $('xfader-value').textContent = x === 0 ? 'A only' : x === 1 ? 'B only' : Math.abs(x-.5) < .005 ? 'Centre' : Math.round(x*100)+'%';
  }
  renderDJ(dj, booth); available();
}
let currentSet = null;
function renderDJ(s, booth) {
  currentSet = booth?.set || null;
  $('set-status').textContent = !currentSet || !currentSet.on ? 'No AI set running' : currentSet.paused ? 'Paused · ready to resume' : `Running · ${currentSet.name || 'AI DJ set'}`;
  $('set-start').textContent = currentSet?.on ? 'AI DJ set running' : 'Start AI DJ set';
  $('set-resume').textContent = currentSet?.on && currentSet?.paused ? 'Resume AI DJ set' : 'Resume paused set';
  renderCountdown(s);
  $('phase').textContent = s?.phase || 'Unavailable';
  $('note').textContent = s?.note || '';
  $('mode-label').textContent = s?.mode || '—';
  const hints = {observe:'Watches the music. Makes no changes.',suggest:'Proposes tracks. You decide what happens.',prepare:'Cues the next move. Approve from the desktop.',act:'Chooses tracks and performs transitions.'};
  $('mode-hint').textContent = hints[s?.mode] || 'Waiting for the AI DJ.';
  buttonStates.forEach(b => b.setAttribute('aria-pressed', String(b.dataset.value === s?.mode || b.dataset.value === s?.direction)));
  const t = s?.transition;
  const style = t?.style ? t.style[0].toUpperCase()+t.style.slice(1) : '';
  $('transition-title').textContent = t ? `${style} · ${t.bars} bars` : 'Listening for the next move';
  const target = t && (s?.sources || []).find(source => source.id === t.to);
  const chosen = (s?.candidates || []).find(c => c.selected);
  $('transition-detail').textContent = t ? `${t.from.replace('deck-','').toUpperCase()} → ${t.to.replace('deck-','').toUpperCase()} · ${shortTitle(target?.title || chosen?.title || 'Incoming track')}` : 'Patch is following the music.';
  $('transition-progress').value = t && t.total ? t.done/t.total : 0;
  const outgoing = t && (s?.sources || []).find(source => source.id === t.from);
  const beats = t && outgoing?.beat != null ? Math.max(0, Math.ceil(t.start_beat-outgoing.beat)) : null;
  $('transition-timing').textContent = t ? (t.started ? `${t.done} / ${t.total} steps` : beats != null ? `Starts in ${beats} beats` : 'Waiting for the engine boundary') : 'Timing follows Patch';
  const rehearsal = s?.evidence?.rehearsal_score;
  $('transition-rehearsal').textContent = t && rehearsal != null ? `Rehearsed ${Math.round(rehearsal)} / 100` : '';
  const candidates = (s?.candidates || []).slice(0,6);
  $('candidate-count').textContent = `${(s?.candidates || []).length} OPTIONS`;
  const key = JSON.stringify(candidates);
  if (key !== candidateKey) {
    candidateKey = key;
    $('candidates').replaceChildren(...candidates.map(c => {
      const row = el('div','','candidate'+(c.selected ? ' selected' : ''));
      const info = el('div',''); info.append(el('strong',shortTitle(c.title)), el('p',[c.artist,c.bpm ? Number(c.bpm).toFixed(1)+' BPM' : '',c.key].filter(Boolean).join(' · ')));
      if ((c.reasons || []).length) {
        const details = document.createElement('details');
        details.append(el('summary','Why this track'), el('p',c.reasons.join(' · '))); info.append(details);
      }
      row.append(info, el('span','FIT '+Math.round(c.score*100),'score')); return row;
    }));
    if (!candidates.length) $('candidates').textContent = 'No candidates yet';
  }
}
function renderCountdown(s) {
  const t = s?.transition;
  const source = t && (s?.sources || []).find(source => source.id === t.from);
  const valid = Number.isFinite(t?.start_beat) && Number.isFinite(source?.beat);
  const remaining = valid ? Math.max(0, t.start_beat-source.beat) : null;
  const running = t?.started;
  $('transition-status').textContent = !s ? 'Live timing unavailable' : !t ? 'No transition scheduled' : running ? 'Mixing now' : !valid ? 'Waiting for engine timing' : !source.playing ? 'Paused · resumes with playback' : remaining === 0 ? 'At the start boundary' : t.ready ? 'Scheduled · 4 beats per bar' : 'Preparing · 4 beats per bar';
  $('transition-bars').textContent = !running && remaining != null ? Math.ceil(remaining/4) : '—';
  $('transition-time').textContent = !running && remaining != null && Number.isFinite(source.bpm) && source.bpm > 0 ? time(Math.ceil(remaining*60/source.bpm)) : '—';
}
async function send(intent) {
  if (!control || !connected || busy || document.hidden) return;
  const epoch = generation, id = commandId(); busy = true; available(); ack('Sending…');
  try {
    const result = await request('/api/command', {method:'POST', headers:{'Content-Type':'application/json','X-Patch-Token':token}, body: JSON.stringify({...intent, id})});
    if (result.id !== id) throw Error('Command acknowledgement did not match');
    if (epoch === generation) ack('Accepted by Patch');
  } catch(e) {
    if (epoch === generation) {
      ack(e.name === 'AbortError' ? 'Result unknown · no automatic retry' : e.message + ' · no automatic retry');
      if (e.status === 403) {
        try { const s = await request('/api/session'); if (epoch === generation) { token = s.token; control = s.control; ack('Session refreshed · repeat your action'); } }
        catch { control = false; }
      }
    }
  } finally {
    busy = false;
    if (epoch === generation) { await poll(); if (state) render(state, latestDJ); available(); }
  }
}
let polling = false;
async function poll() {
  if (polling || document.hidden) return; polling = true; const epoch = generation;
  try {
    const s = await request('/api/state'); if (epoch !== generation) return;
    connected = !!s.booth && !!s.dj; latestDJ = s.dj;
    $('connection').textContent = connected ? 'PATCH CONNECTED' : 'ENGINE UNAVAILABLE';
    $('connection').classList.toggle('online', connected);
    if (connected && s.sequence !== stateSequence) { stateSequence = s.sequence; render(s.booth, s.dj); }
    if (!connected) ack(s.booth_error || s.error || 'Start Patch on the computer');
  } catch(e) {
    if (epoch !== generation) return;
    connected = false;
    $('connection').textContent = 'BRIDGE DISCONNECTED'; $('connection').classList.remove('online');
    ack('Controls offline · music stays on Patch');
  } finally { polling = false; available(); }
}
async function resume() {
  generation++; clearInterval(pollTimer);
  connected = false; control = false; stateSequence = -1; candidateKey = ''; available();
  const epoch = generation;
  try { const s = await request('/api/session'); if (epoch !== generation) return; token = s.token; control = s.control; $('access').textContent = control ? 'CONTROL ENABLED' : 'READ ONLY'; }
  catch(e) { if (epoch === generation) ack(e.message); }
  if (epoch !== generation) return;
  await poll(); if (connected) ack(control ? 'Ready · control Patch' : 'Read-only preview');
  pollTimer = setInterval(poll, 250);
}
document.addEventListener('visibilitychange', () => { if (document.hidden) { generation++; clearInterval(pollTimer); available(); } else resume(); });
window.addEventListener('pageshow', e => { if (e.persisted) resume(); });
available(); resume();
