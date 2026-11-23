// The browser sends human intent. Patch owns transport and mixer state.
const $ = id => document.getElementById(id);
let token = '', control = false, connected = false, busy = false, state;
let generation = 0, stateSequence = -1, pollTimer, sendTimer, lastSend = 0;
const queued = new Map(), dragging = new Set(), cards = new Map();
function el(tag, content='', cls='') { const e=document.createElement(tag); e.textContent=content; if(cls)e.className=cls; return e; }
function commandId() { return Array.from(crypto.getRandomValues(new Uint8Array(16)),b=>b.toString(16).padStart(2,'0')).join(''); }
function ack(message) { $('ack').textContent=message; }
function time(s) { s=Math.max(0,Math.floor(Number(s)||0)); return Math.floor(s/60)+':'+String(s%60).padStart(2,'0'); }
async function request(path, options={}) {
  const abort=new AbortController(), timer=setTimeout(()=>abort.abort(),4000);
  try {
    const r=await patchFetch(path,{cache:'no-store',...options,signal:abort.signal});
    const value=r.headers.get('content-type')?.includes('application/json')?await r.json():{error:await r.text()};
    if(!r.ok){const error=Error(value.error || 'Patch request failed');error.status=r.status;throw error;}
    return value;
  } finally { clearTimeout(timer); }
}
function available() {
  const live=control && connected && !document.hidden;
  document.querySelectorAll('button').forEach(b=>{
    const deck=b.dataset.deck;
    const loaded=!deck || state?.decks?.[deck==='a'?0:1]?.loaded;
    b.disabled=!live || !loaded || busy || queued.size>0;
    if(b.dataset.action==='sync') b.disabled ||= !state?.decks?.[deck==='a'?1:0]?.loaded;
  });
  document.querySelectorAll('input').forEach(input=>input.disabled=!live || (input.dataset.deck && !state?.decks?.[input.dataset.deck==='a'?0:1]?.loaded));
}
function bindSlider(input, key, intent) {
  input.addEventListener('pointerdown',()=>dragging.add(key));
  input.addEventListener('pointerup',()=>{dragging.delete(key);pump();});
  input.addEventListener('pointercancel',()=>{dragging.delete(key);pump();});
  input.addEventListener('input',()=>{
    if(!control || !connected || document.hidden)return;
    queued.set(key,{...intent,value:Number(input.value)});
    renderSliderValue(input,key);available();pump();
  });
  input.addEventListener('change',()=>{dragging.delete(key);pump();});
  input.addEventListener('blur',()=>{dragging.delete(key);if(state)render(state);});
}
function renderSliderValue(input,key,amount=Number(input.value)) {
  const n=amount;
  if(key==='crossfader') $('mix-position').textContent=n===0?'A only':n===1?'B only':Math.abs(n-.5)<.005?'Centre':n<.5?'Towards A':'Towards B';
  else $(key+'-value').textContent=n<=0.0001?'Mute':(Math.round((66*n-60)*10)/10).toFixed(1)+' dB';
}
for(const deck of ['a','b']) {
  const card=el('section','','deck'), heading=el('div','','deck-heading');
  heading.append(el('h2','DECK '+deck.toUpperCase()),el('span','Waiting','transport-state'));
  const title=el('h3','No track loaded','title'), artist=el('div','','artist');
  const meta=el('div','','track-meta');meta.append(el('span','—'),el('span','—'));
  const progress=document.createElement('progress');progress.max=1;progress.value=0;progress.setAttribute('aria-label','Track progress');
  const actions=el('div','','transport-controls');
  for(const [action,label] of [['transport','Play'],['cue','Cue'],['sync','Sync']]) {
    const button=el('button',label,action==='transport'?'primary':action==='sync'?'sync':'');
    button.dataset.action=action;button.dataset.deck=deck;button.disabled=true;
    if(action==='cue')button.title='Pause and return to saved cue';
    button.onclick=()=>{
      const d=state?.decks?.[deck==='a'?0:1];
      const intent={action,deck}; if(action==='transport')intent.value=d?.playing?'pause':'play';
      send(intent);
    };actions.append(button);
  }
  const label=el('label','','level-label'), value=el('output','—');value.id='fader-'+deck+'-value';
  const input=document.createElement('input');input.id='fader-'+deck;input.dataset.deck=deck;input.type='range';input.min=0;input.max=1;input.step=.01;input.value=60/66;input.disabled=true;
  label.htmlFor=input.id;label.append(el('span','Level'),value);value.htmlFor=input.id;
  bindSlider(input,'fader-'+deck,{action:'fader',deck});
  card.append(heading,title,artist,meta,progress,actions,label,input);$('decks').append(card);
  cards.set(deck,{card,heading,title,artist,meta,progress,actions,input});
}
bindSlider($('crossfader'),'crossfader',{action:'crossfader'});
document.querySelectorAll('[data-mix]').forEach(b=>b.onclick=()=>send({action:'crossfader',value:Number(b.dataset.mix)}));
let inFlightKey;
function reconcile(input,key,value) {
  if(!dragging.has(key) && !queued.has(key) && inFlightKey!==key && Number.isFinite(Number(value))) {
    input.value=value;renderSliderValue(input,key,Number(value));
  }
}
function render(booth) {
  state=booth;
  for(const [deck,c] of cards) {
    const d=booth.decks?.[deck==='a'?0:1] || {};
    c.card.classList.toggle('playing',!!d.playing);
    c.heading.lastChild.textContent=d.loaded?(d.playing?'PLAYING':'PAUSED'):'EMPTY';
    c.title.textContent=d.loaded?(d.title || 'Untitled track'):'No track loaded';c.artist.textContent=d.artist || '';
    c.meta.firstChild.textContent=d.grid?.bpm?(d.grid.bpm*(1+(d.tempo || 0))).toFixed(1)+' BPM':'No beat grid';
    c.meta.lastChild.textContent=time(d.position)+' / '+time(d.duration);
    c.progress.max=Math.max(1,d.duration || 0);c.progress.value=d.position || 0;
    c.actions.firstChild.textContent=d.playing?'Pause':'Play';
    reconcile(c.input,'fader-'+deck,booth.faders?.[deck==='a'?0:1]);
  }
  reconcile($('crossfader'),'crossfader',booth.xfader);available();
}
async function send(intent,key) {
  if(!control || !connected || busy || document.hidden)return;
  const epoch=generation, id=commandId();busy=true;inFlightKey=key;lastSend=performance.now();available();ack('Sending…');
  try {
    const result=await request('/api/command',{method:'POST',headers:{'Content-Type':'application/json','X-Patch-Token':token},body:JSON.stringify({...intent,id})});
    if(result.id!==id)throw Error('Command acknowledgement did not match');
    if(epoch===generation)ack('Accepted by Patch');
  } catch(e) {
    queued.clear();dragging.clear();
    if(epoch===generation){
      ack(e.name==='AbortError'?'Result unknown · reconnecting':e.message);
      if(e.status===403){
        try {const s=await request('/api/session');if(epoch===generation){token=s.token;control=s.control;ack('Session refreshed · repeat your action');}}
        catch {control=false;}
      }
    }
  } finally {
    busy=false;inFlightKey=undefined;
    if(epoch===generation){await poll();if(state)render(state);available();pump();}
  }
}
function pump() {
  clearTimeout(sendTimer);
  if(busy || !queued.size || !connected || !control || document.hidden)return;
  const wait=100-(performance.now()-lastSend);
  if(wait>0){sendTimer=setTimeout(pump,wait);return;}
  const [key,intent]=queued.entries().next().value;queued.delete(key);
  send(intent,key);
}
let polling=false;
async function poll() {
  if(polling || document.hidden)return;polling=true;const epoch=generation;
  try {
    const s=await request('/api/state');if(epoch!==generation)return;
    connected=!!s.booth;
    $('connection').textContent=connected?'Patch connected':'Engine unavailable';$('connection').classList.toggle('online',connected);
    if(connected && s.sequence!==stateSequence){stateSequence=s.sequence;render(s.booth);}
    if(!connected){queued.clear();ack(s.booth_error || 'Start Patch on the computer');}
  } catch(e) {
    if(epoch!==generation)return;
    connected=false;queued.clear();dragging.clear();$('connection').textContent='Disconnected';$('connection').classList.remove('online');
    ack('Controls offline · music stays on Patch');
  } finally {polling=false;available();}
}
async function resume() {
  generation++;clearInterval(pollTimer);clearTimeout(sendTimer);queued.clear();dragging.clear();connected=false;control=false;stateSequence=-1;available();
  const epoch=generation;
  try {const s=await request('/api/session');if(epoch!==generation)return;token=s.token;control=s.control;$('access').textContent=control?'CONTROL ENABLED':'READ ONLY';}
  catch(e){if(epoch===generation)ack(e.message);}
  if(epoch!==generation)return;
  await poll();if(connected)ack(control?'Ready · control Patch':'Read-only preview');pollTimer=setInterval(poll,250);
}
document.addEventListener('visibilitychange',()=>{if(document.hidden){generation++;clearInterval(pollTimer);clearTimeout(sendTimer);queued.clear();dragging.clear();available();}else resume();});
window.addEventListener('pageshow',e=>{if(e.persisted)resume();});
available();resume();
