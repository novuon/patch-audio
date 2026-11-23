//! Operator-owned scene library and show preferences. The existing Director
//! remains the only musical scheduler; browsing never changes its title or
//! real mix timing. All selections have stable names, not UI-specific numbers.
use crate::{director::Mood,generative::Mode,stage::{Act,Backdrop}};
use serde::{Deserialize,Serialize};
use std::collections::{BTreeMap,BTreeSet};

#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum Collection {Patch,Ambient,Generative,Effects}
impl Collection {
    pub const ALL:[Self;4]=[Self::Patch,Self::Ambient,Self::Generative,Self::Effects];
    pub fn label(self)->&'static str {match self {Self::Patch=>"Patch",Self::Ambient=>"Ambient",Self::Generative=>"Shaders",Self::Effects=>"Titles"}}
}
/// Which collections play, in any combination: the same choice governs
/// Space, the scene list and the automatic choreographer.
#[derive(Clone,Copy,Debug,PartialEq,Eq,Serialize,Deserialize)]
#[serde(default)]
pub struct Sources {pub patch:bool,pub ambient:bool,pub shaders:bool,pub titles:bool}
impl Default for Sources {fn default()->Self {Self {patch:true,ambient:true,shaders:false,titles:true}}}
impl Sources {
    pub fn includes(&self,c:Collection)->bool {match c {Collection::Patch=>self.patch,Collection::Ambient=>self.ambient,Collection::Generative=>self.shaders,Collection::Effects=>self.titles}}
    pub fn flags(&self)->[bool;4] {Collection::ALL.map(|c|self.includes(c))}
    /// Toggle one collection. At least one picture source (Patch, Ambient or
    /// Shaders) always stays on: titles alone have nothing to sit on.
    pub fn toggle(&mut self,c:Collection) {
        let mut next=*self;
        match c {Collection::Patch=>next.patch^=true,Collection::Ambient=>next.ambient^=true,Collection::Generative=>next.shaders^=true,Collection::Effects=>next.titles^=true}
        if next.patch || next.ambient || next.shaders {*self=next;}
    }
    pub fn active(&self,entries:&[Entry])->Vec<Entry> {entries.iter().filter(|e|self.includes(e.collection)).cloned().collect()}
}
#[derive(Clone,Debug,PartialEq)]
pub enum Material {Grid(Backdrop),Shader(String),Effect(Act)}
#[derive(Clone,Debug,PartialEq)]
pub struct Entry {pub id:String,pub name:String,pub collection:Collection,pub material:Material}
fn title(name:&str)->String {name.split(['-','_',':']).filter(|s|!s.is_empty()).map(|s| {let mut c=s.chars();c.next().map(|v|v.to_uppercase().collect::<String>()+c.as_str()).unwrap_or_default()}).collect::<Vec<_>>().join(" ")}
impl Entry {
    fn grid(kind:Backdrop)->Self {Self {id:format!("cells:{}",kind.name()),name:title(kind.name()),collection:if kind.ambient(){Collection::Ambient}else{Collection::Patch},material:Material::Grid(kind)}}
}
/// Old and Ambient worlds interleave: from Sunset, Space reaches an
/// old Patch visual immediately instead of traversing thirty more new ones.
pub fn native_library()->Vec<Entry> {
    let mut entries=Vec::new();
    for i in 0..Backdrop::ALL.len().max(Backdrop::AMBIENT.len()) {
        if let Some(&kind)=Backdrop::ALL.get(i) {entries.push(Entry::grid(kind));}
        if let Some(&kind)=Backdrop::AMBIENT.get(i) {entries.push(Entry::grid(kind));}
    }
    entries
}
/// Called at setup, not in rendering. Includes user-installed ISF files.
pub fn library()->Vec<Entry> {
    let mut out=native_library();
    // Patch's own generators first, by plain name; the user's ISF files and
    // the older Hydra sketches after them, marked.
    let builtin:Vec<&str>=crate::isf::builtin().into_iter().map(|(n,_)|n).collect();
    for name in crate::isf::scenes() {
        let shown=if builtin.contains(&name.as_str()) {title(&name)} else {format!("ISF · {}",title(&name))};
        out.push(Entry {id:format!("shader:isf:{name}"),name:shown,collection:Collection::Generative,material:Material::Shader(format!("isf:{name}"))});
    }
    for (name,_) in crate::generative::presets() {out.push(Entry {id:format!("shader:{name}"),name:format!("Hydra · {}",title(name)),collection:Collection::Generative,material:Material::Shader(name.into())});}
    for &act in Act::ALL {out.push(Entry {id:format!("effect:{}",act.name()),name:title(act.name()),collection:Collection::Effects,material:Material::Effect(act)});}
    out
}
pub const PARTS:[Mood;7]=[Mood::Idle,Mood::Reveal,Mood::Groove,Mood::Lift,Mood::Calm,Mood::Handover,Mood::Drop];
pub fn part(mood:Mood)->usize {PARTS.iter().position(|v|*v==mood).unwrap_or(2)}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(default)]
pub struct Rule {
    pub weight:f32,
    /// Idle, reveal, groove, build, breakdown, mix, drop.
    pub sections:[bool;7],
    /// Strength of each scene's local beat/spectrum response.
    pub response:f32,
    pub beat:f32,pub motion:f32,pub accent:f32,
}
impl Default for Rule {fn default()->Self {Self {weight:1.0,sections:[true;7],response:1.0,beat:1.0,motion:1.0,accent:1.0}}}
#[derive(Clone,Copy,Debug,PartialEq,Serialize,Deserialize)]
#[serde(default)]
pub struct Timing {pub min_bars:u32,pub max_bars:u32,pub motion:f32,pub intensity:f32}
impl Default for Timing {fn default()->Self {Self {min_bars:8,max_bars:24,motion:1.0,intensity:1.0}}}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum Pulse {Off,#[default] Occasional,Drops,EveryBeat}
#[derive(Clone,Copy,Debug,PartialEq,Serialize,Deserialize)]
#[serde(default)]
pub struct Accents {
    pub pulse:Pulse,pub strength:f32,pub spacing_bars:u32,
    pub bump:bool,pub split:bool,pub shake:bool,
}
impl Default for Accents {fn default()->Self {Self {pulse:Pulse::Occasional,strength:0.45,spacing_bars:16,bump:false,split:false,shake:false}}}
#[derive(Clone,Copy,Debug,Default,PartialEq,Eq,Serialize,Deserialize)]
#[serde(rename_all="snake_case")]
pub enum PaletteMode {#[default] Original,Director,Theme}
#[derive(Clone,Copy,Debug,PartialEq,Serialize,Deserialize)]
#[serde(default)]
pub struct LayerPlan {
    /// 0 follows the layer's own switch; 1 shows; 2 hides.
    pub booth:u8,pub performer:u8,pub opacity:f32,
    /// Silhouette, dots, filmed Me. Equal weights retain the user's existing
    /// tested automatic performer choreography, including its 16-bar cycle.
    pub looks:[f32;3],
}
impl Default for LayerPlan {fn default()->Self {Self {booth:0,performer:0,opacity:1.0,looks:[1.0;3]}}}
#[derive(Clone,Debug,PartialEq,Serialize,Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// 0 weighted variety, 1 sequential, 2 the track's own cast.
    pub order:u8,
    /// -1 uses the track palette, otherwise an explicit palette index.
    pub palette_index:i32,
    pub automatic:bool,
    pub visuals:BTreeMap<String,Rule>,
    /// Scenes switched off: neither the show nor Space plays them (a pick
    /// from the list still does); switched on again they return as they were.
    pub off:BTreeSet<String>,
    /// Off: the Director's own passage lengths (2/4/8 bars by mix length).
    pub custom_lengths:bool,
    pub timing:[Timing;7],
    pub accents:Accents,
    /// Diagonal wipe, radial opening, slats, dissolve.
    pub transitions:[bool;4],
    pub transition_beats:f32,
    pub overlays:bool,
    pub title_gap_bars:u32,
    pub palette:PaletteMode,
    pub shader_mode:Mode,
    pub layers:[LayerPlan;7],
}
impl Default for Preferences {
    fn default()->Self {
        let mut timing=[Timing::default();7];
        timing[0]=Timing {min_bars:8,max_bars:16,motion:1.0,intensity:1.0};
        timing[1]=Timing {min_bars:8,max_bars:8,..Timing::default()};
        timing[3]=Timing {min_bars:4,max_bars:8,..Timing::default()};
        timing[4]=Timing {min_bars:8,max_bars:32,..Timing::default()};
        timing[6]=Timing {min_bars:4,max_bars:4,..Timing::default()};
        Self {order:0,palette_index:-1,automatic:true,visuals:BTreeMap::new(),off:BTreeSet::new(),custom_lengths:false,timing,accents:Accents::default(),transitions:[true;4],transition_beats:2.0,overlays:true,title_gap_bars:4,palette:PaletteMode::Original,shader_mode:Mode::Hybrid,layers:[LayerPlan::default();7]}
    }
}
fn finite(v:f32,low:f32,high:f32,fallback:f32)->f32 {if v.is_finite(){v.clamp(low,high)}else{fallback}}
impl Preferences {
    pub fn sanitized(mut self)->Self {
        self.order=self.order.min(2);self.palette_index=self.palette_index.clamp(-1,crate::pack::PALETTES.len() as i32);
        self.visuals.retain(|key,_| key.len()<=160);
        self.off.retain(|key| key.len()<=160);
        for rule in self.visuals.values_mut() {rule.weight=finite(rule.weight,0.0,5.0,1.0);rule.response=finite(rule.response,0.0,2.0,1.0);rule.beat=finite(rule.beat,0.0,2.0,1.0);rule.motion=finite(rule.motion,0.0,2.0,1.0);rule.accent=finite(rule.accent,0.0,2.0,1.0);}
        for t in &mut self.timing {t.min_bars=t.min_bars.clamp(1,64);t.max_bars=t.max_bars.clamp(t.min_bars,64);t.motion=finite(t.motion,0.0,2.0,1.0);t.intensity=finite(t.intensity,0.0,2.0,1.0);}
        self.accents.strength=finite(self.accents.strength,0.0,1.0,0.45);
        self.accents.spacing_bars=self.accents.spacing_bars.clamp(1,128);
        self.transition_beats=finite(self.transition_beats,0.0,16.0,2.0);
        self.title_gap_bars=self.title_gap_bars.clamp(4,128);
        for l in &mut self.layers {l.booth=l.booth.min(2);l.performer=l.performer.min(2);l.opacity=finite(l.opacity,0.0,1.0,1.0);l.looks=l.looks.map(|v|finite(v,0.0,5.0,1.0));}
        self
    }
    pub fn rule(&self,id:&str)->Rule {self.visuals.get(id).cloned().unwrap_or_default()}
    pub fn rule_for(&self,e:&Entry)->Rule {
        self.visuals.get(&e.id).cloned().unwrap_or_else(|| {
            let mut rule=Rule::default();
            if let Material::Grid(kind)=e.material && !kind.calm() {rule.sections[0]=false;rule.sections[4]=false;}
            // The older Hydra sketches: hand-pickable, not chosen automatically
            // unless given a weight.
            if let Material::Shader(name)=&e.material && !name.starts_with("isf:") {rule.weight=0.0;}
            rule
        })
    }
    pub fn is_off(&self,id:&str)->bool {self.off.contains(id)}
    /// Switch a scene off, or back on; true when it is now off.
    pub fn switch(&mut self,id:&str)->bool {if self.off.remove(id){false}else{self.off.insert(id.into());true}}
    pub fn allowed(&self,e:&Entry,mood:Mood)->f32 {if self.is_off(&e.id){return 0.0;}let rule=self.rule_for(e);if rule.sections[part(mood)]{rule.weight}else{0.0}}
    pub fn select<'a>(&self,entries:&'a [Entry],mood:Mood,seed:u64,avoid:Option<&str>,shader:bool)->Option<&'a Entry> {
        let pool:Vec<_>=entries.iter().filter(|e| matches!(e.material,Material::Grid(_)) || shader && matches!(e.material,Material::Shader(_))).filter(|e|self.allowed(e,mood)>0.0).collect();
        let has_other=pool.iter().any(|e|Some(e.id.as_str())!=avoid);
        let pool:Vec<_>=pool.into_iter().filter(|e|!has_other || Some(e.id.as_str())!=avoid).collect();
        if self.order==1 {
            let start=avoid.and_then(|id|entries.iter().position(|e|e.id==id)).map_or(0,|i|(i+1)%entries.len().max(1));
            return (0..entries.len()).map(|n|&entries[(start+n)%entries.len()]).find(|e|pool.iter().any(|p|p.id==e.id));
        }
        let sum:f32=pool.iter().map(|e|self.allowed(e,mood)).sum();
        let mut pick=(seed as u32 as f32/u32::MAX as f32)*sum;
        for e in &pool {pick-=self.allowed(e,mood);if pick<=0.0{return Some(e);}}
        pool.last().copied()
    }
    pub fn bars(&self,mood:Mood,seed:u64)->f64 {let t=self.timing[part(mood)];f64::from(t.min_bars+(seed%(u64::from(t.max_bars-t.min_bars)+1)) as u32)}
    pub fn transition(&self,seed:u64)->Option<u64> {let allowed:Vec<_>=self.transitions.iter().enumerate().filter_map(|(i,on)|on.then_some(i as u64)).collect();if allowed.is_empty(){None}else{Some(allowed[seed as usize%allowed.len()])}}
    pub fn effect(&self,original:Act,mood:Mood,seed:u64)->Act {
        let candidates:Vec<_>=Act::ALL.iter().copied().filter(|act| *act!=Act::Visual).filter_map(|act| {
            let id=format!("effect:{}",act.name());let rule=self.rule(&id);
            (!self.is_off(&id) && rule.weight>0.0 && rule.sections[part(mood)]).then_some((act,rule.weight))
        }).collect();
        // Preserve the established Director's choices until an effect has an
        // explicit operator rule; then honor every configured weight.
        let configured=self.visuals.keys().any(|id|id.starts_with("effect:"));
        if !configured && candidates.iter().any(|(a,_)|*a==original) {return original;}
        let sum:f32=candidates.iter().map(|(_,w)|w).sum();
        let mut pick=(seed as u32 as f32/u32::MAX as f32)*sum;
        for (act,weight) in candidates {pick-=weight;if pick<=0.0 {return act;}}
        Act::Visual
    }
}
#[derive(Clone,Debug,Default,PartialEq,Serialize,Deserialize)]
#[serde(default)]
pub struct Browser {pub sources:Sources,pub selected:String}
impl Browser {
    pub fn next<'a>(&mut self,entries:&'a [Entry],current:Option<&str>,back:bool,off:&BTreeSet<String>)->Option<&'a Entry> {
        // Space steps through pictures; titles are picked from the list.
        let pool:Vec<_>=entries.iter().filter(|e|e.collection!=Collection::Effects && self.sources.includes(e.collection)).collect();
        // Scenes switched off are skipped, unless every one is (then all play).
        let on:Vec<_>=pool.iter().copied().filter(|e|!off.contains(&e.id)).collect();
        let pool=if on.is_empty(){pool}else{on};
        if pool.is_empty(){return None;}
        let at=pool.iter().position(|e|Some(e.id.as_str())==current.or(Some(self.selected.as_str())));
        let i=at.map_or(0,|i|if back{(i+pool.len()-1)%pool.len()}else{(i+1)%pool.len()});
        self.selected=pool[i].id.clone();Some(pool[i])
    }
    /// Migrate the earlier fixed background setting into an ordinary named
    /// scene; no hidden permanent world override survives migration.
    pub fn legacy(index:i32)->Self {Self {sources:Sources::default(),selected:match index {2=>"cells:sunset-drive",3=>"cells:jellyfish",4=>"cells:event-horizon",_=>""}.into()}}
}
#[derive(Default)]
pub struct AccentEnvelope {last_beat:Option<i64>,last_accent:Option<i64>,amount:f32,drop_active:bool}
impl AccentEnvelope {
    pub fn trigger(&mut self,strength:f32) {self.amount=strength.clamp(0.0,1.0);}
    pub fn advance(&mut self,beat:Option<f64>,drop:bool,dt:f32,settings:Accents)->f32 {
        self.amount*=(-dt.clamp(0.0,0.25)/0.22).exp();
        let b=beat.filter(|v|v.is_finite()).map(|v|v.floor() as i64);
        let boundary=b.is_some() && b!=self.last_beat;
        let spacing=i64::from(settings.spacing_bars.max(1))*4;
        let ready=b.is_some_and(|v|self.last_accent.is_none_or(|last|v-last>=spacing));
        let trigger=match settings.pulse {
            Pulse::Off=>false,
            Pulse::Drops=>drop && !self.drop_active,
            Pulse::EveryBeat=>boundary,
            Pulse::Occasional=>(drop && !self.drop_active && ready) || boundary && ready && b.is_some_and(|v|v.rem_euclid(spacing)==0),
        };
        if trigger {self.amount=settings.strength;self.last_accent=b;}
        self.last_beat=b;self.drop_active=drop;self.amount
    }
}
/// Continuous operator motion scaling. Changing a slider changes speed, not
/// the absolute scene phase, so the scene does not teleport on adjustment.
#[derive(Default)]
pub struct Motion {last:Option<(f64,f64)>,seconds:f64,beats:f64,rate:f64}
impl Motion {
    pub fn clocks(&mut self,seconds:f64,beats:f64,target:f32,dt:f64)->(f64,f64) {
        let Some((s,b))=self.last.replace((seconds,beats)) else {self.seconds=seconds;self.beats=beats;self.rate=f64::from(target);return(seconds,beats)};
        self.rate+=(f64::from(target)-self.rate)*(1.0-(-dt.clamp(0.0,0.25)/0.4).exp());
        self.seconds+=(seconds-s).max(0.0)*self.rate;self.beats+=(beats-b).max(0.0)*self.rate;(self.seconds,self.beats)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn next_is_visible_and_old_and_new_interleave_without_hiding_disabled_scenes() {
        let entries=native_library();let mut b=Browser::legacy(2);
        assert_eq!(b.next(&entries,None,false,&BTreeSet::new()).unwrap().collection,Collection::Patch);
        assert_eq!(b.next(&entries,None,false,&BTreeSet::new()).unwrap().collection,Collection::Ambient);
        for filter in [Collection::Patch,Collection::Ambient] {
            b.sources=Sources {patch:filter==Collection::Patch,ambient:filter==Collection::Ambient,shaders:false,titles:true};
            for _ in 0..100 {assert_eq!(b.next(&entries,None,false,&BTreeSet::new()).unwrap().collection,filter);}
        }
    }
    #[test]
    fn hydra_sketches_are_manual_only_by_default() {
        let entries=library();let p=Preferences::default();
        let pool:Vec<_>=entries.iter().filter(|e|e.collection==Collection::Generative).cloned().collect();
        assert!(pool.iter().any(|e|e.name=="Silk"),"built-in generators have plain names");
        for seed in 0..200 {let e=p.select(&pool,Mood::Groove,seed,None,true).unwrap();assert!(e.id.starts_with("shader:isf:"),"{}",e.id);}
    }
    #[test]
    fn sources_combine_freely_and_never_leave_no_picture() {
        let entries=library();let mut s=Sources::default();
        assert!(s.active(&entries).iter().all(|e|e.collection!=Collection::Generative),"shaders are off by default");
        s.toggle(Collection::Patch);
        let only:Vec<_>=s.active(&entries).iter().map(|e|e.collection).collect();
        assert!(only.contains(&Collection::Ambient) && only.contains(&Collection::Effects) && !only.contains(&Collection::Patch));
        s.toggle(Collection::Ambient);assert!(s.ambient,"the last picture source stays on");
        s.toggle(Collection::Effects);assert_eq!(s.flags(),[false,true,false,false]);
        let p=Preferences::default();let pool=s.active(&entries);
        for seed in 0..200 {let e=p.select(&pool,Mood::Groove,seed,None,true).unwrap();assert_eq!(e.collection,Collection::Ambient);}
    }
    #[test]
    fn disabled_material_is_never_chosen_and_manual_access_is_independent() {
        let entries=native_library();let mut p=Preferences::default();
        for e in &entries {p.visuals.insert(e.id.clone(),Rule {weight:0.0,..Rule::default()});}
        assert!(p.select(&entries,Mood::Groove,42,None,false).is_none());
        assert!(Browser::default().next(&entries,None,false,&BTreeSet::new()).is_some());
        let one=&entries[5];p.visuals.get_mut(&one.id).unwrap().weight=2.0;
        for seed in 0..100 {assert_eq!(p.select(&entries,Mood::Groove,seed,None,false).unwrap().id,one.id);}
        p.visuals.get_mut(&one.id).unwrap().sections[part(Mood::Calm)]=false;
        assert!(p.select(&entries,Mood::Calm,7,None,false).is_none());
    }
    #[test]
    fn scenes_switched_off_are_skipped_and_come_back() {
        let entries=native_library();let mut p=Preferences::default();
        let ambient:Vec<_>=entries.iter().filter(|e|e.collection==Collection::Ambient).collect();
        let (gone,kept)=(ambient[0].id.clone(),ambient[1].id.clone());
        assert!(p.switch(&gone));
        let mut b=Browser {sources:Sources {patch:false,ambient:true,shaders:false,titles:false},selected:String::new()};
        for _ in 0..200 {assert_ne!(b.next(&entries,None,false,&p.off).unwrap().id,gone);}
        for seed in (0..500u64).map(|n|n*8_589_934) {assert_ne!(p.select(&entries,Mood::Groove,seed,None,false).unwrap().id,gone);}
        // Every Ambient scene off but one: Space and the show keep to it.
        for e in &ambient {p.off.insert(e.id.clone());}p.off.remove(&kept);
        let only:Vec<_>=entries.iter().filter(|e|e.collection==Collection::Ambient).cloned().collect();
        for _ in 0..10 {assert_eq!(b.next(&entries,None,false,&p.off).unwrap().id,kept);}
        for seed in 0..50 {assert_eq!(p.select(&only,Mood::Groove,seed,None,false).unwrap().id,kept);}
        // All off: Space still shows something rather than nothing.
        p.off.insert(kept.clone());assert!(b.next(&entries,None,false,&p.off).is_some());
        // On again, as it was.
        assert!(!p.switch(&gone));assert!(!p.is_off(&gone));
        p.off.clear();assert!((0..500u64).map(|n|n*8_589_934).any(|seed|p.select(&entries,Mood::Groove,seed,None,false).is_some_and(|e|e.id==gone)));
    }
    #[test]
    fn occasional_pulse_is_an_accent_not_a_continuous_beat_loop() {
        let mut envelope=AccentEnvelope::default();let a=Accents::default();let mut peaks=0;
        for frame in 0..(128*30) {let value=envelope.advance(Some(f64::from(frame)/15.0),false,1.0/30.0,a);if value>0.44{peaks+=1;}}
        assert_eq!(peaks,4,"one accent per sixteen bars at 120 BPM");
        let off=Accents {pulse:Pulse::Off,..a};
        for frame in 0..60 {envelope.advance(Some(300.0+f64::from(frame)/15.0),false,1.0/30.0,off);}
        let settled=envelope.advance(Some(320.0),true,1.0/30.0,off);
        assert!(settled<0.01,"Off never starts an accent, even on a drop");
    }
    #[test]
    fn drops_only_and_motion_control_have_no_repeated_trigger_or_phase_jump() {
        let mut e=AccentEnvelope::default();let a=Accents {pulse:Pulse::Drops,..Accents::default()};
        let hit=e.advance(Some(1.0),true,0.01,a);assert!(hit>0.0);
        assert!(e.advance(Some(2.0),true,0.1,a)<hit);
        let mut m=Motion::default();m.clocks(100.0,200.0,1.0,0.0);let(s,b)=m.clocks(100.02,200.04,2.0,0.02);assert!((s-100.0)<0.04 && (b-200.0)<0.08);
    }
}
