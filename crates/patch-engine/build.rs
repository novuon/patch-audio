//! Compile the small C shim that talks to libpipewire's pw_filter API.

fn pkg(args: &[&str]) -> Vec<String> {
    let out = std::process::Command::new("pkg-config")
        .args(args)
        .output()
        .expect("pkg-config");
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(String::from)
        .collect()
}

fn main() {
    let mut build = cc::Build::new();
    // Packagers' C flags, but never link-time optimisation: makepkg's
    // -flto=auto made the shim GCC bytecode that Rust's linker can't read
    // (a clean package build failed: undefined patch_engine_* symbols).
    // cc would add CFLAGS after ours, so they are taken over here, filtered.
    // Workspace `[profile.release] lto = "thin"` is rustc LTO. It is not
    // linker-plugin LTO, and `-fno-lto` below keeps this object out of it.
    let target = std::env::var("TARGET").unwrap_or_default();
    for key in [
        "CFLAGS".to_string(),
        "TARGET_CFLAGS".to_string(),
        format!("CFLAGS_{target}"),
        format!("CFLAGS_{}", target.replace('-', "_")),
    ] {
        println!("cargo:rerun-if-env-changed={key}");
        if let Ok(flags) = std::env::var(&key) {
            for f in flags
                .split_whitespace()
                .filter(|f| !f.starts_with("-flto") && *f != "-ffat-lto-objects")
            {
                build.flag(f);
            }
            // SAFETY: the build script is single-threaded; cc reads the
            // variable later, on this thread.
            unsafe { std::env::remove_var(&key) };
        }
    }
    build
        .file("csrc/engine.c")
        .opt_level(2)
        .flag_if_supported("-Wno-unused-parameter")
        .flag_if_supported("-fno-lto");
    for f in pkg(&["--cflags", "libpipewire-0.3"]) {
        build.flag(&f);
    }
    build.compile("patch_engine_shim");
    for f in pkg(&["--libs", "libpipewire-0.3"]) {
        if let Some(l) = f.strip_prefix("-l") {
            println!("cargo:rustc-link-lib={l}");
        } else if let Some(p) = f.strip_prefix("-L") {
            println!("cargo:rustc-link-search={p}");
        }
    }
    stretcher();
    println!("cargo:rerun-if-changed=csrc/engine.c");
    pdeath_helper();
}

/// The decks' keylock and key shift. Rubber Band's R3 engine, except in
/// tester kits (PATCH_KIT), which are distributed as binaries: Rubber Band
/// and the FFTW it links are GPL, so a kit builds Signalsmith Stretch (MIT,
/// vendored) instead and links neither. PATCH_STRETCH_ENGINE=signalsmith
/// picks it for any build, to compare.
fn stretcher() {
    println!("cargo:rerun-if-env-changed=PATCH_KIT");
    println!("cargo:rerun-if-env-changed=PATCH_STRETCH_ENGINE");
    println!("cargo:rustc-check-cfg=cfg(patch_signalsmith)");
    let signalsmith = std::env::var_os("PATCH_KIT").is_some()
        || std::env::var("PATCH_STRETCH_ENGINE").is_ok_and(|v| v == "signalsmith");
    if !signalsmith {
        for f in pkg(&["--libs", "rubberband"]) {
            if let Some(l) = f.strip_prefix("-l") {
                println!("cargo:rustc-link-lib={l}");
            } else if let Some(p) = f.strip_prefix("-L") {
                println!("cargo:rustc-link-search={p}");
            }
        }
        return;
    }
    println!("cargo:rustc-cfg=patch_signalsmith");
    println!("cargo:rerun-if-changed=csrc/stretch.cpp");
    println!("cargo:rerun-if-changed=vendor/signalsmith");
    cc::Build::new()
        .cpp(true)
        .std("c++17")
        .file("csrc/stretch.cpp")
        .include("vendor/signalsmith")
        .opt_level(3)
        .flag_if_supported("-fno-lto")
        .flag_if_supported("-Wno-unused-parameter")
        .compile("patch_stretch");
}

/// A few-kilobyte executable the engine posix_spawns. It is not linked into
/// the engine image: embedding it and execing it is what avoids fork.
fn pdeath_helper() {
    println!("cargo:rerun-if-changed=csrc/pdeath.c");
    let out_dir = std::env::var("OUT_DIR").expect("OUT_DIR");
    let dest = std::path::Path::new(&out_dir).join("pdeath-exec");
    let compiler = cc::Build::new().get_compiler();
    let status = compiler
        .to_command()
        .arg("-Os")
        .arg("-fno-lto")
        .arg("-o")
        .arg(&dest)
        .arg("csrc/pdeath.c")
        .status()
        .expect("compile pdeath helper");
    if !status.success() {
        panic!("pdeath helper failed to compile");
    }
    println!("cargo:rustc-env=PATCH_PDEATH_EXEC={}", dest.display());
}
