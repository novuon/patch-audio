//! Visualiser library, show settings and named profiles. Manual browsing uses
//! the complete registry; exclusions apply only to the automatic Director.
use crate::ui::{PatchWindow,VisualState,VisualLibraryItem,VisualControl};
use patch_visual::{choreography::{self as show,Entry,Preferences,Rule,Browser,Collection,Sources,Pulse,PaletteMode},generative::Mode,performance::Settings};
use slint::{ComponentHandle,ModelRc,VecModel};
use std::{cell::{Cell,RefCell},rc::Rc,path::PathBuf};
use serde::{Serialize,Deserialize};
#[derive(Clone,Default,Serialize,Deserialize)]
#[serde(default)]
struct Saved {preferences:Preferences,browser:Browser}
#[derive(Clone,Serialize,Deserialize)]
struct Profile {show:Saved,layers:Settings,generative:crate::visual_gen::Saved,held:bool}
fn path()->PathBuf {patch_engine::session::state_dir().join("visualiser-control.json")}
fn profiles()->PathBuf {patch_engine::session::state_dir().join("visualiser-profiles")}
fn write(path:&std::path::Path,data:&impl Serialize)->Result<(),String> {
    let dir=path.parent().ok_or("No settings directory")?;std::fs::create_dir_all(dir).map_err(|e|e.to_string())?;
    let bytes=serde_json::to_vec_pretty(data).map_err(|e|e.to_string())?;let tmp=path.with_extension("tmp");std::fs::write(&tmp,bytes).and_then(|_|std::fs::rename(tmp,path)).map_err(|e|e.to_string())
}
fn profile_path(name:&str)->Result<PathBuf,String> {
    let name=name.trim();if name.is_empty() || name.len()>64 || !name.chars().all(|c|c.is_alphanumeric()||matches!(c,' '|'_'|'-')) {return Err("Use a profile name with letters, numbers, spaces or dashes".into());}
    Ok(profiles().join(format!("{name}.json")))
}
pub struct Control {
    pub entries:Vec<Entry>,
    saved:RefCell<Saved>,
    pub request:RefCell<Option<Entry>>,
    pub profile:RefCell<Option<(Settings,crate::visual_gen::Saved,bool)>>,
    pub clear_hold:Cell<bool>,pub pulse:Cell<bool>,
    active:RefCell<Option<Vec<Entry>>>,
    /// Every window showing the controls: the Visualiser's and its own.
    windows:RefCell<Vec<slint::Weak<PatchWindow>>>,
    section:Cell<usize>,target:RefCell<String>,dirty:Cell<bool>,
}
impl Default for Control {
    fn default()->Self {
        let entries=show::library();
        let saved=std::fs::read(path()).ok().and_then(|s|serde_json::from_slice::<Saved>(&s).ok()).unwrap_or_else(|| {
            let old=std::fs::read(patch_engine::session::state_dir().join("visualiser-background.json")).ok().and_then(|s|serde_json::from_slice::<i32>(&s).ok()).unwrap_or(0);
            Saved {preferences:Preferences::default(),browser:Browser::legacy(old)}
        });
        let saved=Saved {preferences:saved.preferences.sanitized(),..saved};
        let request=entries.iter().find(|e| e.id==saved.browser.selected).cloned();
        Self {target:RefCell::new(entries.first().map(|e|e.id.clone()).unwrap_or_default()),entries,saved:RefCell::new(saved),request:RefCell::new(request),profile:RefCell::new(None),active:RefCell::new(None),windows:RefCell::new(Vec::new()),clear_hold:Cell::new(false),pulse:Cell::new(false),section:Cell::new(2),dirty:Cell::new(true)}
    }
}
impl Control {
    pub fn preferences(&self)->Preferences {self.saved.borrow().preferences.clone()}
    /// The material the choreographer may use: only the chosen sources.
    pub fn active(&self)->std::cell::Ref<'_,Vec<Entry>> {
        if self.active.borrow().is_none() {*self.active.borrow_mut()=Some(self.saved.borrow().browser.sources.active(&self.entries));}
        std::cell::Ref::map(self.active.borrow(),|a|a.as_ref().unwrap())
    }
    pub fn viewers(&self)->Vec<PatchWindow> {self.windows.borrow().iter().filter_map(|w|w.upgrade()).filter(|w|w.get_surface()=="visualiser").collect()}
    pub fn sources(&self)->Sources {self.saved.borrow().browser.sources}
    /// Changing the sources moves on at once to something from the new set
    /// when what is showing is no longer part of it.
    pub fn toggle(&self,col:Collection) {
        self.saved.borrow_mut().browser.sources.toggle(col);*self.active.borrow_mut()=None;
        let sources=self.sources();let selected=self.saved.borrow().browser.selected.clone();
        if !self.entries.iter().find(|e|e.id==selected).is_some_and(|e|sources.includes(e.collection)) {self.next(false);}
        self.dirty.set(true);self.save();self.refresh();
    }
    pub fn response(&self,id:&str)->Rule {self.saved.borrow().preferences.rule(id)}
    pub fn next(&self,back:bool) {
        let mut saved=self.saved.borrow_mut();let saved=&mut *saved;
        if let Some(entry)=saved.browser.next(&self.entries,None,back,&saved.preferences.off) {*self.request.borrow_mut()=Some(entry.clone());}
        self.clear_hold.set(true);self.dirty.set(true);
    }
    /// Switch a scene off (never played by the show or Space) or on again;
    /// switching off the one on screen moves on at once.
    pub fn switch(&self,id:&str) {
        if !self.entries.iter().any(|e|e.id==id) {return;}
        let off=self.saved.borrow_mut().preferences.switch(id);
        if off && self.saved.borrow().browser.selected==id {self.next(false);}
        self.save();self.dirty.set(true);self.refresh();
    }
    pub fn observe(&self,id:&str) {
        if let Some(entry)=self.entries.iter().find(|e|e.id==id) {
            for w in self.windows.borrow().iter().filter_map(|w|w.upgrade()) {
                let v=w.global::<VisualState>();v.set_current_scene(entry.name.as_str().into());v.set_current_scene_id(id.into());
                v.set_current_family(entry.collection.label().into());
            }
            if self.saved.borrow().browser.selected!=id {self.saved.borrow_mut().browser.selected=id.into();}
        }
    }
    fn save(&self) {let _=write(&path(),&*self.saved.borrow());}
    fn value(&self,key:&str)->f32 {
        let saved=self.saved.borrow();let p=&saved.preferences;let section=self.section.get();let t=p.timing[section];let layer=p.layers[section];
        match key {
            "automatic"=>f32::from(p.automatic),"order"=>f32::from(p.order),"palette"=>match p.palette {PaletteMode::Original=>0.0,PaletteMode::Director=>1.0,PaletteMode::Theme=>2.0},
            "palette-index"=>(p.palette_index+1) as f32,"shader-mode"=>if p.shader_mode==Mode::Hybrid {1.0}else{0.0},
            "overlays"=>f32::from(p.overlays),"title-gap"=>p.title_gap_bars as f32,
            "transition-beats"=>p.transition_beats,"wipe-diagonal"=>f32::from(p.transitions[0]),"wipe-radial"=>f32::from(p.transitions[1]),"wipe-slats"=>f32::from(p.transitions[2]),"wipe-dissolve"=>f32::from(p.transitions[3]),
            "pulse"=>match p.accents.pulse {Pulse::Off=>0.0,Pulse::Occasional=>1.0,Pulse::Drops=>2.0,Pulse::EveryBeat=>3.0},"pulse-strength"=>p.accents.strength,"pulse-spacing"=>p.accents.spacing_bars as f32,
            "bump"=>f32::from(p.accents.bump),"split"=>f32::from(p.accents.split),"shake"=>f32::from(p.accents.shake),
            "custom-lengths"=>f32::from(p.custom_lengths),"min-bars"=>t.min_bars as f32,"max-bars"=>t.max_bars as f32,"motion"=>t.motion,"intensity"=>t.intensity,
            "layer-booth"=>f32::from(layer.booth),"layer-performer"=>f32::from(layer.performer),"layer-opacity"=>layer.opacity,
            "look-silhouette"=>layer.looks[0],"look-dots"=>layer.looks[1],"look-me"=>layer.looks[2],
            _=>{let entry=self.entries.iter().find(|e|e.id==*self.target.borrow());let r=entry.map(|e|p.rule_for(e)).unwrap_or_default();match key {"rule-weight"=>r.weight,"rule-enabled"=>f32::from(r.sections[section]&&r.weight>0.0),"rule-response"=>r.response,"rule-beat"=>r.beat,"rule-motion"=>r.motion,"rule-accent"=>r.accent,_=>0.0}}
        }
    }
    fn set(&self,key:&str,value:f32) {
        if !value.is_finite(){return;}
        let section=self.section.get();let mut saved=self.saved.borrow_mut();let p=&mut saved.preferences;
        let on=value>=0.5;
        match key {
            "automatic"=>{p.automatic=on;self.clear_hold.set(true);},"order"=>p.order=value.round() as u8,
            "palette"=>p.palette=match value.round() as u8 {1=>PaletteMode::Director,2=>PaletteMode::Theme,_=>PaletteMode::Original},"palette-index"=>p.palette_index=value.round() as i32-1,
            "shader-mode"=>p.shader_mode=if on {Mode::Hybrid}else{Mode::Generative},"overlays"=>p.overlays=on,"title-gap"=>p.title_gap_bars=value.round() as u32,
            "transition-beats"=>p.transition_beats=value,"wipe-diagonal"=>p.transitions[0]=on,"wipe-radial"=>p.transitions[1]=on,"wipe-slats"=>p.transitions[2]=on,"wipe-dissolve"=>p.transitions[3]=on,
            "pulse"=>p.accents.pulse=match value.round() as u8 {1=>Pulse::Occasional,2=>Pulse::Drops,3=>Pulse::EveryBeat,_=>Pulse::Off},"pulse-strength"=>p.accents.strength=value,"pulse-spacing"=>p.accents.spacing_bars=value.round() as u32,
            "bump"=>p.accents.bump=on,"split"=>p.accents.split=on,"shake"=>p.accents.shake=on,
            "custom-lengths"=>p.custom_lengths=on,"min-bars"=>{p.custom_lengths=true;p.timing[section].min_bars=value.round() as u32;p.timing[section].max_bars=p.timing[section].max_bars.max(p.timing[section].min_bars);},"max-bars"=>{p.custom_lengths=true;p.timing[section].max_bars=value.round() as u32;p.timing[section].min_bars=p.timing[section].min_bars.min(p.timing[section].max_bars);},
            "motion"=>p.timing[section].motion=value,"intensity"=>p.timing[section].intensity=value,
            "layer-booth"=>p.layers[section].booth=value.round() as u8,"layer-performer"=>p.layers[section].performer=value.round() as u8,"layer-opacity"=>p.layers[section].opacity=value,
            "look-silhouette"=>p.layers[section].looks[0]=value,"look-dots"=>p.layers[section].looks[1]=value,"look-me"=>p.layers[section].looks[2]=value,
            _=>{
                let id=self.target.borrow().clone();let base=self.entries.iter().find(|e|e.id==id).map(|e|p.rule_for(e)).unwrap_or_default();let rule=p.visuals.entry(id).or_insert(base);
                match key {"rule-weight"=>rule.weight=value,"rule-enabled"=>{rule.sections[section]=on;if on && rule.weight<=0.0 {rule.weight=1.0;}},"rule-response"=>rule.response=value,"rule-beat"=>rule.beat=value,"rule-motion"=>rule.motion=value,"rule-accent"=>rule.accent=value,_=>{}}
            }
        }
        *p=p.clone().sanitized();drop(saved);self.save();self.dirty.set(true);self.refresh();
    }
    /// The controls window mirrors what the Visualiser shows: its hold,
    /// caption, GPU state and layer switches (D and the like change them
    /// there).
    pub fn mirror(&self,from:&PatchWindow) {
        let f=from.global::<VisualState>();
        let layers=crate::visual_layers::settings(from);
        for w in self.windows.borrow().iter().filter_map(|w|w.upgrade()) {
            if w.get_surface()!="visualiser-controls" {continue;}
            let v=w.global::<VisualState>();
            v.set_held(f.get_held());v.set_caption(f.get_caption());v.set_layers_gpu(f.get_layers_gpu());
            if crate::visual_layers::settings(&w)!=layers {crate::visual_layers::apply_settings(&w,layers);}
        }
    }
    pub fn bind(self:&Rc<Self>,w:&PatchWindow) {
        self.windows.borrow_mut().retain(|w|w.upgrade().is_some());
        self.windows.borrow_mut().push(w.as_weak());
        self.dirty.set(true);
        let v=w.global::<VisualState>();
        let c=self.clone();v.on_pick_scene(move |id| {if let Some(entry)=c.entries.iter().find(|e|e.id==id.as_str()) {*c.request.borrow_mut()=Some(entry.clone());c.saved.borrow_mut().browser.selected=entry.id.clone();c.clear_hold.set(true);c.save();c.dirty.set(true);c.refresh();}});
        let c=self.clone();v.on_switch_scene(move |id|c.switch(id.as_str()));
        let c=self.clone();v.on_toggle_source(move |index| {if let Some(&col)=Collection::ALL.get(index.max(0) as usize) {c.toggle(col);}});
        let c=self.clone();v.on_edit_control(move |id,value|c.set(id.as_str(),value));
        let c=self.clone();v.on_edit_section(move |index|{c.section.set((index as usize).min(6));c.dirty.set(true);c.refresh();});
        let c=self.clone();v.on_edit_material(move |id|{*c.target.borrow_mut()=id.to_string();c.dirty.set(true);c.refresh();});
        let c=self.clone();v.on_pulse_now(move ||c.pulse.set(true));
        let c=self.clone();let win=w.as_weak();v.on_save_profile(move |name| {
            if let Some(w)=win.upgrade() {let result=profile_path(name.as_str()).and_then(|dest| {
                let profile=Profile {show:c.saved.borrow().clone(),layers:crate::visual_layers::settings(&w),generative:crate::visual_gen::read_saved(),held:w.global::<VisualState>().get_held()};write(&dest,&profile)
            });w.global::<VisualState>().set_settings_note(result.map_or_else(|e|e,|_|"Profile saved".into()).into());c.dirty.set(true);}
        });
        let c=self.clone();let win=w.as_weak();v.on_load_profile(move |name| {
            let result=profile_path(name.as_str()).and_then(|dest|std::fs::read(dest).map_err(|e|e.to_string())).and_then(|data|serde_json::from_slice::<Profile>(&data).map_err(|e|e.to_string()));
            if let Some(w)=win.upgrade() {match result {
                Ok(mut profile)=>{profile.show.preferences=profile.show.preferences.sanitized();*c.saved.borrow_mut()=profile.show;*c.active.borrow_mut()=None;*c.profile.borrow_mut()=Some((profile.layers,profile.generative,profile.held));c.save();c.dirty.set(true);w.global::<VisualState>().set_settings_note("Profile loaded".into());},
                Err(e)=>w.global::<VisualState>().set_settings_note(e.into())
            }}
        });
        self.refresh_window(w);
    }
    pub fn refresh(&self) {
        if !self.dirty.replace(false){return;}
        self.windows.borrow_mut().retain(|w|w.upgrade().is_some());
        let windows:Vec<_>=self.windows.borrow().iter().filter_map(|w|w.upgrade()).collect();
        for w in &windows {self.refresh_window(w);}
    }
    fn refresh_window(&self,w:&PatchWindow) {
        let v=w.global::<VisualState>();let saved=self.saved.borrow();let sources=saved.browser.sources;let p=&saved.preferences;
        v.set_sources(ModelRc::new(VecModel::from(sources.flags().to_vec())));v.set_show_automatic(p.automatic);v.set_editing_section(self.section.get() as i32);
        let list:Vec<_>=self.entries.iter().filter(|e|sources.includes(e.collection)).map(|e| {
            let r=p.rule_for(e);VisualLibraryItem {id:e.id.as_str().into(),name:e.name.as_str().into(),family:e.collection.label().into(),enabled:r.weight>0.0&&r.sections[self.section.get()],off:p.is_off(&e.id)}
        }).collect();
        v.set_scene_library(ModelRc::new(VecModel::from(list)));
        v.set_library_count(format!("{} Patch · {} Ambient · {} shaders · {} title effects",patch_visual::stage::Backdrop::ALL.len(),patch_visual::stage::Backdrop::AMBIENT.len(),self.entries.iter().filter(|e|e.collection==Collection::Generative).count(),self.entries.iter().filter(|e|e.collection==Collection::Effects).count()).into());
        let target=self.target.borrow();v.set_editing_material(target.as_str().into());v.set_editing_material_name(self.entries.iter().find(|e|e.id==*target).map_or("",|e|e.name.as_str()).into());
        let mut controls=Vec::new();
        let mut add=|id:&str,label:&str,kind:i32,min:f32,max:f32,options:Vec<String>|controls.push(VisualControl {id:id.into(),label:label.into(),kind,value:self.value(id),min,max,options:ModelRc::new(VecModel::from(options.into_iter().map(Into::into).collect::<Vec<_>>()))});
        add("automatic","Automatic scene changes",0,0.0,1.0,vec![]);
        add("order","Selection order",2,0.0,2.0,vec!["Weighted variety".into(),"In library order".into(),"Track motif".into()]);
        add("custom-lengths","Custom scene lengths (off: the Director's)",0,0.0,1.0,vec![]);
        add("min-bars","Minimum scene bars",1,1.0,64.0,vec![]);add("max-bars","Maximum scene bars",1,1.0,64.0,vec![]);
        add("motion","Motion speed",1,0.0,2.0,vec![]);add("intensity","Scene intensity",1,0.0,2.0,vec![]);
        add("shader-mode","Automatic shader use",2,0.0,1.0,vec!["Shader alone".into(),"Hybrid".into()]);
        add("overlays","Supporting native overlays",0,0.0,1.0,vec![]);add("title-gap","Minimum gap between titles (bars)",1,4.0,128.0,vec![]);
        add("palette","Ambient colours",2,0.0,2.0,vec!["Original".into(),"Director palette".into(),"Theme".into()]);
        let mut palettes=vec!["Track palette".into()];palettes.extend(patch_visual::pack::PALETTES.iter().map(|p|p.name.to_string()));palettes.push("Theme".into());let count=palettes.len();add("palette-index","Palette choice",2,0.0,(count-1) as f32,palettes);
        add("transition-beats","Transition length (beats)",1,0.0,16.0,vec![]);
        for (id,label) in [("wipe-diagonal","Diagonal wipe"),("wipe-radial","Radial opening"),("wipe-slats","Vertical slats"),("wipe-dissolve","Dissolve")] {add(id,label,0,0.0,1.0,vec![]);}
        add("pulse","Full-screen pulse",2,0.0,3.0,vec!["Off".into(),"Occasional accents".into(),"Drops only".into(),"Every beat".into()]);
        add("pulse-strength","Pulse strength",1,0.0,1.0,vec![]);add("pulse-spacing","Accent spacing (bars)",1,1.0,128.0,vec![]);
        for (id,label) in [("bump","Frame bump accents"),("split","Colour split accents"),("shake","Frame shake accents")] {add(id,label,0,0.0,1.0,vec![]);}
        add("layer-booth","Booth in this section",2,0.0,2.0,vec!["As set".into(),"Show".into(),"Hide".into()]);
        add("layer-performer","Performer in this section",2,0.0,2.0,vec!["As set".into(),"Show".into(),"Hide".into()]);
        add("layer-opacity","Layer opacity multiplier",1,0.0,1.0,vec![]);
        for (id,label) in [("look-silhouette","Silhouette weight"),("look-dots","Dots weight"),("look-me","Me weight")] {add(id,label,1,0.0,5.0,vec![]);}
        v.set_show_controls(ModelRc::new(VecModel::from(controls)));
        let mut rules=Vec::new();
        for (id,label,min,max,kind) in [("rule-enabled","Allowed in this section",0.0,1.0,0),("rule-weight","Selection weight",0.0,5.0,1),("rule-beat","Local beat response",0.0,2.0,1),("rule-response","Spectrum response",0.0,2.0,1),("rule-motion","Motion multiplier",0.0,2.0,1),("rule-accent","Full-screen accent multiplier",0.0,2.0,1)] {
            rules.push(VisualControl {id:id.into(),label:label.into(),kind,value:self.value(id),min,max,options:ModelRc::default()});
        }
        v.set_material_controls(ModelRc::new(VecModel::from(rules)));
        let mut names:Vec<_>=std::fs::read_dir(profiles()).into_iter().flatten().flatten().filter_map(|e|e.path().file_stem()?.to_str().map(str::to_owned)).collect();names.sort();
        v.set_profile_names(ModelRc::new(VecModel::from(names.into_iter().map(Into::into).collect::<Vec<slint::SharedString>>())));
    }
}
