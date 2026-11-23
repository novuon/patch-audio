const $ = id => document.getElementById(id);
const defaults = {intensity:.5,motion:.5,density:.5,symmetry:.5,feedback:.3,colour:'theme',focus:'whole',section:'unknown',transition_beats:4,hue:null};
let token='', enabled=false, connected=false, pending=false, demo=false, local={...defaults}, draft=null, state=null, polling=false, timer;
const controls=[];
const amounts=['intensity','motion','density','symmetry','feedback'];
function availability(){controls.forEach(input=>input.disabled=!demo && (!enabled || !connected || pending));$('return-director').disabled=demo || !enabled || !connected || pending;document.querySelectorAll('[data-scene]').forEach(button=>button.disabled=demo || !enabled || !connected || pending);}
function effective(){return demo?local:draft ?? state?.visual?.posted?.intent ?? defaults;}
function render(){
 const i=effective();
 const output=state?.visual_output;
 $('output-status').textContent=output ? `Desktop: ${output.mode} · ${output.held ? 'scene held' : 'director running'} · ${output.intent_active ? 'remote intent applied' : 'director intent'} · ${output.caption}` : 'Desktop output not reporting · a visualiser with the choreography bridge must be open.';
 controls.forEach(input=>{if(input.dataset.field && document.activeElement!==input){input.value=i[input.dataset.field];const out=$(input.dataset.field+'-value');if(out)out.textContent=Math.round(input.value*100)+'%';}});
 $('visual-status').textContent=demo?'Local demo':draft?'Sending live changes…':!connected?'Engine unavailable':state.visual.active?`${state.visual.posted.origin ?? 'External'} intent active`:'Director leads';
 renderScenes(output?.scenes);
 $('preview-label').textContent=demo?'LOCAL DEMO / PREVIEW':connected?(state.visual.active?'LIVE INTENT / PREVIEW':'DEFAULT INTENT / PREVIEW'):'WAITING FOR LIVE INTENT';
 const source=state?.dj?.sources?.filter(s=>s.playing).sort((a,b)=>b.presence-a.presence)[0];
 $('beat-label').textContent=demo?'120 BPM · DEMO':source?`${Math.round(source.bpm)} BPM · ${source.title}`:'NO PLAYING DECK';
 $('intent-note').textContent=demo?'Demo changes stay on this device. Exit demo to return to live intent.':'Changes shape the desktop visualiser when it is open. Opening this page does not start a desktop output.';
 window.patchPreview.update(i,source,demo);if(connected && $('ack').textContent==='Waiting for Patch…')$('ack').textContent='Ready · following Patch';availability();
}
async function session(){const r=await patchFetch('/api/session',{cache:'no-store',signal:AbortSignal.timeout(3000)});if(!r.ok)throw Error('Session unavailable');const s=await r.json();token=s.token;enabled=s.control;$('access').textContent=enabled?'PRIVATE CONTROL':'READ ONLY';}
async function send(intent){
 if(!enabled || !connected || pending || document.hidden)return;
 pending=true;availability();$('ack').textContent='Sending…';
 try{
  const id=crypto.randomUUID(),r=await patchFetch('/api/command',{method:'POST',headers:{'Content-Type':'application/json','X-Patch-Token':token},body:JSON.stringify({...intent,id}),signal:AbortSignal.timeout(4000)});
  const result=await r.json();if(!r.ok){if(r.status===403){await session();throw Error('Session refreshed; repeat your action');}throw Error(result.error || 'Command rejected');}
  if(result.id!==id)throw Error('Acknowledgement did not match');
  $('ack').textContent='Accepted by Patch';
 }catch(e){$('ack').textContent=e.message+' · no automatic retry';}
 finally{pending=false;if(['visual_intent','visual_clear'].includes(intent.action))draft=null;await poll();render();}
}
function change(fields){if(demo){Object.assign(local,fields);render();$('ack').textContent='Local demo · booth unchanged';}else{draft={...effective(),...fields};render();$('ack').textContent='Applying to desktop visualiser…';send({action:'visual_intent',intent:fields});}}
for(const field of amounts){
 const row=document.createElement('label');row.className='parameter';const label=document.createElement('span');label.textContent=field;
 const input=document.createElement('input');input.type='range';input.min=0;input.max=1;input.step=.01;input.id=field;input.dataset.field=field;
 const out=document.createElement('output');out.id=field+'-value';out.htmlFor=field;
 input.oninput=()=>{out.textContent=Math.round(input.value*100)+'%';if(demo){local[field]=Number(input.value);window.patchPreview.update(local,null,true);}else{draft={...effective(),[field]:Number(input.value)};window.patchPreview.update(draft,state?.dj?.sources?.find(s=>s.playing),false);}};
 input.onchange=()=>change({[field]:Number(input.value)});input.onblur=render;
 row.append(label,input,out);$('parameters').append(row);controls.push(input);
}
for(const [field,label,values] of [['colour','Colour',['theme','hold','drift','step','flash']],['focus','Follow',['whole','kick','bass','drums','vocals','melody']],['transition_beats','Change over',[0,1,4,8,16,32,64]]]){
 const row=document.createElement('label');row.className='choice';const title=document.createElement('span');title.textContent=label;
 const input=document.createElement('select');input.dataset.field=field;
 values.forEach(value=>{const option=document.createElement('option');option.value=value;option.textContent=field==='transition_beats'?value===0?'Instant':value+' beats':value==='unknown'?'Whole performance':value;input.append(option);});
 input.onchange=()=>change({[field]:field==='transition_beats'?Number(input.value):input.value});input.onblur=render;row.append(title,input);$('choices').append(row);controls.push(input);
}
let sceneSignature='';
function renderScenes(scenes){
 const signature=JSON.stringify(scenes||null);if(signature===sceneSignature)return;sceneSignature=signature;
 const root=$('cues');root.replaceChildren();
 if(!Array.isArray(scenes)||!scenes.length){const note=document.createElement('p');note.className='muted';note.textContent='Open the desktop Visualiser to load this track’s choreography.';root.append(note);return;}
 for(const scene of scenes){const button=document.createElement('button');button.className='cue';button.dataset.scene='true';button.textContent=scene.name;button.title=`${scene.part} · ${scene.bars} bars`;button.onclick=()=>{if(demo)return;$('ack').textContent=`Calling ${scene.name}…`;send({action:'visual_scene',part:scene.part,index:scene.index});};root.append(button);}
}
for(const [name,fields] of [['Drift',{intensity:.45,motion:.25,density:.35,symmetry:.75,feedback:.75,colour:'drift',focus:'whole',section:'breakdown'}],['Pulse',{intensity:.75,motion:.55,density:.55,symmetry:.9,feedback:.35,colour:'theme',focus:'kick',section:'verse'}],['Lift',{intensity:.9,motion:.85,density:.85,symmetry:.4,feedback:.55,colour:'step',focus:'drums',section:'build'}]]){
 const button=document.createElement('button');button.textContent=name;button.onclick=()=>change(fields);$('looks').append(button);controls.push(button);
}
$('return-director').onclick=()=>{draft={...defaults};render();$('ack').textContent='Returning to planned choreography…';send({action:'visual_clear'});};
$('demo').onclick=()=>{demo=!demo;local={...(state?.visual?.posted?.intent ?? defaults)};$('demo').setAttribute('aria-pressed',String(demo));$('demo').textContent=demo?'Exit demo':'Demo mode';$('ack').textContent=demo?'Local demo · booth unchanged':'Following Patch';render();};
$('freeze').setAttribute('aria-pressed',String(window.patchPreview.paused));$('freeze').textContent=window.patchPreview.paused?'Resume preview':'Pause preview';
$('freeze').onclick=()=>{window.patchPreview.pause(!window.patchPreview.paused);$('freeze').setAttribute('aria-pressed',String(window.patchPreview.paused));$('freeze').textContent=window.patchPreview.paused?'Resume preview':'Pause preview';};
$('fullscreen').hidden=!$('preview-panel').requestFullscreen;
$('fullscreen').onclick=async()=>{try{if(document.fullscreenElement)await document.exitFullscreen();else await $('preview-panel').requestFullscreen();}catch{$('ack').textContent='Full screen unavailable in this browser';}};
async function poll(){
 if(polling || document.hidden)return;polling=true;
 try{const r=await patchFetch('/api/state',{cache:'no-store',signal:AbortSignal.timeout(3000)});if(!r.ok)throw Error('Bridge unavailable');state=await r.json();connected=!!state.visual;}
 catch{connected=false;state=null;}
 finally{polling=false;$('connection').textContent=connected?'PATCH CONNECTED':'BRIDGE DISCONNECTED';$('connection').classList.toggle('online',connected);render();}
}
async function resume(){clearInterval(timer);connected=false;availability();try{await session();}catch(e){enabled=false;$('ack').textContent=e.message;}await poll();timer=setInterval(poll,500);}
document.addEventListener('visibilitychange',()=>{if(document.hidden){clearInterval(timer);connected=false;availability();}else resume();});
window.addEventListener('pageshow',e=>{if(e.persisted)resume();});render();resume();
