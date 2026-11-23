//! ISF (Interactive Shader Format, isf.video) generators as generative
//! scenes: a GLSL fragment shader whose leading `/*{ … }*/` JSON comment
//! declares its inputs. Patch reads the file and writes the shader for its
//! own GL header itself (no ISF runtime, no third-party code), so any ISF
//! generator a user drops in the folder plays to the music.
//!
//! The music: each input is a uniform bound to the signal bus. A sidecar
//! `<name>.patch.json` (`{"inputs": {"<input>": <Param>}}`) says which
//! signal drives which input; without one, an input's name picks it
//! (`bass` → low band, `level` → energy, `phase` → the beat, `barPhase`
//! → the bar…, an `event` fires on the kick)
//! and the rest stay at their defaults.
//!
//! Supported: one pass, inputs float / bool / long / color / point2D /
//! event, the built-ins `TIME`, `TIMEDELTA`, `FRAMEINDEX`, `RENDERSIZE`,
//! `DATE`, `isf_FragNormCoord`. Refused, with the reason: image and audio
//! inputs, multiple passes, imported images (no `IMG_*` sampling).

use crate::generative::{CompiledShader, Param, replace_word, shim};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

/// One declared input.
#[derive(Clone, Debug, PartialEq)]
pub enum Input {
    Float { min: f32, max: f32, default: f32 },
    Bool { default: bool },
    Long { default: f32 },
    Color { default: [f32; 4] },
    Point2D { default: [f32; 2] },
    Event,
}

/// A parsed ISF generator.
#[derive(Clone, Debug, PartialEq)]
pub struct Isf {
    pub description: String,
    pub credit: String,
    pub inputs: Vec<(String, Input)>,
    /// The GLSL after the JSON header, as written.
    pub code: String,
}

/// `<name>.patch.json`: which signal drives which input.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct Mapping {
    pub inputs: BTreeMap<String, Param>,
}

fn num(v: &Value) -> Option<f32> {
    v.as_f64().map(|f| f as f32).or_else(|| v.as_bool().map(|b| b as u8 as f32))
}

fn nums<const N: usize>(v: &Value, or: [f32; N]) -> [f32; N] {
    let mut out = or;
    if let Some(a) = v.as_array() {
        for (o, x) in out.iter_mut().zip(a) {
            if let Some(f) = num(x) {
                *o = f;
            }
        }
    }
    out
}

/// A name GLSL accepts (and that won't collide with Patch's own).
fn identifier(s: &str) -> bool {
    let mut c = s.chars();
    c.next().is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
        && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && !s.starts_with("gl_")
        && !s.starts_with("isf_")
        && !["frag", "prevBuffer"].contains(&s)
}

/// Parse an ISF file: its JSON header and the code after it.
pub fn parse(src: &str) -> Result<Isf, String> {
    let start = src.find("/*").ok_or("no ISF header (a leading /*{ … }*/ comment)")?;
    let end = src[start..].find("*/").ok_or("the ISF header comment never closes")? + start;
    let json: Value =
        serde_json::from_str(src[start + 2..end].trim()).map_err(|e| format!("the ISF header isn't JSON: {e}"))?;
    if let Some(p) = json["PASSES"].as_array()
        && (p.len() > 1 || p.iter().any(|x| x.get("TARGET").is_some()))
    {
        return Err("multiple passes or render targets aren't supported yet".into());
    }
    if json.get("IMPORTED").is_some_and(|v| v.as_object().is_some_and(|o| !o.is_empty()) || v.as_array().is_some_and(|a| !a.is_empty())) {
        return Err("imported images aren't supported".into());
    }
    let code = src[end + 2..].to_string();
    if code.contains("IMG_") {
        return Err("it samples images (IMG_*), which aren't supported".into());
    }
    let mut inputs = Vec::new();
    for i in json["INPUTS"].as_array().map_or(&[][..], |a| a.as_slice()) {
        let name = i["NAME"].as_str().ok_or("an input has no NAME")?;
        if !identifier(name) {
            return Err(format!("input name {name:?} can't be used"));
        }
        let input = match i["TYPE"].as_str().unwrap_or("") {
            "float" => {
                let min = num(&i["MIN"]).unwrap_or(0.0);
                let max = num(&i["MAX"]).unwrap_or(1.0);
                Input::Float { min, max, default: num(&i["DEFAULT"]).unwrap_or(min) }
            }
            "bool" => Input::Bool { default: num(&i["DEFAULT"]).unwrap_or(0.0) > 0.5 },
            "long" => Input::Long { default: num(&i["DEFAULT"]).unwrap_or(0.0) },
            "color" => Input::Color { default: nums(&i["DEFAULT"], [1.0, 1.0, 1.0, 1.0]) },
            "point2D" => Input::Point2D { default: nums(&i["DEFAULT"], [0.0, 0.0]) },
            "event" => Input::Event,
            t @ ("image" | "audio" | "audioFFT") => return Err(format!("{t} inputs aren't supported")),
            t => return Err(format!("input type {t:?} isn't ISF")),
        };
        inputs.push((name.to_string(), input));
    }
    Ok(Isf {
        description: json["DESCRIPTION"].as_str().unwrap_or("").to_string(),
        credit: json["CREDIT"].as_str().unwrap_or("").to_string(),
        inputs,
        code,
    })
}

/// `code` uses the identifier `name`.
fn mentions(code: &str, name: &str) -> bool {
    replace_word(code, name, "\u{1}") != code
}

/// The signal an input's name suggests, scaled into its range; `None`
/// keeps the default.
fn by_name(name: &str, min: f32, max: f32) -> Option<Param> {
    let n = name.to_ascii_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| n.contains(w));
    let signal = if n == "phase" || n == "beatphase" {
        "phase"
    } else if n == "barphase" {
        "bar"
    } else if n == "beats" {
        // The running beat count: motion locked to the music, not the clock.
        return Some(Param::sig("beat", 1.0, 0.0));
    } else if n == "hue" {
        return Some(Param::sig("intent:hue", max - min, min).smoothed(2.0));
    } else if n == "punch" {
        // The kick's envelope (1 on the hit, falling over ~150 ms).
        return Some(Param::sig("kick", max - min, min));
    } else if has(&["bass", "low", "kick"]) {
        "low"
    } else if has(&["mid"]) {
        "mid"
    } else if has(&["high", "treble"]) {
        "high"
    } else if has(&["audio", "level", "volume", "energy", "intensity", "amp", "pulse", "react"]) {
        "energy"
    } else {
        return None;
    };
    Some(Param::sig(signal, max - min, min).smoothed(0.08))
}

/// Patch's shader for `isf`: the inputs as uniforms (bound per `mapping`,
/// else by name, else constant), the built-ins defined over Patch's.
pub fn compile(isf: &Isf, mapping: &Mapping) -> Result<CompiledShader, String> {
    // Patch's own uniforms under names no shader uses (shaders often
    // declare a `time` or a `uv` of their own).
    let mut decl = String::from(
        "uniform float isf_TIME;\nuniform vec2 isf_RENDERSIZE;\n\
         #define TIME isf_TIME\n#define RENDERSIZE isf_RENDERSIZE\n\
         #define isf_FragNormCoord (gl_FragCoord.xy / isf_RENDERSIZE)\n\
         #define vv_FragNormCoord isf_FragNormCoord\n#define TIMEDELTA (1.0 / 60.0)\n\
         #define FRAMEINDEX int(isf_TIME * 60.0)\n#define PASSINDEX 0\n\
         #define DATE vec4(0.0, 0.0, 0.0, mod(isf_TIME, 86400.0))\n",
    );
    let mut uniforms = Vec::new();
    let mut bind = |u: String, p: Param, decl: &mut String| {
        decl.push_str(&format!("uniform float {u};\n"));
        uniforms.push((u, p));
    };
    for (name, input) in &isf.inputs {
        let given = mapping.inputs.get(name).cloned();
        let u = |i: usize| format!("isf_{name}_{i}");
        match input {
            Input::Float { min, max, default } => {
                let p = given.or_else(|| by_name(name, *min, *max)).unwrap_or(Param::c(*default));
                bind(name.clone(), p, &mut decl);
            }
            Input::Bool { default } => {
                bind(u(0), given.unwrap_or(Param::c(*default as u8 as f32)), &mut decl);
                decl.push_str(&format!("#define {name} ({} > 0.5)\n", u(0)));
            }
            Input::Long { default } => {
                bind(u(0), given.unwrap_or(Param::c(*default)), &mut decl);
                decl.push_str(&format!("#define {name} int({})\n", u(0)));
            }
            Input::Event => {
                // True on the frame the kick lands (its envelope starts at 1).
                bind(u(0), given.unwrap_or(Param::sig("kick", 1.0, 0.0)), &mut decl);
                decl.push_str(&format!("#define {name} ({} > 0.9)\n", u(0)));
            }
            Input::Color { default } => {
                for (i, d) in default.iter().enumerate() {
                    bind(u(i), Param::c(*d), &mut decl);
                }
                decl.push_str(&format!("#define {name} vec4({}, {}, {}, {})\n", u(0), u(1), u(2), u(3)));
            }
            Input::Point2D { default } => {
                for (i, d) in default.iter().enumerate() {
                    bind(u(i), Param::c(*d), &mut decl);
                }
                decl.push_str(&format!("#define {name} vec2({}, {})\n", u(0), u(1)));
            }
        }
    }
    // Common VJ audio names (not ISF, but many shaders read them), when
    // used and not declared.
    for (name, p) in [
        ("audioLevel", Param::sig("energy", 1.0, 0.0)),
        ("audioBass", Param::sig("low", 1.0, 0.0)),
        ("audioMid", Param::sig("mid", 1.0, 0.0)),
        ("audioMids", Param::sig("mid", 1.0, 0.0)),
        ("audioHigh", Param::sig("high", 1.0, 0.0)),
        ("audioHighs", Param::sig("high", 1.0, 0.0)),
        ("audioBeat", Param::sig("kick", 1.0, 0.0)),
        ("audioBeatPhase", Param::sig("phase", 1.0, 0.0)),
    ] {
        if mentions(&isf.code, name)
            && !isf.code.contains(&format!("float {name}"))
            && !isf.inputs.iter().any(|(n, _)| n == name)
        {
            bind(name.to_string(), p, &mut decl);
        }
    }
    // Some shaders declare the built-ins or their inputs themselves: Patch
    // does.
    let mut code: String = isf
        .code
        .lines()
        .filter(|l| {
            let w: Vec<&str> = l.split(|c: char| c.is_whitespace() || c == ';').filter(|w| !w.is_empty()).collect();
            !(w.len() == 3
                && w[0] == "uniform"
                && (["TIME", "RENDERSIZE", "TIMEDELTA", "FRAMEINDEX"].contains(&w[2])
                    || isf.inputs.iter().any(|(n, _)| n == w[2])))
        })
        .map(|l| format!("{l}\n"))
        .collect();
    // Names GLSL 3.30 reserves that ES 1.0 shaders use freely.
    for n in ["noise1", "noise2", "noise3", "noise4", "centroid"] {
        code = replace_word(&code, n, &format!("isf_{n}"));
    }
    let t0 = std::time::Instant::now();
    let frag = shim(&format!("{decl}{code}"))?;
    Ok(CompiledShader { frag, uniforms, feedback: false, compile_us: t0.elapsed().as_secs_f64() * 1e6 })
}

/// Patch's own ISF generators (MIT, part of Patch): what plays before a
/// user adds any.
pub fn builtin() -> Vec<(&'static str, &'static str)> {
    vec![
        ("silk", include_str!("isf/silk.fs")),
        ("aurora", include_str!("isf/aurora.fs")),
        ("beams", include_str!("isf/beams.fs")),
        ("contours", include_str!("isf/contours.fs")),
        ("caustics", include_str!("isf/caustics.fs")),
        ("orbit", include_str!("isf/orbit.fs")),
        ("rings", include_str!("isf/rings.fs")),
        ("bloom", include_str!("isf/bloom.fs")),
    ]
}

/// The user's folder: `$PATCH_ISF_DIR`, else
/// `~/.local/share/omarchy-patch/isf`.
pub fn user_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("PATCH_ISF_DIR").map(Into::into).or_else(|| {
        std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/share/omarchy-patch/isf"))
    })
}

/// Every ISF scene: built-ins, then the user's `*.fs` by name (a user file
/// with a built-in's name replaces it).
pub fn scenes() -> Vec<String> {
    let mut names: Vec<String> = builtin().iter().map(|(n, _)| n.to_string()).collect();
    if let Some(dir) = user_dir()
        && let Ok(rd) = std::fs::read_dir(dir)
    {
        let mut user: Vec<String> = rd
            .flatten()
            .filter_map(|e| e.path().file_name()?.to_str()?.strip_suffix(".fs").map(str::to_string))
            .collect();
        user.sort();
        for n in user {
            if !names.contains(&n) {
                names.push(n);
            }
        }
    }
    names
}

/// A scene's source and mapping: the user's file first, else a built-in.
pub fn load(name: &str) -> Result<(String, Mapping), String> {
    if let Some(dir) = user_dir() {
        let file = dir.join(format!("{name}.fs"));
        if let Ok(src) = std::fs::read_to_string(&file) {
            let mapping = match std::fs::read(dir.join(format!("{name}.patch.json"))) {
                Ok(b) => serde_json::from_slice(&b).map_err(|e| format!("{name}.patch.json: {e}"))?,
                Err(_) => Mapping::default(),
            };
            return Ok((src, mapping));
        }
    }
    builtin()
        .into_iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| (s.to_string(), Mapping::default()))
        .ok_or_else(|| format!("no ISF scene {name:?}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRC: &str = r#"/*{
        "DESCRIPTION": "test", "CREDIT": "Patch",
        "INPUTS": [
            {"NAME": "bassAmount", "TYPE": "float", "MIN": 0.0, "MAX": 2.0, "DEFAULT": 0.5},
            {"NAME": "zoom", "TYPE": "float", "MIN": 1.0, "MAX": 4.0, "DEFAULT": 2.0},
            {"NAME": "invert", "TYPE": "bool", "DEFAULT": true},
            {"NAME": "tint", "TYPE": "color", "DEFAULT": [1.0, 0.5, 0.25, 1.0]},
            {"NAME": "flash", "TYPE": "event"}
        ]
    }*/
    void main() {
        vec2 p = isf_FragNormCoord * zoom;
        vec4 c = tint * (0.5 + 0.5 * sin(TIME + p.x * bassAmount));
        if (invert) c.rgb = 1.0 - c.rgb;
        if (flash) c = vec4(1.0);
        gl_FragColor = c;
    }"#;

    #[test]
    fn inputs_bind_to_the_music_by_name_or_stay_default() {
        let isf = parse(SRC).unwrap();
        assert_eq!(isf.inputs.len(), 5);
        let c = compile(&isf, &Mapping::default()).unwrap();
        let p = |n: &str| c.uniforms.iter().find(|(u, _)| u == n).map(|(_, p)| p.clone()).unwrap();
        assert_eq!(p("bassAmount"), Param::sig("low", 2.0, 0.0).smoothed(0.08));
        assert_eq!(p("zoom"), Param::c(2.0));
        assert_eq!(p("isf_invert_0"), Param::c(1.0));
        assert_eq!(p("isf_tint_1"), Param::c(0.5));
        assert_eq!(p("isf_flash_0"), Param::sig("kick", 1.0, 0.0));
        assert!(c.frag.contains("frag = c;"));
    }

    #[test]
    fn a_sidecar_mapping_wins() {
        let isf = parse(SRC).unwrap();
        let m: Mapping = serde_json::from_str(r#"{"inputs": {"zoom": {"signal": "bar", "scale": 3, "offset": 1}}}"#).unwrap();
        let c = compile(&isf, &m).unwrap();
        assert!(c.uniforms.contains(&("zoom".into(), Param::sig("bar", 3.0, 1.0))));
    }

    #[test]
    fn unsupported_files_say_why() {
        let img = r#"/*{"INPUTS": [{"NAME": "inputImage", "TYPE": "image"}]}*/ void main(){gl_FragColor=vec4(1.0);}"#;
        assert!(parse(img).unwrap_err().contains("image"));
        let passes = r#"/*{"PASSES": [{"TARGET": "a", "PERSISTENT": true}, {}]}*/ void main(){gl_FragColor=vec4(1.0);}"#;
        assert!(parse(passes).unwrap_err().contains("passes"));
        assert!(parse("void main(){}").unwrap_err().contains("header"));
        let clash = r#"/*{"INPUTS": [{"NAME": "frag", "TYPE": "float"}]}*/ void main(){gl_FragColor=vec4(1.0);}"#;
        assert!(parse(clash).unwrap_err().contains("frag"));
    }

    #[test]
    fn the_builtins_parse_and_compile() {
        for (name, src) in builtin() {
            let isf = parse(src).unwrap_or_else(|e| panic!("{name}: {e}"));
            compile(&isf, &Mapping::default()).unwrap_or_else(|e| panic!("{name}: {e}"));
        }
    }
}
