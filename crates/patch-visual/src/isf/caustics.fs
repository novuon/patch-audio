/*{
    "DESCRIPTION": "Light through moving water onto a deep floor: a slow web of caustics with rays from above; each kick sends a ripple out from the centre.",
    "CREDIT": "Patch (MIT)",
    "ISFVSN": "2",
    "CATEGORIES": ["Generator"],
    "INPUTS": [
        {"NAME": "energy", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5},
        {"NAME": "bass", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.3},
        {"NAME": "highs", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.2},
        {"NAME": "beats", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0},
        {"NAME": "phase", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0},
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

// Bright where a warped noise field crosses its mid level: thin, joined
// filaments like light focused by waves.
float web(vec2 p, float t) {
    vec2 w = vec2(fbm(p + vec2(t, 0.0)), fbm(p + vec2(3.7, t * 0.8)));
    float n = fbm(p * 1.3 + w * 1.6 - t * 0.3);
    return pow(1.0 - abs(2.0 * n - 1.0), 14.0);
}

void main() {
    vec2 uv = isf_FragNormCoord;
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = (uv - 0.5) * vec2(aspect, 1.0);
    // The kick's ripple: a ring leaves the centre each beat.
    float r = length(p);
    float ring = exp(-pow((r - phase * 1.1) * 14.0, 2.0)) * (1.0 - phase);
    vec2 q = p * 3.2 + normalize(p + 1e-4) * ring * (0.04 + 0.08 * punch);
    float t = beats * 0.03 + TIME * 0.04;
    float c1 = web(q, t);
    float c2 = web(q * 1.7 + 11.0, t * 1.3);
    float caustic = c1 * 0.8 + c2 * 0.5 + c1 * c2 * 1.5;

    vec3 deep = mix(vec3(0.0, 0.025, 0.05), vec3(0.0, 0.07, 0.11), uv.y);
    vec3 light = vec3(0.55, 0.92, 1.0);
    vec3 c = deep;
    c += light * caustic * (0.5 + 0.45 * energy + 0.2 * bass) * (0.6 + 0.4 * uv.y);
    // Shafts from the surface.
    float shaft = fbm(vec2(p.x * 3.0 + p.y * 0.8, TIME * 0.05));
    c += vec3(0.25, 0.55, 0.65) * pow(shaft, 3.0) * smoothstep(0.0, 1.0, uv.y) * 0.25;
    // Glints on the highs.
    c += light * step(0.97, vnoise(q * 14.0 + TIME)) * caustic * highs * 0.8;
    // A warm accent in the ripple, restrained.
    c += vec3(0.94, 0.14, 0.23) * ring * 0.12 * (0.3 + punch);

    c *= 1.0 - 0.6 * dot(uv - 0.5, uv - 0.5) * 2.0;
    c = c / (1.0 + c);
    c = hueRotate(c, hue);
    gl_FragColor = vec4(max(c, 0.0), 1.0);
}
