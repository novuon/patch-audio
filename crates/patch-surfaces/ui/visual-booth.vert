uniform vec2 resolution;
flat out int part_id;
out vec3 local_position,world_position,surface_normal;
// PATCH_SURFACE
vec3 cube_corner(int face,int corner){
 vec2 q[6]=vec2[6](vec2(-1,-1),vec2(1,-1),vec2(-1,1),vec2(-1,1),vec2(1,-1),vec2(1,1));
 vec2 p=q[corner];
 if(face==0)return vec3(p.x,1,p.y);if(face==1)return vec3(p.x,-1,-p.y);
 if(face==2)return vec3(p.x,p.y,1);if(face==3)return vec3(-p.x,p.y,-1);
 if(face==4)return vec3(1,p.y,-p.x);return vec3(-1,p.y,p.x);
}
// The booth, seen from the audience, laid out as a real controller is
// used: the DJ's near side is the far side from us (−z), the DJ's left is
// our right (+x). The mixer: knobs from the far edge (HIGH toward us) down
// to the filter, CUE buttons, channel faders going up toward us, the
// crossfader nearest the DJ, the level meters in the middle. Each deck: a
// waveform screen at its far edge, the jog, eight performance pads nearest
// the DJ, play / cue / sync at the DJ's left, the pitch fader at the right.
void booth_part(int part,out vec3 c,out vec3 h,out bool cylinder){
 cylinder=false;c=vec3(0);h=vec3(0);
 // Each pair's first part is deck A's side (screen left), its second B's.
 int first=part<=5?4:part<=7?6:part<=9?8:part<=12?11:part<=26?25:part<=28?27:part<=30?29:part<=32?31:part<=34?33:part<=36?35:part<=39?38:part<=41?40:part<=43?42:part<=45?44:46;
 float sx=(part-first)%2==0?-1.0:1.0;
 float dx=sx*PATCH_DECK_X;
 if(part==0){c=vec3(0,-.03,.09);h=vec3(8.0,.03,.29);}
 else if(part==1){c=vec3(0,-1.2,.37);h=vec3(8.0,1.17,.012);}
 else if(part==2){c=vec3(0,-.004,.385);h=vec3(8.0,.006,.006);}
 else if(part==3){c=vec3(0,-.25,.385);h=vec3(0);}
 else if(part<=5){c=vec3(dx,.006,.02);h=vec3(PATCH_DECK_W,.006,.27);}
 else if(part<=7){c=vec3(dx,.022,PATCH_DECK_Z);h=vec3(PATCH_PLATTER_R,.012,PATCH_PLATTER_R);cylinder=true;}
 else if(part<=9){c=vec3(dx,.036,PATCH_DECK_Z);h=vec3(.01,.008,.01);cylinder=true;}
 else if(part==10){c=vec3(0,.025,.01);h=vec3(PATCH_MIXER_W,.025,.21);}
 else if(part<=12){c=vec3(sx*PATCH_STRIP_X,.0505,PATCH_FADER_Z);h=vec3(.003,.001,PATCH_FADER_TRAVEL*.5+.01);}
 else if(part<=14){
  int i=part-13;float gain=source_count>i?source_gain[i]:0.0;
  c=vec3((i==0?-1.0:1.0)*PATCH_STRIP_X,.058,PATCH_FADER_Z+(gain-.5)*PATCH_FADER_TRAVEL);h=vec3(.018,.008,.009);
 }
 else if(part==15){c=vec3(0,.0505,PATCH_XFADER_Z);h=vec3(PATCH_XFADER_TRAVEL*.5+.01,.001,.003);}
 else if(part==16){c=vec3((crossfader-.5)*PATCH_XFADER_TRAVEL,.058,PATCH_XFADER_Z);h=vec3(.009,.008,.018);}
 else if(part<=24){
  int i=(part-17)%2,row=(part-17)/2;
  float r=row==3?.016:.013;
  c=vec3((i==0?-1.0:1.0)*PATCH_STRIP_X,.058,PATCH_KNOB_Z0-float(row)*PATCH_KNOB_DZ);h=vec3(r,.008,r);cylinder=true;
 }
 else if(part<=26){c=vec3(sx*.017,.0505,.115);h=vec3(.009,.001,.078);}
 else if(part<=28){c=vec3(sx*.19,.054,PATCH_XFADER_Z);h=vec3(.013,.004,.013);}
 else if(part<=30){c=vec3(dx+.201,.016,-.205);h=vec3(.018,.004,.018);cylinder=true;}
 else if(part<=32){c=vec3(dx-.203,.0125,.02);h=vec3(.003,.001,.12);}
 else if(part<=34){c=vec3(dx-.203,.018,.02);h=vec3(.015,.006,.008);}
 else if(part<=36){c=vec3(sx*(PATCH_DECK_X+PATCH_DECK_W+.41),.018,.06);h=vec3(.40,.018,.24);}
 else if(part==37){c=vec3(0,.0512,.205);h=vec3(PATCH_MIXER_W-.01,.001,.003);}
 else if(part<=39){c=vec3(dx,.0125,.255);h=vec3(.116,.001,.02);}
 else if(part<=41){c=vec3(sx*PATCH_STRIP_X,.054,-.002);h=vec3(.013,.004,.008);}
 else if(part<=43){c=vec3(dx-.018,.0135,-.21);h=vec3(.151,.0015,.026);}
 else if(part<=45){c=vec3(dx+.201,.016,-.145);h=vec3(.018,.004,.018);cylinder=true;}
 else {c=vec3(dx-.203,.016,-.17);h=vec3(.016,.004,.011);}
}
void main(){
 int part=gl_VertexID/PATCH_BOOTH_VERTICES,id=gl_VertexID%PATCH_BOOTH_VERTICES;
 vec3 centre,scale;bool cylinder;booth_part(part,centre,scale,cylinder);
 vec3 local,n;
 if(cylinder){
  int slice=id/12,c=id%12;float a=float(slice)*6.2831853/16.0,b=float(slice+1)*6.2831853/16.0;
  vec3 at=vec3(cos(a),1,sin(a)),bt=vec3(cos(b),1,sin(b));
  vec3 ab=vec3(at.x,-1,at.z),bb=vec3(bt.x,-1,bt.z);
  if(c==0){local=vec3(0,1,0);}else if(c==1){local=at;}else if(c==2){local=bt;}
  else if(c==3){local=vec3(0,-1,0);}else if(c==4){local=bb;}else if(c==5){local=ab;}
  else if(c==6){local=at;}else if(c==7){local=ab;}else if(c==8){local=bt;}
  else if(c==9){local=bt;}else if(c==10){local=ab;}else{local=bb;}
  n=c<3?vec3(0,1,0):c<6?vec3(0,-1,0):normalize(vec3(local.x,0,local.z));
 }else{
  if(id>=36){gl_Position=vec4(2,2,2,1);part_id=part;local_position=vec3(0);world_position=vec3(0);surface_normal=vec3(0,1,0);return;}
  int face=id/6;local=cube_corner(face,id%6);
  n=face==0?vec3(0,1,0):face==1?vec3(0,-1,0):face==2?vec3(0,0,1):face==3?vec3(0,0,-1):face==4?vec3(1,0,0):vec3(-1,0,0);
 }
 part_id=part;local_position=local;world_position=centre+scale*local;surface_normal=n;
 gl_Position=patch_clip(world_position,resolution);
}
