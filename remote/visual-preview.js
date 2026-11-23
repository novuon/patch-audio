// Browser-only renderer. No engine control, audio capture or desktop scene logic.
window.patchPreview = (() => {
  const canvas = document.getElementById('preview'), ctx = canvas.getContext('2d');
  let intent = {}, beat = 0, bpm = 120, playing = false, received = performance.now();
  let demo = false, paused = matchMedia('(prefers-reduced-motion: reduce)').matches;
  let frame = 0, last = 0, phase = 0, width = 0, height = 0;
  const smooth = {intensity:.5,motion:.5,density:.5,symmetry:.5,feedback:.3};
  const palette = ['#f0243b','#4fb3bd','#9d7cf0'];
  function resize() {
    const rect = canvas.getBoundingClientRect(), scale = Math.min(devicePixelRatio || 1, 1.5);
    width = Math.max(1,Math.round(rect.width*scale)); height = Math.max(1,Math.round(rect.height*scale));
    if (canvas.width !== width || canvas.height !== height) { canvas.width=width;canvas.height=height; }
  }
  new ResizeObserver(resize).observe(canvas);
  function draw(now) {
    frame = 0; if(document.hidden) return;
    if (now-last < 1000/30 && !paused) { frame=requestAnimationFrame(draw);return; }
    const dt = Math.min(.08,(now-last)/1000 || 0); last=now;
    if (!paused) phase += dt * (.08 + (intent.motion ?? .5)*.7);
    const currentBeat = demo ? now/1000*2 : beat + (playing ? Math.min(1.5,(now-received)/1000)*bpm/60 : 0);
    const pulse = (playing || demo) ? Math.exp(-(currentBeat%1)*5) : 0;
    ctx.fillStyle = `rgba(10,12,11,${1-(intent.feedback ?? .3)*.86})`;ctx.fillRect(0,0,width,height);
    const blend = intent.transition_beats ? Math.min(1,dt*bpm/60/intent.transition_beats*4) : 1;
    for(const field of Object.keys(smooth))smooth[field] += ((intent[field] ?? smooth[field])-smooth[field])*blend;
    const count = 6+Math.round(smooth.density*26), mirror = 2+Math.round(smooth.symmetry*10);
    const radius = Math.min(width,height)*.34, intensity = smooth.intensity;
    ctx.save();ctx.translate(width/2,height/2);ctx.globalCompositeOperation='screen';
    for(let ring=0;ring<count;ring++){
      const fraction=ring/count, r=radius*(.14+fraction)*(1+pulse*.09);
      ctx.beginPath();
      for(let point=0;point<=128;point++){
        const angle=point/128*Math.PI*2;
        const modulation=1+(.07+intensity*.13)*Math.sin(angle*mirror+phase*3+fraction*(intent.focus==='bass'?4:intent.focus==='drums'?14:8));
        const x=Math.cos(angle+phase*.17)*r*modulation, y=Math.sin(angle+phase*.17)*r*modulation;
        point?ctx.lineTo(x,y):ctx.moveTo(x,y);
      }
      const colour=intent.colour ?? 'theme';
      ctx.strokeStyle=colour==='theme'?palette[ring%3]:`hsl(${((intent.hue ?? .9)*360+(colour==='drift'?phase*24:colour==='step'?Math.floor(currentBeat/4)*35:0))%360} 70% 62%)`;
      ctx.globalAlpha=(.18+intensity*.5)*(colour==='flash'?.5+pulse*.5:1);
      ctx.lineWidth=(1+intensity*1.2)*width/900;ctx.stroke();
    }
    ctx.restore();
    if(!paused)frame=requestAnimationFrame(draw);
  }
  function wake(){if(!frame)frame=requestAnimationFrame(draw);}
  document.addEventListener('visibilitychange',()=>{if(document.hidden){cancelAnimationFrame(frame);frame=0;}else{last=performance.now();wake();}});
  return {
    update(i, source, local){intent={...i}; demo=local; beat=source?.beat ?? 0;bpm=source?.bpm ?? 120;playing=!!source?.playing;received=performance.now();wake();},
    pause(value){paused=value;wake();},
    get paused(){return paused;}
  };
})();
