//! Plugin contributions. A plugin adds surfaces and layout recipes to a Unit.
//! Only manifests exist today; the plugin runtime is future work.

pub mod dj;

pub struct Manifest {
    pub surfaces: Vec<patch_core::SurfaceSpec>,
    pub layouts: Vec<patch_core::LayoutRecipe>,
}
