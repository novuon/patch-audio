uniform vec2 resolution;
uniform float scene_beat,handover,section,chapter,energy;
// The DJ's joints in window pixels, each hand's direction (x outward),
// curl and pointing, and a metre in pixels.
flat out vec2 j_waist,j_sh[2],j_el[2],j_wr[2],j_dir[2],j_hand[2];
flat out float j_work[2],j_desk[2],j_lift[2],j_unit,j_yaw,j_back;
// PATCH_SURFACE
void main(){
 PatchPose o=patch_pose(scene_beat,energy,handover,section,chapter);
 vec2 s=resolution;
 j_waist=patch_pixel(o.waist,s);
 for(int i=0;i<2;i++){
  j_sh[i]=patch_pixel(o.shoulder[i],s);j_el[i]=patch_pixel(o.elbow[i],s);j_wr[i]=patch_pixel(o.wrist[i],s);
  j_dir[i]=o.arm[i].dir;j_hand[i]=vec2(o.arm[i].curl,o.arm[i].index);
  j_work[i]=o.work[i];j_desk[i]=o.on_desk[i];j_lift[i]=o.lift[i];
 }
 vec2 chest=patch_pixel(o.chest,s);
 j_unit=length(patch_pixel(o.chest+vec3(.1,0,0),s)-chest)*10.0*PATCH_BODY;
 j_yaw=o.yaw;
 // Depth: the body stands at the DJ's distance, so the booth in front
 // hides it (the arms and hands lie on top of the controls).
 vec4 b=patch_clip(o.chest,s);
 j_back=b.z/b.w*.5+.5;
 // One quad around the figure, raised arms included.
 vec2 lo=j_waist-vec2(.95,.12)*j_unit,hi=j_waist+vec2(.95,1.30)*j_unit;
 vec2 corner[6]=vec2[6](vec2(0,0),vec2(1,0),vec2(0,1),vec2(0,1),vec2(1,0),vec2(1,1));
 gl_Position=vec4(mix(lo,hi,corner[gl_VertexID%6])/s*2.0-1.0,0.0,1.0);
}
