uniform vec2 resolution;
uniform float preparing;
uniform float time,beat,energy,blend,motion,density,seed;
uniform float low_a,low_b;
uniform float chapter,section,handover;
uniform float ar,ag,ab,br,bg,bb;
uniform float composed,appearance,layer_opacity,point_step;
// PATCH_SURFACE
out vec3 color;
out float opacity;
const float TAU=6.2831853;
mat2 rot(float a){return mat2(cos(a),-sin(a),sin(a),cos(a));}
float gauss(vec2 q,vec2 s){q/=s;return exp(-dot(q,q)*2.0);}
void main(){
 int id=gl_VertexID;
 float t=time;
 vec3 p;float light=1.0,front=1.0,alpha=1.0;
 float side=0.0;
 if(id<96000){
  float u=(float(id%400)+0.5)/400.0;
  float v=(float(id/400)+0.5)/240.0;
  float a=u*TAU+(t*(0.008+motion*.018)),b=v*3.14159265;
  float wave=sin(a*5.0+b*9.0+t*0.17+seed*TAU)*0.018;
  p=vec3(0.63*sin(a)*sin(b),0.78*cos(b),0.24*cos(a)*sin(b));
  front=max(cos(a),0.0);side=sin(a);
  vec2 f=p.xy;
  float le=gauss(f-vec2(-0.25,0.13),vec2(0.19,0.085));
  float re=gauss(f-vec2(0.25,0.13),vec2(0.19,0.085));
  // The two apertures are voids in a living field, never painted eyes.
  float eye=max(le,re)*pow(front,0.5);
  alpha*=1.0-smoothstep(0.18,0.65,eye);
  float arch=gauss(f-vec2(0.0,0.34),vec2(0.56,0.32));
  p.z+=pow(front,3.0)*(0.07*arch-0.10*eye);
  // Branching crown and cheek flow: the stable nonhuman signature.
  float crown=smoothstep(0.30,0.75,p.y);
  p.x*=1.0+crown*0.35;
  p.y+=crown*0.045*sin(a*3.0+t*0.14);
  p.z+=wave*(0.6+motion);
  float beatfront=beat<0.0?0.0:exp(-pow((v-fract(beat*.25))/.075,2.0));
  p.z+=beatfront*energy*.014*front;
  float unfold=smoothstep(.60,.96,motion);
  float wing=pow(1.0-front,2.0);
  p+=normalize(vec3(p.xy,p.z+0.1))*unfold*wing*(.15+.18*sin(v*8.0+t*.23));
  p.xz=rot(unfold*sin(b*3.0+t*.12)*.30)*p.xz;
  float seam=pow(0.5+0.5*sin(a*23.0+b*4.0+t*0.38),14.0);
  float ribbon=pow(0.5+0.5*cos(b*42.0+sin(a*3.0+t*.11)*1.6),16.0);
  light=0.18+0.85*seam+0.6*ribbon+exp(-pow((eye-.20)/.09,2.0))*.6;
  float faceWeight=0.015+0.985*pow(front,0.6);
  alpha*=faceWeight*(0.35+light*0.7);
  // Signal events lift only the relevant region of the manifold.
  float pressure=energy*0.02*sin(b*12.0-t*0.7);
  p+=normalize(vec3(p.xy,p.z+0.1))*pressure;
  p.x+=pow(front,2.0)*(blend-.5)*0.025;
  p.y+=gauss(f-vec2(-.3,-.15),vec2(.25,.3))*low_a*.025;
  p.y-=gauss(f-vec2(.3,-.15),vec2(.25,.3))*low_b*.025;
 }else if(id<112000){
  int n=id-96000;float u=float(n%4000)/4000.0;float lane=float(n/4000);
  float a=u*TAU+t*(0.025+lane*.007)*(.3+motion*1.6);
  float radius=.88+lane*.045;
  p=vec3(cos(a)*radius,sin(a)*radius*.65,sin(a*2.0+t*.065)*.32);
  p.xy=rot(lane*.31-.4)*p.xy;
  p.yz=rot(sin(t*.035+lane)*.35)*p.yz;
  side=cos(a);light=1.0;alpha=.28+.48*pow(.5+.5*sin(a*4.0-t*.7+lane),6.0);
 }else{
  int n=id-112000;float u=float(n%4000)/3999.0;float lane=float(n/4000);
  float x=(u-.5)*3.0;
  p=vec3(x,-.75+.025*lane+sin(x*(5.0+lane*.21)-t*(.22+lane*.014))*(.06+energy*.025),.05+sin(x*2.0+lane*.2)*.12);
  side=x;light=.65;alpha=.12+pow(.5+.5*cos(x*8.0-t*.5+lane),8.0)*.45;
 }
 // Interpret the director's actual section/scene clock, not a clip timeline.
 float arrival=section>0.5&&section<1.5?smoothstep(0.0,.22,chapter):1.0;
 float drop=section>3.5&&section<4.5?exp(-chapter*22.0):0.0;
 p*=1.0+drop*.12;
 if(arrival<1.0){p.xz=rot((1.0-arrival)*.55)*p.xz;alpha*=.25+.75*arrival;}
 if(handover>=0.0){p.xz=rot(side*(handover-.5)*.16)*p.xz;}
 // Independent optical motion and real perspective. No short animation clip.
 p.xz=rot(sin(t*.085+seed)*.19+(blend-.5)*.15)*p.xz;
 p.yz=rot(sin(t*.063+seed*2.0)*.07)*p.yz;
 float depth=3.0-p.z;
 vec2 screen=p.xy*(1.48/depth);
 screen.x/=resolution.x/resolution.y;
 gl_Position=vec4(screen*2.0,0.0,1.0);
 gl_PointSize=(1.35+light*1.1)*(resolution.y/720.0)*(2.8/depth);
 vec3 ice=mix(vec3(.32,.92,1.0),vec3(ar,ag,ab),.30),pearl=mix(vec3(.88,.82,1.0),vec3(br,bg,bb),.25),warm=vec3(1.0,.63,.42);
 color=mix(ice,pearl,smoothstep(-.5,.4,side));
 color=mix(color,warm,smoothstep(.1,.8,side)*.35);
 color*=1.0+light*1.2;
 opacity=alpha*(.55+.30*density);
}
