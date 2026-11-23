// Rendering and user intent only. No musical scheduling or optimistic state.
const $ = id => document.getElementById(id);
const page = document.body.dataset.page;
let token = '', enabled = false, connected = false, pending = false, sequence = -1;
let timer;
let candidateKey = '';
let currentSet = null;
const sourceNodes = new Map();
const buttons = [];
function text(id, value) { if ($(id)) $(id).textContent = value; }
function node(tag, content, className) {
  const e = document.createElement(tag); e.textContent = content;
  if (className) e.className = className; return e;
}
function availability() {
  document.querySelectorAll('button,input').forEach(e => e.disabled = !enabled || !connected || pending || (page === 'visual' && !visualAvailable));
  if (page === 'dj') {
    const ready = enabled && connected && !pending;
    $('set-start').disabled = !ready || currentSet?.on === true;
    $('set-resume').disabled = !ready || currentSet?.on !== true || currentSet?.paused !== true;
  }
}
let visualAvailable = false;
async function session() {
  const r = await patchFetch('/api/session', {cache:'no-store'});
  if (!r.ok) throw Error('Session unavailable');
  const s = await r.json(); token = s.token; enabled = s.control;
  text('access', enabled ? (location.protocol === 'https:' ? 'PRIVATE CONTROL' : 'LOCAL CONTROL') : 'READ ONLY');
}
async function send(intent) {
  if (!enabled || !connected || pending) return;
  pending = true; availability(); text('ack','Sending…');
  try {
    const id = crypto.randomUUID();
    const r = await patchFetch('/api/command', {method:'POST', headers:{'Content-Type':'application/json','X-Patch-Token':token}, body:JSON.stringify({...intent,id}), signal:AbortSignal.timeout(4000)});
    const result = await r.json();
    if (!r.ok) {
      if (r.status === 403) { await session(); throw Error('Session refreshed; repeat your action'); }
      throw Error(result.error || 'Command rejected');
    }
    if (result.id !== id) throw Error('Acknowledgement did not match');
    text('ack','Accepted by Patch · refreshing state');
  } catch(e) { text('ack',e.message + ' · no automatic retry'); }
  finally { pending = false; await poll(); availability(); }
}
function segmented(id, values, action) {
  values.forEach(value => {
    const b = node('button', value); b.type = 'button'; b.dataset.value = value;
    b.setAttribute('aria-pressed','false'); b.onclick = () => send({action,value});
    $(id).append(b); buttons.push(b);
  });
}
if (page === 'dj') {
  $('mix-now').onclick = () => send({action:'mix_now'});
  $('change-next').onclick = () => send({action:'change_next'});
  $('set-start').onclick = () => send({action:'set_start'});
  $('set-resume').onclick = () => send({action:'set_resume'});
  segmented('modes',['observe','suggest','prepare','act'],'mode');
  segmented('directions',['build','hold','release'],'direction');
  if ($('candidate-panel')) $('candidate-panel').open = matchMedia('(min-width: 900px)').matches;
} else {
  ['intensity','motion','density','symmetry','feedback'].forEach(field => {
    const row = node('label','', 'parameter');
    row.append(node('span',field));
    const input = document.createElement('input'); input.type = 'range';
    input.min = '0'; input.max = '1'; input.step = '0.01'; input.id = field;
    const value = node('output','—'); value.id = field+'-value'; value.htmlFor = field;
    input.oninput = () => value.textContent = Math.round(Number(input.value)*100)+'%';
    // Release sends one intent; input movement does not flood IPC.
    input.onchange = () => send({action:'visual',field,value:Number(input.value)});
    row.append(input,value); $('parameters').append(row);
  });
}
document.querySelector('[data-command]')?.addEventListener('click',e => send(e.currentTarget.dataset.command === 'clear' ? {action:'visual_clear'} : {action:'mode',value:'suggest'}));
function time(s) { s = Math.max(0, Math.floor(s || 0)); return Math.floor(s/60)+':'+String(s%60).padStart(2,'0'); }
function shortTitle(title) {
  return (title || '').replace(/\s*\([^)]*(?:mix|version|edit)\)\s*$/i,'') || title;
}
function signal(values) {
  const svg = document.createElementNS('http://www.w3.org/2000/svg','svg');
  svg.setAttribute('viewBox','0 0 600 66'); svg.setAttribute('preserveAspectRatio','none');
  svg.setAttribute('role','img'); svg.setAttribute('aria-label','Track energy over the next seconds');
  for (const [cls,d] of [['signal-grid','M0 1H600 M0 33H600 M0 65H600'],['signal-fill',''],['signal-line','']]) {
    const path = document.createElementNS(svg.namespaceURI,'path'); path.setAttribute('class',cls); path.setAttribute('d',d); svg.append(path);
  }
  return svg;
}
function renderCountdown(s) {
  const t = s?.transition;
  const source = t && (s.sources || []).find(source => source.id === t.from);
  const valid = Number.isFinite(t?.start_beat) && Number.isFinite(source?.beat);
  const remaining = valid ? Math.max(0,t.start_beat-source.beat) : null;
  const running = t?.started;
  text('countdown-status',!s ? 'Live timing unavailable' : !t ? 'No transition scheduled' : running ? 'Mixing now' : !valid ? 'Waiting for engine timing' : !source.playing ? 'Paused · resumes with playback' : remaining === 0 ? 'At the start boundary' : t.ready ? 'Scheduled · 4 beats per bar' : 'Preparing · 4 beats per bar');
  text('countdown-bars',!running && remaining != null ? Math.ceil(remaining/4) : '—');
  text('countdown-time',!running && remaining != null && Number.isFinite(source.bpm) && source.bpm > 0 ? time(Math.ceil(remaining*60/source.bpm)) : '—');
}
function renderDJ(s) {
  currentSet = s.set || null;
  text('set-status',!currentSet || !currentSet.on ? 'No AI set running' : currentSet.paused ? 'Paused · ready to resume' : `Running · ${currentSet.name || 'AI DJ set'}`);
  $('set-start').textContent = currentSet?.on ? 'AI DJ set running' : 'Start AI DJ set';
  $('set-resume').textContent = currentSet?.on && currentSet?.paused ? 'Resume AI DJ set' : 'Resume paused set';
  availability();
  renderCountdown(s);
  text('phase',s.phase || 'Unavailable'); text('note',s.note || ''); text('mode-label',s.mode || '—');
  const hints = {observe:'Watches the music. Makes no changes.',suggest:'Proposes tracks. You decide what happens.',prepare:'Cues the next move. Approve from the desktop.',act:'Chooses tracks and performs transitions.'};
  text('mode-hint',hints[s.mode] || 'Waiting for the AI DJ.');
  buttons.forEach(b => b.setAttribute('aria-pressed',String(b.dataset.value === s.mode || b.dataset.value === s.direction)));
  const sources = (s.sources || []).filter(source => source.title);
  const lead = [...sources].filter(source => source.playing).sort((a,b) => b.presence-a.presence)[0] || sources[0];
  const t = s.transition, chosen = (s.candidates || []).find(c => c.selected);
  const incoming = t && sources.find(source => source.id === t.to && source !== lead);
  let next = incoming;
  if (!next && chosen) next = {...chosen,id:'candidate:'+chosen.id,preview:true};
  if (!next) next = sources.find(source => source !== lead);
  const displayed = [lead,next].filter(Boolean), ids = new Set(displayed.map(source => source.id));
  for (const [id,card] of sourceNodes) if (!ids.has(id)) { card.remove(); sourceNodes.delete(id); }
  if (displayed.length && !sourceNodes.size) $('sources').replaceChildren();
  displayed.forEach((source,index) => {
    let card = sourceNodes.get(source.id);
    if (!card) {
      card = node('article','', 'source');
      const label = node('div','','eyebrow'); label.append(node('span',''),node('span','','deck-mark'));
      const graph = node('div','','signal'); graph.append(signal());
      const caption = node('div','','signal-caption'); caption.append(node('span','ENERGY AHEAD'),node('span','')); graph.append(caption);
      const clock = node('div','','track-clock'); clock.append(node('span',''),node('span',''));
      const progress = document.createElement('progress'); progress.setAttribute('aria-label','Track position');
      card.append(label,node('h2',''),node('p','','track-meta'),graph,progress,clock);
      sourceNodes.set(source.id,card); $('sources').append(card);
    }
    card.classList.toggle('leading',index === 0); card.classList.toggle('playing',!!source.playing);
    const [label,title,meta,graph,progress,clock] = card.children;
    label.firstChild.textContent = index === 0 ? (source.playing ? 'NOW PLAYING' : 'LOADED') : source.preview ? 'NEXT CANDIDATE' : incoming ? 'UP NEXT' : 'STANDBY';
    label.lastChild.textContent = source.preview ? 'AI PICK' : source.id.replace('deck-','').toUpperCase();
    title.textContent = shortTitle(source.title); title.title = source.title;
    meta.textContent = [source.artist,source.bpm ? Number(source.bpm).toFixed(1)+' BPM' : '',source.key].filter(Boolean).join(' · ');
    const values = (source.energy_ahead || []).slice(0,60);
    graph.hidden = !values.length || index !== 0;
    if (values.length) {
      const points = values.map((v,i) => `${(i*600/Math.max(1,values.length-1)).toFixed(1)},${(65-Math.min(1,Math.max(0,Number(v)||0))*64).toFixed(1)}`);
      const svg = graph.firstChild;
      svg.children[1].setAttribute('d',`M0,66 L${points.join(' L')} L600,66 Z`);
      svg.children[2].setAttribute('d',`M${points.join(' L')}`);
      graph.lastChild.lastChild.textContent = `NEXT ${values.length} SECONDS`;
    }
    progress.hidden = !!source.preview; clock.hidden = !!source.preview;
    if (!source.preview) {
      progress.max = Math.max(1,source.position+source.remaining); progress.value = source.position;
      clock.firstChild.textContent = source.playing ? time(source.position) : 'Cued at '+time(source.position);
      clock.lastChild.textContent = source.playing ? '−'+time(source.remaining) : time(source.position+source.remaining);
    }
    $('sources').append(card);
  });
  if (!displayed.length) $('sources').replaceChildren(node('div','Waiting for a playing deck. Open the Booth to start.','empty'));
  const style = t?.style ? t.style[0].toUpperCase()+t.style.slice(1) : '';
  text('transition',t ? `${style} · ${t.bars} bars` : 'Listening for the next move');
  const target = t && sources.find(source => source.id === t.to);
  text('detail',t ? `${t.from.replace('deck-','').toUpperCase()} → ${t.to.replace('deck-','').toUpperCase()} · ${shortTitle(target?.title || chosen?.title || 'Incoming track')}` : 'Patch is following the music.');
  text('plan-state',t ? (t.started ? 'IN THE MIX' : t.ready ? 'READY' : 'PREPARING') : (s.phase || 'LISTENING').toUpperCase());
  $('plan-progress').value = t && t.total ? t.done/t.total : 0;
  const outgoing = t && sources.find(source => source.id === t.from);
  const beats = t && outgoing?.beat != null ? Math.max(0,Math.ceil(t.start_beat-outgoing.beat)) : null;
  text('timing',t ? t.started ? `${t.done} / ${t.total} steps` : beats != null ? `Starts in ${beats} beats` : 'Waiting for the engine boundary' : 'Timing follows Patch');
  const rehearsal = s.evidence?.rehearsal_score;
  text('rehearsal',t && rehearsal != null ? `Rehearsed ${Math.round(rehearsal)} / 100` : '');
  const candidates = (s.candidates || []).slice(0,6);
  text('candidate-count',`${(s.candidates || []).length} OPTIONS`);
  const key = JSON.stringify(candidates);
  if (key !== candidateKey) {
    candidateKey = key;
    $('candidates').replaceChildren(...candidates.map(c => {
      const row = node('div','', 'candidate'+(c.selected ? ' selected' : ''));
      const info = node('div',''); info.append(node('strong',shortTitle(c.title)),node('p',[c.artist,c.bpm ? Number(c.bpm).toFixed(1)+' BPM' : '',c.key].filter(Boolean).join(' · ')));
      if ((c.reasons || []).length) {
        const details = document.createElement('details');
        details.append(node('summary','Why this track'),node('p',c.reasons.join(' · '))); info.append(details);
      }
      row.append(info,node('span','FIT '+Math.round(c.score*100),'score')); return row;
    }));
    if (!candidates.length) text('candidates','No candidates yet');
  }
}
function renderVisual(v) {
  const p = v.posted; if (!p) return;
  text('visual-status',v.active ? 'Manual intent active' : 'Director leads');
  text('section',p.intent.section === 'unknown' ? 'Whole performance' : p.intent.section);
  text('visual-note',`Focus: ${p.intent.focus} · colour: ${p.intent.colour} · change over ${p.intent.transition_beats} beats`);
  ['intensity','motion','density','symmetry','feedback'].forEach(field => {
    const input = $(field);
    if (document.activeElement !== input) { input.value = p.intent[field]; text(field+'-value',Math.round(p.intent[field]*100)+'%'); }
  });
}
let polling = false;
let latestVisual;
document.querySelectorAll('input').forEach(input => input.addEventListener('blur',() => { if (latestVisual) renderVisual(latestVisual); }));
async function poll() {
  if (polling) return;
  polling = true;
  try {
    const r = await patchFetch('/api/state',{cache:'no-store',signal:AbortSignal.timeout(3000)});
    if (!r.ok) throw Error('Bridge unavailable');
    const s = await r.json(); connected = page === 'dj' ? !!s.dj && !!s.booth : !!s.visual;
    visualAvailable = !!s.visual;
    text('connection',connected ? 'PATCH CONNECTED' : 'ENGINE UNAVAILABLE');
    $('connection').classList.toggle('online',connected);
    if (s.sequence !== sequence) {
      sequence = s.sequence;
      if (page === 'dj' && s.dj) renderDJ({...s.dj,set:s.booth?.set});
      if (page === 'visual' && s.visual) { latestVisual = s.visual; renderVisual(s.visual); }
    }
    if (!connected) { text('note',s.error || 'Engine unavailable. Playback is independent of this page.'); if (page === 'dj') renderCountdown(null); }
    if (page === 'visual' && !s.visual) text('visual-status',s.visual_error || 'Visual intent unavailable');
  } catch(e) { connected = false; if (page === 'dj') renderCountdown(null); text('connection','BRIDGE DISCONNECTED'); $('connection').classList.remove('online'); }
  finally { polling = false; availability(); }
}
async function resume() {
  clearInterval(timer); sequence = -1; connected = false; availability();
  try { await session(); } catch(e) { text('ack',e.message); enabled = false; }
  await poll(); timer = setInterval(poll,500);
}
availability(); resume();
document.addEventListener('visibilitychange',() => { if (document.hidden) clearInterval(timer); else resume(); });
window.addEventListener('pageshow',e => { if(e.persisted) resume(); });
