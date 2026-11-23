uniform vec2 resolution;
uniform vec3 colour_a,colour_b;
uniform vec4 body0,body1;
uniform float body_beat,rim,emblem;
uniform float layer_opacity,appearance,energy,kick,scene_beat,section,chapter,seed;
// A change of performer passes through the hologram's distortion.
uniform float film_glitch,clock_s,presence;
flat in vec2 j_waist,j_sh[2],j_el[2],j_wr[2],j_dir[2],j_hand[2];
flat in float j_work[2],j_desk[2],j_lift[2],j_unit,j_yaw,j_back;
out vec4 frag;
// The DJ as a shadow: one drawn outline, filled flat near-black, read
// against the lit scene behind it (the user, 2026-10-26). Optimus-like:
// a helmet wider at the crown than the jaw, a slim neck, a V torso, tapered
// arms, real hands. The arms' joints come from one skeleton
// (performer-surface.glsl), so they keep their length in every pose.
// Metres, origin at the hips (level with the console's top), x right, y up.
const int BODY=16;
// The right half of the torso, hips to neck (mirrored for the left), traced
// from the reference photos, and the same side with that arm raised: the
// armpit and the shoulder's top lift, and the chest stretches up into the arm.
const vec2 TORSO[BODY]=vec2[BODY](
 vec2(.150,-.08),vec2(.155,.10),vec2(.132,.29),vec2(.132,.35),vec2(.138,.392),vec2(.170,.45),vec2(.222,.493),vec2(.215,.516),
 vec2(.205,.538),vec2(.189,.560),vec2(.156,.583),vec2(.109,.605),vec2(.062,.628),vec2(.047,.660),vec2(.046,.700),vec2(.0,.705));
const vec2 TORSO_UP[BODY]=vec2[BODY](
 vec2(.150,-.08),vec2(.155,.10),vec2(.132,.29),vec2(.142,.35),vec2(.155,.44),vec2(.168,.482),vec2(.172,.50),vec2(.172,.53),
 vec2(.170,.56),vec2(.168,.585),vec2(.150,.600),vec2(.120,.615),vec2(.080,.628),vec2(.047,.660),vec2(.046,.700),vec2(.0,.705));
float smin(float a,float b,float k){float h=max(k-abs(a-b),0.0)/k;return min(a,b)-h*h*k*.25;}
// Distance to a polygon (negative inside), after Inigo Quilez.
void seg(vec2 p,vec2 a,vec2 b,inout float s,inout float d){
 vec2 e=b-a,w=p-a,q=w-e*clamp(dot(w,e)/dot(e,e),0.0,1.0);
 d=min(d,dot(q,q));
 bvec3 c=bvec3(p.y>=a.y,p.y<b.y,e.x*w.y>e.y*w.x);
 if(all(c)||all(not(c)))s=-s;
}
float torso(vec2 p,float bounce){
 float d=1e9,s=1.0;
 vec2 prev=vec2(0),first=vec2(0);
 for(int i=0;i<2*BODY;i++){
  // Right half up, then the left half down, each with its own arm's lift.
  int k=i<BODY?i:2*BODY-1-i;
  vec2 v=mix(TORSO[k],TORSO_UP[k],i<BODY?j_lift[1]:j_lift[0]);
  if(i>=BODY)v.x=-v.x;
  v.y+=bounce*smoothstep(.45,.6,v.y);
  if(i>0)seg(p,prev,v,s,d);else first=v;
  prev=v;
 }
 seg(p,prev,first,s,d);
 return s*sqrt(d);
}
// A tapered segment from a (radius ra) to b (rb).
float cone(vec2 p,vec2 a,vec2 b,float ra,float rb){
 vec2 ba=b-a;float t=clamp(dot(p-a,ba)/dot(ba,ba),0.0,1.0);
 return length(p-a-ba*t)-mix(ra,rb,t);
}
float capsule(vec2 p,vec2 a,vec2 b,float r){return cone(p,a,b,r,r);}
float round_box(vec2 p,vec2 c,vec2 half_size,float r){
 vec2 q=abs(p-c)-half_size+r;return length(max(q,0.0))+min(max(q.x,q.y),0.0)-r;
}
// A hand from the wrist w along dir: a palm, four fingers (split at the
// tips) that curl into a fist, the thumb on the side toward the body, the
// index finger out when it points. `lying`: flat on a control, seen end-on
// from the audience, so it shows short.
float hand(vec2 p,vec2 w,vec2 dir,float curl,float index,float lying){
 vec2 n=vec2(-dir.y,dir.x);
 float squash=mix(1.0,.5,lying);
 vec2 q=vec2(dot(p-w,dir)/squash,dot(p-w,n));
 float s=n.x>0.0?-1.0:1.0;
 float palm=round_box(q,vec2(.046,0),vec2(.042,mix(.033,.037,curl)),.015);
 float reach=.058*(1.0-curl);
 float fingers=cone(q,vec2(.082,0),vec2(.082+reach,0),.031,.027);
 // Gaps between the fingers, only toward their tips.
 float tips=smoothstep(.0,.02,q.x-.095);
 for(int k=-1;k<=1;k++){
  float gap=abs(q.y-float(k)*.0155)-.0016;
  fingers=max(fingers,-(gap+(1.0-tips)*.02));
 }
 float d=smin(palm,curl<.98?fingers:1e9,.012);
 vec2 tb=mix(vec2(.078,s*.056),vec2(.062,s*.036),curl);
 d=smin(d,cone(q,vec2(.022,s*.028),tb,.014,.011),.008);
 if(index>.01)d=smin(d,cone(q,vec2(.078,s*.016),vec2(.078+.066*index,s*.020),.011,.0095),.006);
 return d*squash;
}
float oval(vec2 p,vec2 c,vec2 r){vec2 q=(p-c)/r;return (length(q)-1.0)*min(r.x,r.y);}
float emblem_p(vec2 p){
 float stem=max(abs(p.x+.30)-.09,abs(p.y)-.62);
 vec2 b=p-vec2(.02,.28);
 float bowl=max(abs(length(b*vec2(1.0,1.15))-.27)-.085,-(p.x+.22));
 return min(min(stem,bowl),length(p-vec2(.52,-.52))-.11);
}
// The Omarchy logo on a 15 × 15 grid, one row per int (bit 0 the left
// column, row 0 the top), from its official SVG.
const int OMARCHY[15]=int[15](32767,16513,22781,20485,20485,20485,20485,20487,20485,20485,20485,20485,24573,16513,32511);
// 1 inside the logo, p in −1 … 1.
float omarchy(vec2 p){
 vec2 g=(p*.5+.5)*15.0;
 if(any(lessThan(g,vec2(0)))||any(greaterThanEqual(g,vec2(15))))return 0.0;
 int x=int(g.x),y=14-int(g.y);
 return float((OMARCHY[y]>>x)&1);
}
float patch_accent(float beat){float c=.5+.5*cos(beat*6.2831853);return c*c;}
void main(){
 float u=j_unit;
 vec2 fc=gl_FragCoord.xy;
 float beam=floor(clock_s*30.0);
 if(film_glitch>.001){
  // Bands slip sideways, a few at a time, as the filmed DJ's do.
  float band=floor(fc.y/(u*.03));
  float n=fract(sin(dot(vec2(band,beam),vec2(12.9898,78.233)))*43758.5453);
  float on=step(1.0-film_glitch*.3,fract(sin(dot(vec2(band*.7,beam+3.0),vec2(12.9898,78.233)))*43758.5453));
  fc.x+=(n-.5)*.08*u*film_glitch*on;
  // A signal in trouble: slow rolling waves, and blocks that jump aside.
  fc.x+=(sin(fc.y/u*12.0+clock_s*3.0)*.018+sin(fc.y/u*41.0-clock_s*7.0)*.006)*u*film_glitch;
  vec2 blk=floor(fc/(u*vec2(.19,.06)));
  float bn=fract(sin(dot(blk+vec2(beam*.37,floor(clock_s*8.0)),vec2(12.9898,78.233)))*43758.5453);
  fc.x+=(bn-.5)*.18*u*film_glitch*step(1.0-film_glitch*.18,bn);
 }
 vec2 p=(fc-j_waist)/u;
 // The motion, eased in Rust (patch_visual::groove): nothing here jumps.
 bool playing=scene_beat>=0.0;
 float pulse=playing?patch_accent(body_beat):0.0;
 float nod=body0.x,bob=body0.y;
 // The head looks toward a hand that works (or the one on its ear), and
 // down a little while it does.
 float look=clamp(body1.x+(j_work[1]-j_work[0])*.2,-1.0,1.0);
 float down=max(j_work[0],j_work[1])*.008+body1.y*.012;
 vec2 body=p;
 // The head and headphones, turned by the bob and the look, on the neck.
 vec2 pivot=vec2(0,.655);
 float tilt=bob+look*.10+j_yaw*.4;
 vec2 hp=body-pivot;hp=mat2(cos(tilt),sin(tilt),-sin(tilt),cos(tilt))*hp+pivot;
 hp+=vec2(-look*.012,nod*1.6+down);
 // A man's head against his body (1.92 m, 90 kg): rounder and smaller than
 // the helmet was, its bottom still on the neck.
 const vec2 HEAD=vec2(1.12,.92);
 vec2 h=vec2(0,.655+.107*HEAD.y);
 hp=h+(hp-h)/HEAD;
 // The helmet: a full crown over a square jaw and a firm chin.
 vec2 hq=hp-h;
 float head=smin(oval(hq,vec2(0,.022),vec2(.078,.090)),round_box(hq,vec2(0,-.052),vec2(.064,.060),.034),.02);
 // Over-ear cups at the eyes' height, the band high over the crown.
 float cups=min(oval(hp,h+vec2(-.086,.004),vec2(.017,.040)),oval(hp,h+vec2(.086,.004),vec2(.017,.040)));
 float band=max(abs(oval(hp,h+vec2(0,.016),vec2(.096,.126)))-.0045,h.y+.004-hp.y);
 float top=min(smin(head,cups,.006),band)*HEAD.y;
 float trunk=torso(body,max(body1.z,body1.w)*.5);
 float d=smin(trunk,top,.01);
 float limbs=1e9;
 for(int i=0;i<2;i++){
  float side=i==0?-1.0:1.0;
  // The arm in its own half (x outward).
  vec2 q=body*vec2(side,1.0),m=vec2(side,1.0)/u;
  vec2 sj=(j_sh[i]-j_waist)*m,el=(j_el[i]-j_waist)*m,wr=(j_wr[i]-j_waist)*m;
  // One tapered limb: shoulder to elbow to wrist, the same width either
  // side of the elbow, so it bends without a lump.
  float upper=cone(q,sj,el,.058,.044);
  float fore=cone(q,el,wr,.044,.030);
  vec2 along=wr-el;
  vec2 dir=normalize(mix(normalize(j_dir[i]),along/max(length(along),1e-4),.35));
  // The hand is drawn 0.17 long: at scale, 0.18.
  float hd=hand(wr+(q-wr)/1.06,wr-dir*.008,dir,j_hand[i].x,j_hand[i].y,j_desk[i])*1.06;
  // The shoulder joins the body softly; the forearm and hand keep their
  // edges, so a hand by the head stays a hand.
  d=smin(d,upper,.018);
  d=min(d,smin(fore,hd,.012));
  limbs=min(limbs,min(upper,min(fore,hd)));
 }
 float aa=max(fwidth(d),1e-4);
 float alpha=clamp(.5-d/aa,0.0,1.0);
 vec3 sidec=mix(colour_a,colour_b,smoothstep(-.05,.05,p.x));
 // The backlight: a light behind the DJ catching the outline, A's colour
 // on the left, B's on the right, strongest on the head and shoulders,
 // fading down the body; a fine bright edge and a soft halo outside it.
 float px=d*u;
 float high=mix(.35,1.0,smoothstep(.25,.62,p.y));
 float halo=rim*high*(.32*exp(-max(px,0.0)/2.2)+.14*exp(-max(px,0.0)/12.0))*(1.0-alpha);
 if(alpha<=0.0&&halo<.004)discard;
 vec3 col=vec3(.010,.010,.013);
 float edge=exp(min(px,0.0)/1.3)*rim*high;
 col+=mix(sidec,vec3(1.0),.25)*edge*.9;
 // Slit eyes and the headphones' inner edges, brighter on the beat.
 for(int i=0;i<2;i++){
  float sx=i==0?-1.0:1.0;
  vec2 e=hp-(h+vec2(sx*.034,.008));
  float r=sx*.32;e=mat2(cos(r),-sin(r),sin(r),cos(r))*e;
  float slit=length(e/vec2(.024,.0045))-1.0;
  col+=mix(sidec,vec3(1.0),.45)*smoothstep(.4,-.4,slit)*(1.3+pulse*(.8+energy));
  vec2 cup=h+vec2(sx*.086,.004);
  float ring=abs(oval(hp,cup,vec2(.012,.029)))-.0015;
  float inner=smoothstep(.0,-.012,(hp.x-cup.x)*sx);
  col+=sidec*smoothstep(.002,-.001,ring)*inner*(.55+pulse*.35);
 }
 // The emblem on the left breast (our right).
 // The chest mark, as set: "P.", the Omarchy logo, or none.
 if(emblem<.5)col+=vec3(.6)*smoothstep(.06,-.02,emblem_p((body-vec2(.085,.445))/.024));
 else if(emblem<1.5){
  // Supersampled 2 × 2 so the logo's square edges stay clean.
  vec2 c=(body-vec2(.085,.445))/.026,dp=vec2(dFdx(c.x),dFdy(c.y))*.25;
  float m=(omarchy(c+dp*vec2(-1,-1))+omarchy(c+dp*vec2(1,-1))+omarchy(c+dp*vec2(-1,1))+omarchy(c+dp*vec2(1,1)))*.25;
  col+=vec3(.55)*m;
 }
 // Arms and hands on top of the controls, the rest at the DJ's place.
 gl_FragDepth=limbs<.004&&body.y<.62?0.0:j_back;
 if(appearance>0.001){
  vec2 cell=fract(gl_FragCoord.xy/6.0)-.5;
  float dots=smoothstep(.34,.18,length(cell));
  col=mix(col,sidec*dots*1.2,appearance);
  alpha*=mix(1.0,max(dots,.1),appearance);
 }
 // Outside the outline only the halo's light shows (straight alpha).
 vec3 glow=mix(sidec,vec3(1.0),.15);
 float cover=alpha+halo;
 // Present only in part while it morphs: hologram points come and go.
 float cellN=fract(sin(dot(floor(gl_FragCoord.xy/2.0),vec2(12.9898,78.233)))*43758.5453);
 cover*=smoothstep(cellN-.3,cellN+.3,presence*1.6-.3);
 if(film_glitch>.001){
  // A hologram while it morphs: cool blue light, fine rolling scanlines and
  // a soft flicker.
  float scan=.65+.35*step(.5,fract((gl_FragCoord.y+clock_s*40.0)/4.0));
  float flick=.92+.08*fract(sin(beam*12.9898)*43758.5453);
  vec3 holo=vec3(.35,.78,1.0)*(.45+.8*max(col.r,max(col.g,col.b)))*scan*flick;
  col=mix(col,holo,film_glitch*.85);
  cover*=1.0-film_glitch*(.3+.15*(1.0-scan));
  // Colour fringes on the outline, an interference band sweeping down, lines
  // dropping out.
  col+=vec3(.9,.1,.3)*edge*film_glitch*.6*step(0.0,p.x)+vec3(.1,.4,1.0)*edge*film_glitch*.6*step(p.x,0.0);
  float sweep=abs(fract(gl_FragCoord.y/resolution.y+clock_s*.6)-.5);
  col+=vec3(.5,.9,1.0)*smoothstep(.012,0.0,sweep)*film_glitch*1.2;
  cover*=1.0-.6*film_glitch*step(.97,fract(sin(floor(gl_FragCoord.y/3.0)*12.9898+beam)*43758.5453));
 }
 frag=vec4((col*alpha+glow*halo)/max(cover,1e-4),min(cover,1.0)*layer_opacity);
}
