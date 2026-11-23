//! Isolated MIT scene source. No windowing, audio capture or producer loops.
mod eye;
mod matrix;
pub(super) mod scenes3d;
mod showcase_scenes;
mod tunnel;
use super::{Sound,raster::DirectCell};
use rand::{SeedableRng,rngs::StdRng};
pub(super) mod seed {
    thread_local! {static STATE:std::cell::Cell<u32>=const {std::cell::Cell::new(1)};}
    pub(super) fn reset(seed:u32) {STATE.with(|s|s.set(seed));}
    pub(crate) fn next()->u32 {STATE.with(|s| {let n=s.get().wrapping_mul(1664525).wrapping_add(1013904223);s.set(n);n})}
}
pub(super) enum Scene {
    Showcase(Box<showcase_scenes::Showcase>),
    Eye(eye::Eye,StdRng),
    Matrix(Option<matrix::Rain>,StdRng,(usize,usize),Vec<DirectCell>),
    Tunnel(tunnel::Tunnel),
}
impl Scene {
    pub(super) fn new(name:&str,seed:u32)->Self {
        self::seed::reset(seed);
        match name {
            "eye-reactive"=>Self::Eye(eye::Eye::new(),StdRng::seed_from_u64(u64::from(seed))),
            "matrix-reactive"=>Self::Matrix(None,StdRng::seed_from_u64(u64::from(seed)),(0,0),vec![]),
            "tunnel-reactive"=>Self::Tunnel(tunnel::Tunnel::new()),
            _=>Self::Showcase(Box::new(showcase_scenes::Showcase::new(showcase_scenes::Kind::parse(name).expect("registered Ambient scene"))))
        }
    }
    pub(super) fn draw(&mut self,dt:f32,pointer:Option<(f32,f32)>,sound:Sound,cols:usize,rows:usize,out:&mut Vec<DirectCell>) {
        match self {
            Self::Showcase(s)=>{s.prepare(cols,rows);s.step(dt,pointer,sound);s.draw(cols,rows,2.0,out);}
            Self::Eye(s,rng)=>{let lid=s.step(dt,pointer,rng);s.draw(cols,rows,2.0,lid,sound.bass,out);}
            Self::Matrix(s,rng,size,cached)=>{
                let first=s.is_none();
                if first {*s=Some(matrix::Rain::new(cols,rows,2.0,rng));*size=(cols,rows);}
                if dt>0.0 || first {s.as_mut().unwrap().step(dt,pointer,sound,rng,cached);}
                out.clear();out.extend(cached.iter().cloned().map(|mut c| {c.x=c.x*cols/size.0.max(1);c.y=c.y*rows/size.1.max(1);c}));
            }
            Self::Tunnel(s)=>{s.step(dt,pointer,sound);s.draw(cols,rows,2.0,out);}
        }
    }
}
