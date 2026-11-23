// The booth and the DJ, shared by the Visualiser's layer programs.
// No mesh/video assets; everything follows the shared musical/mixer snapshot.
//
// World, in metres: y up, x to the right of the screen, z toward the
// audience (the camera). The booth top is y = 0 (about hip height), its
// front edge z = .38. The DJ stands behind it facing the audience, at real
// human scale next to a controller-sized pair of decks and a mixer. Deck A
// is on screen left (-x).
uniform vec4 source_state[8],source_tone[8];
uniform float source_fx[8],source_gain[8],source_progress[8],crossfader;
uniform vec2 source_meter[8];
uniform int source_count;
// The body, eased in Rust (patch_visual::groove): nod, tilt, sway, bounce;
// look, lean, shrug A, shrug B; gestures; its steady beat clock.
uniform vec4 body0,body1,gesture,gesture2;
uniform float body_beat;
const int PATCH_BOOTH_PARTS=48;
const int PATCH_BOOTH_VERTICES=192;
// Proportioned against the filmed DJ (2026-11-09): a club mixer about two
// thirds of his shoulders wide with its channels under his resting hands,
// the decks either side, about a metre across in all as real gear is.
const float PATCH_MIXER_W=.25,PATCH_DECK_W=.24;
const float PATCH_DECK_X=.50,PATCH_DECK_Z=.04,PATCH_PLATTER_R=.17;
const float PATCH_STRIP_X=.16;
// The mixer as the DJ reads it, from the far edge (toward the audience)
// to the near: HIGH, MID, LOW, FILTER, the CUE button, the channel fader,
// the crossfader nearest the DJ.
const float PATCH_KNOB_Z0=.175,PATCH_KNOB_DZ=.045;
const float PATCH_FADER_Z=-.075,PATCH_FADER_TRAVEL=.08;
const float PATCH_XFADER_Z=-.165,PATCH_XFADER_TRAVEL=.10;
const float PATCH_OPERATOR_Z=-.26;
const float PATCH_FOV_T=.3443; // tan(19°)
const vec3 PATCH_TARGET=vec3(0,.45,-.04);
// A lens shift: the whole picture lower on screen, same size and
// perspective, the booth's front at the window's bottom edge.
const float PATCH_SHIFT=-.30;
const vec3 PATCH_VIEW=vec3(0,.22,1.0);
// The DJ, a tall man, with his head and a raised fist in frame; further back only when a narrow
// window would crop the decks.
float patch_distance(vec2 size){
 float aspect=size.x/max(size.y,1.0);
 return max(2.65,1.0/(PATCH_FOV_T*aspect));
}
vec3 patch_eye(vec2 size){return PATCH_TARGET+normalize(PATCH_VIEW)*patch_distance(size);}
vec4 patch_clip(vec3 p,vec2 size){
 float aspect=size.x/max(size.y,1.0);
 vec3 eye=patch_eye(size);
 vec3 f=normalize(PATCH_TARGET-eye),r=normalize(cross(f,vec3(0,1,0))),u=cross(r,f);
 vec3 q=p-eye;float z=dot(q,f);
 const float n=.25,far=14.0;
 return vec4(dot(q,r)/(PATCH_FOV_T*aspect),dot(q,u)/PATCH_FOV_T+PATCH_SHIFT*z,z*(far+n)/(far-n)-2.0*far*n/(far-n),z);
}
// A world point in window pixels.
vec2 patch_pixel(vec3 p,vec2 size){vec4 c=patch_clip(p,size);return (c.xy/c.w*.5+.5)*size;}
float patch_balance(){
 float a=source_count>0?source_state[0].z*source_state[0].w:0.0;
 float b=source_count>1?source_state[1].z*source_state[1].w:0.0;
 return (b-a)/max(a+b,.01);
}
// A smooth pulse on each beat, no corners (the motion never snaps).
float patch_accent(float beat){float c=.5+.5*cos(beat*6.2831853);return c*c;}
// One control on this side, on the surface. 0..2 EQ, 3 filter, 4 cue,
// 5 channel fader, 6 the deck's pads (nearest the DJ, not the jog).
vec3 patch_control(int which,float sx,float gain){
 if(which<=0)return vec3(sx*PATCH_STRIP_X,.075,PATCH_KNOB_Z0);
 if(which==1)return vec3(sx*PATCH_STRIP_X,.075,PATCH_KNOB_Z0-PATCH_KNOB_DZ);
 if(which==2)return vec3(sx*PATCH_STRIP_X,.075,PATCH_KNOB_Z0-2.0*PATCH_KNOB_DZ);
 if(which==3)return vec3(sx*PATCH_STRIP_X,.075,PATCH_KNOB_Z0-3.0*PATCH_KNOB_DZ);
 if(which==4)return vec3(sx*PATCH_STRIP_X,.062,-.002);
 if(which==5)return vec3(sx*PATCH_STRIP_X,.070,PATCH_FADER_Z+(gain-.5)*PATCH_FADER_TRAVEL);
 return vec3(sx*(PATCH_DECK_X-.08),.060,-.20);
}
// Where a hand is. The right hand holds the crossfader while a deck plays.
// The other hand walks that side's knobs, fader, cue and the deck's pads.
// Nothing lifts: a rise of one hand and then the other read as a yawn.
// Every term is continuous, so the hand glides along the surface.
vec3 patch_hand(int side,float beat,float handover){
 int i=side<0?0:1;float sx=float(side);
 float on_a=source_count>0?source_state[0].w:0.0;
 float on_b=source_count>1?source_state[1].w:0.0;
 float any_on=max(on_a,on_b);
 vec4 eq=source_count>i?source_tone[i]:vec4(0.0);
 float gain=source_count>i?source_gain[i]:0.0;
 float mixing=handover<0.0?0.0:sin(handover*3.14159265);
 vec3 xf=vec3((crossfader-.5)*PATCH_XFADER_TRAVEL,.070,PATCH_XFADER_Z);
 // On the cap. A finger presses in on the beat; the hand does not rise.
 if(side>0&&any_on>0.5){xf.y-=.003*patch_accent(beat);return xf;}
 float slot=floor(beat*.125);
 float glide=smoothstep(0.0,0.45,fract(beat*.125));
 float h0=fract(sin((slot+sx*3.0)*12.9898)*43758.5453);
 float h1=fract(sin((slot+1.0+sx*3.0)*12.9898)*43758.5453);
 vec3 walked=mix(patch_control(int(floor(h0*7.0)),sx,gain),patch_control(int(floor(h1*7.0)),sx,gain),glide);
 vec3 aw=eq.xyz*eq.xyz+1e-4;
 float band=(aw.y+2.0*aw.z)/(aw.x+aw.y+aw.z);
 float eq_work=clamp(max(abs(eq.x),max(abs(eq.y),abs(eq.z)))*2.0,0.0,1.0);
 float filter_work=clamp(abs(eq.w)*2.0,0.0,1.0);
 float fx_work=source_count>i?clamp(source_fx[i]*2.0,0.0,1.0):0.0;
 vec3 target=walked;
 target=mix(target,patch_control(int(clamp(floor(band+.5),0.0,2.0)),sx,gain),eq_work);
 target=mix(target,patch_control(3,sx,gain),filter_work*(1.0-eq_work));
 target=mix(target,vec3(sx*.115,.065,-.165),fx_work*(1.0-eq_work));
 target=mix(target,patch_control(5,sx,gain),mixing*.85);
 // Nothing playing: both hands rest on the decks' near controls.
 if(any_on<0.5){target=patch_control(6,sx,gain);target.x+=sx*.02*sin(beat*.7);}
 // A visit to the pads, still on the surface, never a hand on the platter.
 target=mix(target,patch_control(6,sx,gain),gesture2.w*(1.0-max(eq_work,filter_work))*(1.0-mixing));
 target.y-=.003*patch_accent(beat);
 return target;
}
// The DJ's skeleton, in metres from the hips: measured from reference photos
// of a fit man, to the 0.224 m head. One set of bone lengths for every pose:
// upper arm 0.30, forearm 0.25, hand 0.18. As the arm rises the whole shoulder
// lifts and moves in with it, so there is only ever one shoulder.
// The drawn DJ's size against the filmed one, so the three performers stand as
// one: everything below is scaled by it about the console's top. The booth is
// drawn larger than real decks, so at true metres he looked short beside it;
// both performers are scaled up together until his shoulders are wider than a deck.
const float PATCH_BODY=1.53;
// Where his waist is against the console's top.
const float PATCH_WAIST=0.0;
const vec2 PATCH_SHOULDER=vec2(.175,.50)*PATCH_BODY,PATCH_SHOULDER_UP=vec2(.165,.575)*PATCH_BODY;
// The upper arm about as long as the forearm, as on the filmed DJ.
const float PATCH_UPPER_ARM=.27*PATCH_BODY,PATCH_FOREARM=.26*PATCH_BODY;
vec3 patch_reach(vec3 shoulder,vec3 wrist){
 vec3 delta=wrist-shoulder;float d=length(delta),most=(PATCH_UPPER_ARM+PATCH_FOREARM)*.95;
 return d>most?shoulder+delta*(most/d):wrist;
}
// Two-bone arm. At the console the elbow leans out and forward, toward the
// controls; as the arm rises it swings out and under the arm.
vec3 patch_elbow(vec3 shoulder,vec3 wrist,float side,float lift){
 const float l1=PATCH_UPPER_ARM,l2=PATCH_FOREARM;
 vec3 delta=wrist-shoulder;float d=clamp(length(delta),.1,l1+l2-.005);vec3 axis=normalize(delta);
 float along=(l1*l1-l2*l2+d*d)/(2.0*d);float height=sqrt(max(l1*l1-along*along,0.0));
 vec3 pole=mix(vec3(side,-.2,-.3),vec3(side,-.3,-.1),lift);pole=normalize(pole-axis*dot(pole,axis));
 return shoulder+axis*along+pole*height;
}
// Where a gesture puts the wrist (x outward from the body's centre, y up
// from the waist, z toward the audience), the hand's direction on screen,
// how far it curls (0 open, 1 a fist), whether the index points.
struct PatchArm{vec3 wrist;vec2 dir;float curl;float index;};
const PatchArm ON_EAR=PatchArm(vec3(.19,.69,.20)*PATCH_BODY,vec2(-.45,1.0),.35,0.0);
const PatchArm ON_AIR=PatchArm(vec3(.39,.80,.0)*PATCH_BODY,vec2(-.02,1.0),1.0,0.0);
const PatchArm ON_POINT=PatchArm(vec3(.62,.68,.05)*PATCH_BODY,vec2(.86,.52),1.0,1.0);
const PatchArm ON_UP=PatchArm(vec3(.10,.96,.03)*PATCH_BODY,vec2(-.35,1.0),0.0,0.0);
PatchArm patch_arm_mix(PatchArm a,PatchArm b,float t){
 return PatchArm(mix(a.wrist,b.wrist,t),mix(a.dir,b.dir,t),mix(a.curl,b.curl,t),mix(a.index,b.index,t));
}
// The DJ's joints: where the silhouette is drawn from; the body's motion
// comes eased from Rust (body0/body1).
struct PatchPose {vec3 chest;vec3 waist;vec3 shoulder[2];vec3 elbow[2];vec3 wrist[2];PatchArm arm[2];float work[2];float on_desk[2];float lift[2];float yaw;};
PatchPose patch_pose(float beat,float energy,float handover,float section,float chapter){
 PatchPose o;
 float balance=patch_balance();
 vec3 root=vec3(balance*.02+body0.z,PATCH_WAIST-body0.w-body1.y*.02,PATCH_OPERATOR_Z);
 o.yaw=balance*.3;
 o.chest=root+vec3(0,.40,0);
 o.waist=root;
 for(int i=0;i<2;i++){
  float side=i==0?-1.0:1.0;
  float ear=i==0?gesture.x:gesture.y,air=i==0?gesture.z:gesture.w;
  float point=i==0?gesture2.y:gesture2.z,clap=gesture2.x;
  float shrug=i==0?body1.z:body1.w;
  // At the console the hand lies on its real control, the wrist just
  // behind it; deck → mixer turns the hand inward.
  vec3 control=patch_hand(i==0?-1:1,body_beat,handover)+vec3(0,.02,-.06);
  vec3 from_root=control-root;from_root.x*=side;
  o.work[i]=clamp((.30-from_root.x)/.22,0.0,1.0);
  PatchArm a=PatchArm(from_root,mix(vec2(.30,-1.0),vec2(-.55,-1.0),o.work[i]),mix(.25,.35,o.work[i]),0.0);
  a=patch_arm_mix(a,ON_EAR,ear);a=patch_arm_mix(a,ON_AIR,air);
  a=patch_arm_mix(a,ON_POINT,point);a=patch_arm_mix(a,ON_UP,clap);
  o.on_desk[i]=1.0-min(1.0,ear+air+point+clap);
  // How far the arm is up, from where the elbow would be: down at the
  // console 0, level about a half, overhead 1. The shoulder follows.
  vec3 wrist=root+a.wrist*vec3(side,1,1);
  vec3 low=root+vec3(side*PATCH_SHOULDER.x,PATCH_SHOULDER.y,0);
  // The shoulder lifts with an arm raised in a gesture; at the console the
  // elbows stay down and close.
  o.lift[i]=smoothstep(0.0,1.0,1.0-o.on_desk[i]);
  vec2 joint=mix(PATCH_SHOULDER,PATCH_SHOULDER_UP,o.lift[i]);
  o.shoulder[i]=root+vec3(side*joint.x,joint.y+shrug,0);
  o.wrist[i]=patch_reach(o.shoulder[i],wrist);
  o.elbow[i]=patch_elbow(o.shoulder[i],o.wrist[i],side,o.lift[i]);
  o.arm[i]=a;
 }
 return o;
}
