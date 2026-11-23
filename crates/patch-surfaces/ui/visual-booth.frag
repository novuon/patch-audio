uniform vec2 resolution;
uniform vec3 colour_a,colour_b,bands;
uniform float layer_opacity,kick,energy,handover,preparing;
flat in int part_id;
in vec3 local_position,world_position,surface_normal;
out vec4 frag;
// PATCH_SURFACE
float lit(float x){return clamp(x,0.0,1.0);}
// A thin line where d is 0, about w wide (metres).
float hair(float d,float w){return smoothstep(w,0.0,abs(d));}
float meter_of(int i,float side){vec2 m=source_count>i?source_meter[i]:vec2(0);return side<0.0?m.x:m.y;}
void main(){
 vec3 q=local_position,n=normalize(surface_normal),w=world_position;
 vec3 v=normalize(patch_eye(resolution)-w);
 vec3 tint=mix(colour_a,colour_b,smoothstep(-.45,.45,w.x));
 // Solid machined surfaces: dark gunmetal, a key light from above, a tight
 // highlight, deck-coloured rim light, and a lit bevel on every edge.
 vec3 key=normalize(vec3(-.25,.9,.55));
 float diffuse=max(dot(n,key),0.0);
 float spec=pow(max(dot(n,normalize(key+v)),0.0),64.0);
 float fresnel=pow(1.0-max(dot(n,v),0.0),4.0);
 vec3 col=vec3(.032,.037,.046)*(.4+diffuse*1.2)+vec3(.6,.68,.78)*spec*.45+tint*fresnel*.35;
 vec3 a=abs(q);
 float second=a.x+a.y+a.z-max(a.x,max(a.y,a.z))-min(a.x,min(a.y,a.z));
 if(part_id>=4&&part_id<=34&&part_id!=6&&part_id!=7&&part_id!=8&&part_id!=9)col+=tint*smoothstep(.93,1.0,second)*.22;
 if(part_id==35||part_id==36)col*=.55;
 vec3 glow=vec3(0);
 // The console's lit edges: the deck colours meet in the middle.
 if(part_id==2)glow=tint*(.8+kick*.5)+vec3(1.0)*exp(-abs(w.x)/.02)*(1.2+kick);
 if(part_id==3)glow=tint*(.35+kick*.25);
 if(part_id==1){float grid=smoothstep(.97,1.0,abs(fract(w.x*2.0)*2.0-1.0));col+=tint*grid*.03;}
 // Jogs, as on a pro controller: a brushed-metal outer ring with grip
 // notches, a satin touch plate, a glass display in the middle with the
 // track's progress round its edge and a dot turning with the beat; the
 // edge lit in the deck's colour with its level.
 if((part_id==6||part_id==7)&&n.y>.5){
  int i=part_id-6;vec4 s=source_count>i?source_state[i]:vec4(0,-1,0,0);
  vec3 ink=i==0?colour_a:colour_b;
  float r=length(q.xz);
  float ang=atan(q.x,q.z);
  float turn=s.y<0.0?0.0:-s.y*1.5707963;
  // Brushed metal: an anisotropic sheen across the ring, fine notches.
  float sheen=pow(max(cos(ang*2.0-1.1),0.0),5.0);
  vec3 metal=vec3(.085,.09,.105)+vec3(.30,.32,.36)*sheen*.55+vec3(.5,.55,.6)*spec*.4;
  metal*=1.0-.35*step(.6,fract(ang/6.2831853*96.0))*smoothstep(.9,.93,r);
  // The touch plate: satin black, fine concentric texture.
  vec3 plate=vec3(.014,.016,.02)*(1.0+.35*(.5+.5*sin(r*260.0)))+vec3(.45,.5,.6)*spec*.22;
  col=mix(plate,metal,smoothstep(.855,.865,r));
  col+=vec3(.22,.24,.28)*hair(r-.86,.01)+vec3(.3,.32,.36)*hair(r-.992,.008);
  // The display: dark glass with a soft reflection.
  float glass=step(r,.34);
  col=mix(col,vec3(.006,.007,.01)+vec3(.08,.09,.11)*smoothstep(.34,.0,length(q.xz-vec2(-.1,.12)))*.3,glass);
  col+=vec3(.2,.22,.26)*hair(r-.34,.01);
  float heard=max(meter_of(i,-1.0),meter_of(i,1.0));
  float pulse=s.y<0.0?0.0:exp(-fract(s.y)*7.0);
  // The lit edge: thin, following the level.
  glow+=ink*hair(r-.975,.018)*(.25+heard*1.3+pulse*.4)*max(s.w,.3);
  // Progress round the display; the beat dot inside it.
  float progress=source_count>i?source_progress[i]:0.0;
  float swept=fract(-ang/6.2831853+1.0);
  glow+=ink*hair(r-.30,.018)*(swept<progress?.95:.07)*max(s.w,.4);
  vec2 dot_at=vec2(sin(turn),cos(turn))*.21;
  glow+=mix(ink,vec3(1.0),.4)*smoothstep(.05,.03,length(q.xz-dot_at))*1.2*s.w;
  // The position line on the touch plate, as on a CDJ's jog.
  float marker=pow(max(cos(ang-turn),0.0),900.0)*smoothstep(.42,.5,r)*smoothstep(.84,.78,r);
  glow+=vec3(.9,.92,1.0)*marker*(.25+.6*s.w);
 }
 // The spindle: hidden in the display's glass.
 if(part_id==8||part_id==9)col=vec3(.006,.007,.01)+vec3(.2)*spec;
 // Channel fader and crossfader caps catch the deck colour.
 if(part_id==13||part_id==14){vec3 ink=part_id==13?colour_a:colour_b;glow+=ink*(n.y>.5?.9:.25);}
 if(part_id==16)glow+=mix(colour_a,colour_b,crossfader)*(n.y>.5?1.0:.3);
 if(part_id==11||part_id==12||part_id==15)col*=.3;
 // Knobs: the pointer at 12 o'clock faces the audience; turned up, it goes
 // clockwise as the DJ sees it (toward the DJ's right, our left).
 if(part_id>=17&&part_id<=24&&n.y>.5){
  int i=(part_id-17)%2,row=(part_id-17)/2;
  vec4 eq=source_count>i?source_tone[i]:vec4(0);
  float value=row==0?eq.z:row==1?eq.y:row==2?eq.x:eq.w;
  float ang=value*2.35;
  vec2 dir=vec2(-sin(ang),cos(ang));
  float along=dot(q.xz,dir),across=dot(q.xz,vec2(dir.y,-dir.x));
  float pointer=smoothstep(.16,.04,abs(across))*smoothstep(.15,.3,along)*step(along,.95);
  vec3 ink=i==0?colour_a:colour_b;
  float killed=row<3&&value<=-.97?1.0:0.0;
  glow+=mix(ink,vec3(1.0,.25,.2),killed)*pointer*1.3;
  float ring=exp(-pow((length(q.xz)-.92)/.05,2.0));
  glow+=ink*ring*(.08+abs(value)*.5);
 }
 // Channel meters in the middle, left and right of each channel, the
 // same readings as the DJ surface's: LED segments rising toward the
 // audience, amber near the top, red at the top.
 if(part_id==25||part_id==26){
  int i=part_id-25;
  float level=meter_of(i,q.x);
  float x=abs(q.x);
  float column=step(.18,x)*step(x,.86);
  float z=(q.z+1.0)*.5;
  float seg=step(.28,fract(z*15.0));
  float on=step(z,level);
  vec3 led=mix(i==0?colour_a:colour_b,vec3(1.0,.72,.25),smoothstep(.66,.72,z));
  led=mix(led,vec3(1.0,.22,.2),step(.87,z));
  glow+=led*seg*column*(on*1.5+.045);
 }
 // The mixer's top: scales round the knobs, beside the faders and along
 // the crossfader, each channel's panel outlined.
 if(part_id==10&&n.y>.5){
  float marks=0.0;
  for(int k=0;k<2;k++){
   float side=k==0?-1.0:1.0;
   for(int row=0;row<4;row++){
    vec2 d=w.xz-vec2(side*PATCH_STRIP_X,PATCH_KNOB_Z0-float(row)*PATCH_KNOB_DZ);
    float r=length(d),ang=atan(-d.x,d.y);
    float rad=row==3?.026:.022;
    float tick=hair(fract(ang/.47+.5)-.5,.09)*step(abs(ang),2.4)*step(rad,r)*step(r,rad+.0045);
    marks=max(marks,tick);
   }
   float fx=w.x-side*PATCH_STRIP_X;
   float along=(w.z-PATCH_FADER_Z)/(PATCH_FADER_TRAVEL*.5);
   marks=max(marks,step(.016,abs(fx))*step(abs(fx),.024)*step(abs(along),1.0)*hair(fract(along*4.0+.5)-.5,.06));
   // The channel's panel.
   vec2 panel=abs(w.xz-vec2(side*PATCH_STRIP_X,.04))-vec2(.036,.168);
   marks=max(marks,hair(max(panel.x,panel.y),.0012)*.6);
  }
  float cx=w.x/(PATCH_XFADER_TRAVEL*.5);
  marks=max(marks,step(abs(cx),1.0)*step(.008,abs(w.z-PATCH_XFADER_Z))*step(abs(w.z-PATCH_XFADER_Z),.016)*hair(fract(cx*4.0+.5)-.5,.06));
  marks=max(marks,hair(w.z+.138,.0012)*step(abs(w.x),PATCH_MIXER_W-.01)*.6);
  col+=vec3(.15,.16,.19)*marks;
 }
 // Deck plates: the jog's well, the pitch scale.
 if((part_id==4||part_id==5)&&n.y>.5){
  float sx=part_id==4?-1.0:1.0;
  vec2 d=w.xz-vec2(sx*PATCH_DECK_X,PATCH_DECK_Z);
  float well=hair(length(d)-PATCH_PLATTER_R-.012,.0015);
  float px=w.x-(sx*PATCH_DECK_X-.203);
  float along=(w.z-.02)/.12;
  float scale=step(.012,abs(px))*step(abs(px),.02)*step(abs(along),1.0)*hair(fract(along*4.0+.5)-.5,.07);
  // Section lines: the screen's bay, the pads' row; four corner screws.
  float local_z=w.z-.02;
  float lines=max(hair(local_z-.205,.0012)*step(abs(w.x-sx*PATCH_DECK_X),.178),hair(local_z+.19,.0012)*step(abs(w.x-sx*PATCH_DECK_X+.018),.178));
  vec2 c=abs(w.xz-vec2(sx*PATCH_DECK_X,.02))-vec2(PATCH_DECK_W-.018,.252);
  float screw=smoothstep(.0055,.004,length(c))*(.6+.4*step(.0012,abs(c.x-c.y)));
  col+=vec3(.14,.15,.18)*max(max(well,scale),lines*.7)+vec3(.12,.13,.15)*screw;
 }
 // Performance pads, eight in a row: the deck's colour, the one under the
 // beat (of two bars) bright.
 if((part_id==42||part_id==43)&&n.y>.5){
  int i=part_id-42;vec4 s=source_count>i?source_state[i]:vec4(0,-1,0,0);
  vec3 ink=i==0?colour_a:colour_b;
  float x=(q.x+1.0)*4.0,k=floor(x);
  float pad=step(.1,fract(x))*step(fract(x),.9)*step(abs(q.z),.78);
  float now=s.y<0.0?-1.0:mod(floor(s.y),8.0);
  float hit=k==now?1.0:0.0;
  float fade=s.y<0.0?0.0:exp(-fract(s.y)*3.0);
  glow+=ink*pad*(.10+.08*s.w+hit*(.6+fade*.9)*s.w);
 }
 // The deck's CUE (amber while stopped), SYNC lit while it plays in sync.
 if(part_id==44||part_id==45){
  int i=part_id-44;vec4 s=source_count>i?source_state[i]:vec4(0,-1,0,0);
  float rq=length(q.xz);
  float ring=hair(rq-.88,.08)+hair(rq-.35,.07)*step(-.2,q.x*-1.0);
  glow+=mix(vec3(1.0,.62,.2)*.45,(i==0?colour_a:colour_b)*.22,s.w)*(n.y>.5?(.2+ring):.3);
 }
 if(part_id==46||part_id==47){
  int i=part_id-46;vec4 s=source_count>i?source_state[i]:vec4(0,-1,0,0);
  glow+=vec3(.85,.9,1.0)*(.04+s.w*.55)*(n.y>.5?1.0:.3);
 }
 // The mixer's CUE: the headphones on the incoming deck while the next
 // transition waits and early in the blend.
 if(part_id==40||part_id==41){
  int i=part_id-40;
  float mine=source_count>i?source_state[i].z:0.0,other=source_count>1-i?source_state[1-i].z:0.0;
  float cueing=(preparing>.5||(handover>=0.0&&handover<.5))&&mine<other?1.0:0.0;
  glow+=vec3(1.0,.62,.2)*(.05+cueing*.9)*(n.y>.5?1.0:.3);
 }
 if(part_id==27||part_id==28){int i=part_id-27;float fx=source_count>i?source_fx[i]:0.0;glow+=(i==0?colour_a:colour_b)*(.06+fx*1.6)*(n.y>.5?1.0:.3);}
 // PLAY / PAUSE: the ▶‖ glyph lit, the edge pulsing with the beat.
 if(part_id==29||part_id==30){
  int i=part_id-29;vec4 s=source_count>i?source_state[i]:vec4(0,-1,0,0);
  float pulse=s.y<0.0?0.0:exp(-fract(s.y)*5.0);
  vec3 ink=i==0?colour_a:colour_b;
  vec2 g=q.xz*vec2(-1,1);
  float tri=max(max(-g.x-.35,g.x*.8+abs(g.y)-.05),-g.x*0.0-1.0);
  tri=max(tri,-(g.x+.35));
  float bars=min(max(abs(g.x-.28)-.06,abs(g.y)-.32),max(abs(g.x-.48)-.06,abs(g.y)-.32));
  float glyph=step(min(max(g.x-.08,tri),bars),0.0);
  float edge=hair(length(q.xz)-.9,.08);
  glow+=(n.y>.5?1.0:.3)*(ink*glyph*(.25+s.w*.9)+ink*edge*(.08+s.w*(.4+pulse*.8)));
 }
 if(part_id==33||part_id==34)glow+=tint*.25*(n.y>.5?1.0:.3);
 // The console wings: spectrum bars from the shared output bands.
 if(false){
  float x=(q.x+1.0)*.5;float k=floor(x*16.0);
  float band=k<5.0?bands.x:k<11.0?bands.y:bands.z;
  float wobble=.75+.25*sin(k*1.7+kick*3.0);
  float height=clamp(band*wobble*1.4,0.0,1.0);
  float bar=step(.25,fract(x*16.0))*step(.75,1.0-fract(x*16.0)+.5);
  float z=(1.0-q.z)*.5;
  glow+=tint*bar*step(z,height)*(.25+.75*z)*.9;
  glow+=tint*bar*.03;
 }
 if(part_id==37)glow=mix(colour_a,colour_b,(q.x+1.0)*.5)*(.6+kick*.6);
 // Deck screens: dark glass in a frame, the track's waveform played up to
 // the playhead, beat-grid ticks under it.
 if((part_id==38||part_id==39)&&n.y>.5){
  int i=part_id-38;float progress=source_count>i?source_progress[i]:0.0;
  vec4 s=source_count>i?source_state[i]:vec4(0,-1,0,0);
  vec3 ink=i==0?colour_a:colour_b;
  float x=(-q.x+1.0)*.5;
  col=vec3(.005,.006,.009)+vec3(.12,.13,.16)*spec*.3;
  float wave=.35+.5*abs(sin(x*90.0+float(i))*sin(x*13.0+float(i)*2.0))+.15*sin(x*400.0);
  float body=step(abs(q.z),wave*.62)*step(abs(q.x),.97);
  glow+=ink*body*(x<progress?.85:.18)*max(s.w,.35);
  glow+=vec3(1.0)*hair(x-progress,.004)*step(abs(q.z),.85);
  float grid=hair(fract(x*64.0)-.5,.06)*step(.78,abs(q.z));
  glow+=vec3(.5,.55,.6)*grid*.25;
  col+=vec3(.18,.2,.24)*max(hair(abs(q.x)-.985,.012),hair(abs(q.z)-.96,.035));
 }
 frag=vec4(col+glow,layer_opacity);
}
