/*{
    "DESCRIPTION": "A living topographic map: fine contour lines over a slowly shifting terrain; the bass raises the ground, one red line climbs through the levels over each phrase.",
    "CREDIT": "Patch (MIT)",
    "ISFVSN": "2",
    "CATEGORIES": ["Generator"],
    "INPUTS": [
        {"NAME": "energy", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5},
        {"NAME": "bass", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.3},
        {"NAME": "highs", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.2},
        {"NAME": "beats", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0},
        {"NAME": "punch", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0},
        {"NAME": "hue", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0}
    ]
}*/

float h21(vec2 p) {
    p = fract(p * vec2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

float vnoise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    vec2 u = f * f * (3.0 - 2.0 * f);
    return mix(mix(h21(i), h21(i + vec2(1.0, 0.0)), u.x),
               mix(h21(i + vec2(0.0, 1.0)), h21(i + vec2(1.0, 1.0)), u.x), u.y);
}

float fbm(vec2 p) {
    float v = 0.0;
    float a = 0.5;
    mat2 m = mat2(1.6, 1.2, -1.2, 1.6);
    for (int i = 0; i < 5; i++) {
        v += a * vnoise(p);
        p = m * p;
        a *= 0.5;
    }
    return v;
}

vec3 hueRotate(vec3 c, float h) {
    float a = h * 6.2831853;
    vec3 k = vec3(0.57735);
    return c * cos(a) + cross(k, c) * sin(a) + k * dot(k, c) * (1.0 - cos(a));
}

float terrain(vec2 p, float t) {
    float h = fbm(p * 1.1 + vec2(t * 0.05, -t * 0.03));
    h += 0.45 * fbm(p * 2.6 - vec2(t * 0.04, t * 0.02) + 7.0);
    return h;
}

void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 p = (uv - 0.5) * vec2(RENDERSIZE.x / RENDERSIZE.y, 1.0) * 2.4;
    float t = beats * 0.25 + TIME * 0.05;
    float h = terrain(p, t);
    // The bass swells the ground outward from the centre, beat by beat.
    h += bass * 0.12 * (0.5 + 0.5 * sin(length(p) * 2.5 - beats * 1.5708));

    float levels = 22.0;
    float v = h * levels;
    float w = max(fwidth(v), 1e-4);
    float idx = floor(v + 0.5);
    float lineDist = abs(fract(v - 0.5) - 0.5) / w;
    bool major = mod(idx, 5.0) < 0.5;
    float line = 1.0 - clamp(lineDist - (major ? 0.9 : 0.35), 0.0, 1.0);

    // Shading: a faint hillshade so the map reads as relief.
    float e = 0.01;
    vec2 g = vec2(terrain(p + vec2(e, 0.0), t) - terrain(p - vec2(e, 0.0), t),
                  terrain(p + vec2(0.0, e), t) - terrain(p - vec2(0.0, e), t)) / (2.0 * e);
    float shade = clamp(0.5 + dot(normalize(vec3(-g, 1.5)), normalize(vec3(-0.5, 0.6, 0.6))) * 0.5, 0.0, 1.0);

    vec3 ink = vec3(0.012, 0.012, 0.018);
    vec3 c = ink + vec3(0.025, 0.022, 0.03) * shade;
    vec3 minorCol = vec3(0.46, 0.45, 0.47) * (0.35 + 0.3 * energy);
    vec3 majorCol = vec3(0.92, 0.9, 0.88) * (0.55 + 0.35 * energy);
    vec3 lc = major ? majorCol : minorCol;

    // One red level climbs through the map over 16 beats.
    float lo = 0.35;
    float hi = 1.25;
    float target = floor((lo + (hi - lo) * fract(beats / 16.0)) * levels + 0.5);
    float accent = 1.0 - clamp(abs(idx - target), 0.0, 1.0);
    lc = mix(lc, vec3(0.94, 0.14, 0.23) * 1.1, accent);
    float lw = line * (1.0 + 0.6 * punch * (major ? 1.0 : 0.3));
    // Highs glint along the major lines.
    lw += line * float(major) * highs * 0.5 * step(0.8, vnoise(p * 30.0 + TIME * 2.0));
    c += lc * lw;
    c += vec3(0.94, 0.14, 0.23) * accent * 0.25 * exp(-lineDist * 0.15);

    c *= 1.0 - 0.5 * dot(uv - 0.5, uv - 0.5) * 2.0;
    c = c / (1.0 + c * 0.6);
    c = hueRotate(c, hue);
    gl_FragColor = vec4(max(c, 0.0), 1.0);
}
