// The filmed DJ: the person, one of the three performers. `film_look` is how
// much he shows (see-through below 1); a change of performer passes through a
// hologram's distortion.
uniform sampler2D film;
uniform float film_look,film_glitch;
uniform float layer_opacity,clock_s;
uniform vec3 colour_a,colour_b;
in vec2 v_uv;
flat in float j_back;
out vec4 frag;
bool is_skin(vec4 c){return c.a>.5&&c.r>c.b+.07&&c.r>.1&&c.r>=c.g;}
float noise(vec2 p){return fract(sin(dot(p,vec2(12.9898,78.233)))*43758.5453);}
void main(){
 vec2 uv=v_uv;
 float g=film_glitch;
 float beam=floor(clock_s*30.0);
 if(g>.001){
  // A signal in trouble: the picture bends in slow rolling waves, and now
  // and then a block of it jumps aside and snaps back.
  uv.x+=sin(uv.y*18.0+clock_s*3.0)*.012*g+sin(uv.y*61.0-clock_s*7.0)*.004*g;
  vec2 blk=floor(uv*vec2(8.0,24.0));
  float bn=noise(blk+vec2(beam*.37,floor(clock_s*8.0)));
  uv.x+=(bn-.5)*.12*g*step(1.0-g*.18,bn);
 }
 vec4 c=texture(film,uv);
 float a=c.a;
 if(g>.001){
  // The colours part from the middle outwards, and the mask with them; a
  // faint late echo trails him.
  vec2 split=(uv-vec2(.5,.45))*.03*g;
  vec4 l=texture(film,uv+split),r=texture(film,uv-split);
  c.r=l.r;c.b=r.b;
  a=max(a,max(l.a,r.a)*.8);
  vec4 echo=texture(film,uv+vec2(.025,-.012)*g);
  a=max(a,echo.a*.35*g);
 }
 // Present only in part while it morphs: hologram points come and go.
 float cell=noise(floor(gl_FragCoord.xy/2.0));
 a*=smoothstep(cell-.3,cell+.3,film_look*1.6-.3);
 if(a<.004)discard;
 vec3 col=c.rgb;
 if(g>.001){
  // A hologram while it morphs: cool blue light, fine rolling scanlines and
  // a soft flicker.
  float luma=dot(col,vec3(.3,.59,.11));
  float scan=.65+.35*step(.5,fract((gl_FragCoord.y+clock_s*40.0)/4.0));
  float flick=.92+.08*noise(vec2(beam,2.0));
  vec3 holo=vec3(.35,.78,1.0)*(.35+1.1*luma)*scan*flick;
  col=mix(col,holo,g*.85);
  a*=1.0-g*(.3+.15*(1.0-scan));
  // Interference: a bright band sweeps down, lines drop out.
  float sweep=abs(v_uv.y-fract(clock_s*.6));
  col+=vec3(.5,.9,1.0)*smoothstep(.015,0.0,sweep)*g*1.2;
  a*=1.0-.6*g*step(.97,noise(vec2(floor(v_uv.y*120.0),beam)));
 }
 // Near the console his forearms and hands lie over the booth, its raised
 // mixer included; his shirt stays behind it. Shadowed fingers fail a colour
 // test, so a hand is also whatever is mostly skin around it: no holes.
 bool hand=false;
 if(v_uv.y*704.0>540.0&&c.a>.5){
  hand=is_skin(c);
  if(!hand){
   vec2 t=vec2(1.0/768.0,1.0/704.0)*4.0;
   int n=0;
   for(int k=0;k<8;k++){
    float an=float(k)*.7853982;
    if(is_skin(texture(film,uv+t*vec2(cos(an),sin(an)))))n++;
   }
   hand=n>=4;
  }
 }
 gl_FragDepth=hand?0.0:j_back;
 frag=vec4(col,a*layer_opacity);
}
