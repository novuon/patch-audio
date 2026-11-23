/*{
    "DESCRIPTION": "Satin folds drifting on the beat: domain-warped fabric lit from the side; the bass deepens the folds, the kick runs a sheen across them.",
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
    for (int i = 0; i < 4; i++) {
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

float field(vec2 p, float t, float warp) {
    vec2 q = vec2(fbm(p + vec2(0.0, t)), fbm(p + vec2(5.2, 1.3) - t));
    vec2 r = vec2(fbm(p + 3.0 * q + vec2(1.7, 9.2) + 0.6 * t),
                  fbm(p + 3.0 * q + vec2(8.3, 2.8) - 0.4 * t));
    return fbm(p + warp * r);
}

void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 p = (uv - 0.5) * vec2(RENDERSIZE.x / RENDERSIZE.y, 1.0) * 0.9;
    float t = beats * 0.045 + TIME * 0.01;
    float warp = 1.4 + bass * 1.2;
    float f = field(p, t, warp);
    // Satin: shade the field as a surface.
    float e = 0.012;
    float fx = field(p + vec2(e, 0.0), t, warp) - f;
    float fy = field(p + vec2(0.0, e), t, warp) - f;
    vec3 n = normalize(vec3(-fx / e, -fy / e, 2.4));
    vec3 l = normalize(vec3(-0.6, 0.5, 0.6));
    float diff = clamp(dot(n, l), 0.0, 1.0);
    float spec = pow(clamp(dot(reflect(-l, n), vec3(0.0, 0.0, 1.0)), 0.0, 1.0), 14.0);

    vec3 ink = vec3(0.03, 0.012, 0.025);
    vec3 deep = vec3(0.30, 0.03, 0.08);
    vec3 crimson = vec3(0.86, 0.11, 0.20);
    vec3 pale = vec3(1.0, 0.84, 0.80);
    float body = smoothstep(0.15, 0.7, f);
    vec3 c = mix(ink, deep, body);
    c = mix(c, crimson, smoothstep(0.45, 0.85, f) * (0.45 + 0.45 * energy));
    c *= 0.6 + 1.6 * diff;
    // The sheen travels: a soft band sweeps diagonally, brighter on the kick.
    float sweep = exp(-pow((p.x + p.y * 0.6) - mod(beats * 0.25, 6.0) + 3.0, 2.0) * 1.5);
    c += pale * spec * (0.35 + 0.35 * highs + 0.9 * punch * sweep + 0.25 * sweep);
    // Gentle vignette, filmic roll-off.
    c *= 1.0 - 0.55 * dot(uv - 0.5, uv - 0.5) * 2.0;
    c = c / (1.0 + c);
    c = hueRotate(c, hue);
    gl_FragColor = vec4(max(c, 0.0), 1.0);
}
