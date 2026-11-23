// Adapted from avillagran/omarchy-ttfx-layer, MIT, c969b6c172108c5f6ab937d99db67c17ddd28196.
// See ../LICENSE and ../README.md for provenance and integration notes.
use crate::ambient::{Sound,raster::DirectCell};
use rand::Rng;
struct Drop { head: f32, speed: f32, trail: usize, birth_light:Option<f32> }
struct Spark { x: usize, head: f32, speed: f32, trail: usize, birth_light:Option<f32> }
pub struct Rain {
    cols: usize, rows: usize, _aspect: f32,
    pub(crate) reactive_speed: bool,
    drops: Vec<Drop>, glyphs: Vec<char>, chars: Vec<char>, heat: Vec<f32>,
    sparks: Vec<Spark>, spawn_budget: f32, bass_budget:f32,
}
impl Rain {
    pub fn new(cols: usize, rows: usize, aspect: f32, rng: &mut impl Rng) -> Self {
        let chars: Vec<char> = "ｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜﾝ0123456789ABCDEF".chars().collect();
        let drops = (0..cols).map(|_| {let head=rng.gen_range(-(rows as f32)..rows as f32);Drop {head,speed:rng.gen_range(12.0..32.0),trail:rng.gen_range(10..25),birth_light:if head>=0. {Some(0.)} else {None}}}).collect();
        let glyphs = (0..cols*rows).map(|_| chars[rng.gen_range(0..chars.len())]).collect();
        Self { cols, rows, reactive_speed:false, _aspect: aspect.clamp(1.,4.), drops, glyphs, chars, heat: vec![0.;cols*rows], sparks: Vec::with_capacity(384), spawn_budget: 0., bass_budget:0. }
    }
    pub fn step(&mut self, dt: f32, pointer: Option<(f32,f32)>, sound: Sound, rng: &mut impl Rng, cells: &mut Vec<DirectCell>) {
        let dt = dt.clamp(0., 0.1);
        let bass=sound.bass.clamp(0.,1.);
        let treble=sound.treble.clamp(0.,1.);
        let motion=if self.reactive_speed {1.+bass*0.4} else {1.};
        let decay = (-dt*3.5).exp();
        for h in &mut self.heat { *h *= decay; }
        if let Some((nx,_ny)) = pointer.filter(|(x,y)| x.is_finite() && y.is_finite()) {
            let px = nx.clamp(0.,1.)*(self.cols-1) as f32;
            // The pointer selects rain lanes, not a radial emitter at its position.
            let radius = 12.;
            let x0 = (px-radius).max(0.) as usize;
            let x1 = ((px+radius) as usize+1).min(self.cols);
            for x in x0..x1 {
                let light = (1.-((x as f32-px)/radius).abs()).max(0.)*0.35;
                for y in 0..self.rows {
                    self.heat[y*self.cols+x] = self.heat[y*self.cols+x].max(light);
                }
            }
            self.spawn_budget = (self.spawn_budget+dt*36.).min(4.);
            while self.spawn_budget >= 1. && self.sparks.len()<384 {
                self.spawn_budget-=1.;
                self.sparks.push(Spark {
                    x: (px+rng.gen_range(-12.0..12.0)).clamp(0.,(self.cols-1) as f32) as usize,
                    head: -rng.gen_range(0.0..1.0),
                    speed: rng.gen_range(12.0..32.0), trail:rng.gen_range(10..25),birth_light:None,
                });
            }
        } else { self.spawn_budget=0.; }
        // Bass adds top-origin streams across the whole output, independently of the mouse.
        self.bass_budget=if bass<0.001 {0.} else {(self.bass_budget+dt*bass*144.).min(16.)};
        while self.bass_budget>=1. && self.sparks.len()<384 {
            self.bass_budget-=1.;
            self.sparks.push(Spark{x:rng.gen_range(0..self.cols),head:-rng.gen_range(0.0..1.0),speed:rng.gen_range(12.0..32.0),trail:rng.gen_range(10..25),birth_light:None});
        }
        cells.clear();
        for (x,drop) in self.drops.iter_mut().enumerate() {
            drop.head += drop.speed*dt*motion;
            if drop.birth_light.is_none() && drop.head>=0. {drop.birth_light=Some(treble);}
            if drop.head-drop.trail as f32 > self.rows as f32 {
                drop.head = -rng.gen_range(5.0..self.rows.max(6) as f32);
                drop.speed = rng.gen_range(12.0..32.0);
                drop.trail = rng.gen_range(10..25);
                drop.birth_light=None;
            }
            for t in 0..drop.trail {
                let y=drop.head.floor() as i32-t as i32;
                if y<0 || y>=self.rows as i32 { continue; }
                let y=y as usize; let i=y*self.cols+x;
                if rng.r#gen::<f32>() < 0.025 { self.glyphs[i]=self.chars[rng.gen_range(0..self.chars.len())]; }
                let color=strip_color(t,drop.trail,drop.birth_light.unwrap_or(0.),self.heat[i]);
                cells.push(DirectCell { x,y,ch:self.glyphs[i],color,background:None,attrs:0 });
            }
        }
        for spark in &mut self.sparks {
            spark.head+=spark.speed*dt*motion;
            if spark.birth_light.is_none() && spark.head>=0. {spark.birth_light=Some(treble);}
            for t in 0..spark.trail {
                let y=spark.head.floor() as i32-t as i32;
                if y>=0 && y<self.rows as i32 {
                    let y=y as usize; let i=y*self.cols+spark.x;
                    let color=strip_color(t,spark.trail,spark.birth_light.unwrap_or(0.),self.heat[i]);
                    cells.push(DirectCell { x:spark.x,y,ch:self.glyphs[i],color,background:None,attrs:0 });
                }
            }
        }
        self.sparks.retain(|s|s.head-(s.trail as f32)<self.rows as f32);

    }
}
fn strip_color(t:usize,trail:usize,birth:f32,heat:f32)->(u8,u8,u8) {
    let g=20.+180.*(1.-t as f32/trail as f32).powf(1.4);
    let (base,shade)=if t==0 {((110.,255.,165.),1.)} else {((0.,g,g*0.18),g/200.)};
    let light=birth.clamp(0.,1.);
    let boost=1.+heat*0.7;
    let channel=|old:f32,target:f32|((old+(target*shade-old)*light)*boost).clamp(0.,255.) as u8;
    (channel(base.0,225.),channel(base.1,255.),channel(base.2,235.))
}
