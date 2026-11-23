//! Headless Library shots: places, Where, and collections. No audio and no
//! engine. Writes PNGs to the directory given as the first argument
//! (default `/tmp/patch-library-shots`).
use patch_host::{HostedApp, SurfaceWindow};
use patch_surfaces::ui::DjState;
use patch_surfaces::{music_dir, AudioApp};
use slint::ComponentHandle;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    patch_host::shot::install()?;
    let app = AudioApp::new();
    let out = std::path::PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "/tmp/patch-library-shots".into()),
    );
    std::fs::create_dir_all(&out)?;
    let shooter = patch_host::shot::Shooter { scale: 1.5 };
    let spec = AudioApp::spec();
    let window = app.create("library");
    window.apply_theme(&patch_ui::load_palette());
    window.apply_host(&patch_host::HostState::build(
        &*app,
        &spec,
        "library",
        &[],
        None,
    ));
    let save = |name: &str, w: u32, h: u32| -> Result<(), Box<dyn std::error::Error>> {
        let path = out.join(name);
        shooter.render_component(&window, w, h).save(&path)?;
        println!("{}", path.display());
        Ok(())
    };
    save("library-all.png", 1180, 920)?;
    save("library-where-column.png", 1680, 860)?;
    let dj = window.global::<DjState>();
    dj.invoke_lib_browse(music_dir().join("House").to_string_lossy().to_string().into());
    save("library-folder.png", 1180, 920)?;
    dj.invoke_lib_browse(music_dir().to_string_lossy().to_string().into());
    save("library-source.png", 1180, 920)?;
    // Warm-up · deep and slow, the crate whose tracks share House/2024.
    dj.invoke_collection_view(3);
    save("library-collection.png", 1180, 920)?;
    // Back to the whole library, with the favourites filter on.
    dj.invoke_collection_view(-1);
    dj.invoke_lib_smart_toggle("fav".into());
    save("library-favourites.png", 1180, 920)?;
    Ok(())
}
