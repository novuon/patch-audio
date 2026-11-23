uniform vec2 resolution;
uniform float scene_beat,handover,section,chapter,energy;
out vec2 v_uv;
flat out float j_back;
// PATCH_SURFACE
// The filmed DJ on one quad: his head at the same scale as the drawn DJ, the
// console's line in the film on the booth's back edge, at his belt. FILM_PER_METRE
// matches the drawn DJ's height: both sized against the booth, not true metres.
const vec2 FILM=vec2(768.0,704.0);
const float FILM_CONSOLE=596.0,FILM_PER_METRE=425.0;
void main(){
 PatchPose o=patch_pose(scene_beat,energy,handover,section,chapter);
 vec2 s=resolution;
 vec2 chest=patch_pixel(o.chest,s);
 float unit=length(patch_pixel(o.chest+vec3(.1,0,0),s)-chest)*10.0;
 float px=unit/FILM_PER_METRE;
 float cut=patch_pixel(vec3(0,0,-.20),s).y;
 float x=patch_pixel(o.waist,s).x;
 vec4 b=patch_clip(o.chest,s);
 j_back=b.z/b.w*.5+.5;
 vec2 corner[6]=vec2[6](vec2(0,0),vec2(1,0),vec2(0,1),vec2(0,1),vec2(1,0),vec2(1,1));
 vec2 c=corner[gl_VertexID%6];
 vec2 size=FILM*px;
 vec2 p=vec2(x-size.x*.5,cut-(FILM.y-FILM_CONSOLE)*px)+size*c;
 // The groove moves him as it moves the drawn DJ: the sway above, a bounce
 // and a lean around where he meets the console.
 p.y-=body0.w*unit*.6;
 float lean=body0.y*.12;
 vec2 at=vec2(x,cut),q=p-at;
 p=at+vec2(q.x*cos(lean)-q.y*sin(lean),q.x*sin(lean)+q.y*cos(lean));
 v_uv=vec2(c.x,1.0-c.y);
 gl_Position=vec4(p/s*2.0-1.0,0.0,1.0);
}
