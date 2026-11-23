uniform vec2 resolution;
uniform vec3 colour_a,colour_b,bands;
uniform float clock_s,scene_beat,handover,motion,density,section,chapter,layer_opacity,kick,point_step;
out vec3 color;
out float opacity;
// PATCH_SURFACE
float patch_hash(float x){return fract(sin(x*127.1)*43758.5453);}
// Each deck's signal as ribbons flowing in from its own side of the screen
// toward the DJ (the reference, 2026-10-26): four strands, the waveform
// moving with the deck's own beat, shaped by its level, EQ and FX. In a
// real handover the two reach across behind the DJ and cross.
void main(){
 int id=gl_VertexID*int(point_step),source=id/4096,n=id%4096;
 float u=float(n%1024)/1023.0,lane=float(n/1024);
 vec4 s=source_state[source],eq=source_tone[source];
 float side=source_count<2?-1.0:float(source)/float(source_count-1)*2.0-1.0;
 float overlap=handover<0.0?0.0:sin(handover*3.14159265);
 float flow=s.y<0.0?clock_s*.5:s.y*1.5707963;
 float low=max(0.0,1.0+eq.x),mid=max(0.0,1.0+eq.y),high=max(0.0,1.0+eq.z);
 // From the screen's edge (u = 0) to just behind the DJ (u = 1); a handover
 // carries it across to the other side.
 float inner=mix(.26,-.4,overlap);
 float x=side*mix(3.2,inner,u);
 float level=s.x*(.35+.65*clamp(s.z,0.0,1.0));
 float amp=(.02+level*.07)*(.6+bands.x*.8*low);
 float y=.44+lane*.012-.018;
 y+=sin(x*2.2*side-flow+lane*.9)*amp;
 y+=sin(x*7.0*side-flow*2.0+lane*1.7)*amp*.35*bands.y*mid;
 // The waveform's strand: spikes from the deck's level, like a scope.
 if(lane>2.5){
  float spike=(patch_hash(floor(u*360.0)+floor(flow*4.0))-.5)*2.0;
  y+=spike*level*(.03+bands.z*.09*high)*smoothstep(.0,.3,u);
 }
 y+=sin(u*31.0-flow*3.0)*source_fx[source]*.05;
 float z=-.40-lane*.015+sin(x*1.3+flow*.5)*.04;
 vec3 p=vec3(x,y,z);
 gl_Position=patch_clip(p,resolution);
 float pulse=s.y<0.0?0.0:exp(-fract(s.y)*9.0)*kick;
 gl_PointSize=(2.0+pulse*1.6+level*1.2)*(resolution.y/1080.0);
 color=source==0?colour_a:colour_b;
 float ends=smoothstep(0.0,.12,u)*smoothstep(1.0,.82,u);
 float thread=lane>2.5?.9:.45+.55*pow(.5+.5*sin(u*60.0-flow*2.0+lane),6.0);
 opacity=min(1.0,layer_opacity*s.w*ends*(.35+level*1.3)*thread*(1.0+pulse*.6));
}
