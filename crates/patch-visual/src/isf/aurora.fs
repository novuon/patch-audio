/*{
    "DESCRIPTION": "Northern lights over a dark ridge: curtains of light sway with the mids, the bass lifts them, stars twinkle on the highs.",
    "CREDIT": "Patch (MIT)",
    "ISFVSN": "2",
    "CATEGORIES": ["Generator"],
    "INPUTS": [
        {"NAME": "energy", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5},
        {"NAME": "bass", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.3},
        {"NAME": "mids", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.3},
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
    for (int i = 0; i < 4; i++) {
        v += a * vnoise(p);
        p = p * 2.03 + vec2(1.7, 9.2);
        a *= 0.5;
    }
    return v;
}

vec3 hueRotate(vec3 c, float h) {
    float a = h * 6.2831853;
    vec3 k = vec3(0.57735);
    return c * cos(a) + cross(k, c) * sin(a) + k * dot(k, c) * (1.0 - cos(a));
}

void main() {
    vec2 uv = isf_FragNormCoord;
    float aspect = RENDERSIZE.x / RENDERSIZE.y;
    vec2 p = vec2((uv.x - 0.5) * aspect, uv.y);
    float t = beats * 0.03 + TIME * 0.02;

    // Night sky.
    vec3 c = mix(vec3(0.004, 0.007, 0.018), vec3(0.012, 0.02, 0.05), uv.y);
    vec2 sg = p * 90.0;
    vec2 cell = floor(sg);
    float star = h21(cell);
    vec2 sp = fract(sg) - 0.5 - (vec2(h21(cell + 3.1), h21(cell + 7.7)) - 0.5) * 0.6;
    float twinkle = 0.6 + 0.4 * sin(TIME * (1.0 + star * 3.0) + star * 40.0);
    c += vec3(0.8, 0.85, 1.0) * step(0.985, star) * smoothstep(0.08, 0.0, length(sp))
        * twinkle * (0.25 + 0.5 * highs) * smoothstep(0.25, 0.6, uv.y);

    // Curtains: far layers first, each a bright lower edge fading upward.
    vec3 lights = vec3(0.0);
    for (int i = 0; i < 5; i++) {
        float d = float(i) / 4.0;
        float x = p.x * (0.9 + d * 0.5) + t * (0.6 + 0.3 * d) + d * 13.0;
        float base = 0.30 + 0.20 * fbm(vec2(x * 0.8, d * 3.0 + t * 0.4)) + bass * 0.08 - d * 0.06;
        float sway = (fbm(vec2(x * 2.5 + t, d * 7.0)) - 0.5) * (0.06 + 0.08 * mids);
        float dy = uv.y - base - sway;
        float rays = pow(vnoise(vec2(x * 28.0 + fbm(vec2(x * 3.0, t)) * 6.0, t * 0.8 + d * 5.0)), 2.0);
        float edge = smoothstep(-0.012, 0.004, dy);
        float fold = 0.35 + 0.65 * smoothstep(0.25, 0.75, fbm(vec2(x * 1.7 - t * 0.5, d * 4.0 + 2.0)));
        float body = edge * exp(-max(dy, 0.0) * (2.0 + 1.5 * d)) * (0.3 + 0.7 * rays) * fold;
        vec3 low = vec3(0.10, 0.92, 0.62);
        vec3 high = mix(vec3(0.42, 0.20, 0.95), vec3(0.86, 0.12, 0.26), d * 0.6);
        vec3 col = mix(low, high, smoothstep(0.0, 0.35, dy));
        lights += col * body * (1.0 - d * 0.55);
    }
    float gain = 0.26 + 0.3 * energy + 0.15 * punch;
    c += lights * gain;

    // The ridge, with a faint reflection of the light on its slope.
    c += vec3(0.02, 0.06, 0.06) * exp(-abs(uv.y - 0.2) * 9.0);
    float ridge = 0.14 + 0.07 * fbm(vec2(p.x * 1.6 + 4.0, 2.0)) + 0.03 * fbm(vec2(p.x * 6.0, 8.0));
    float land = smoothstep(ridge + 0.002, ridge - 0.002, uv.y);
    vec3 ground = vec3(0.006, 0.008, 0.012) + lights * gain * 0.06 * smoothstep(ridge - 0.08, ridge, uv.y);
    c = mix(c, ground, land);

    c = c / (1.0 + c);
    c = hueRotate(c, hue);
    gl_FragColor = vec4(max(c, 0.0), 1.0);
}
