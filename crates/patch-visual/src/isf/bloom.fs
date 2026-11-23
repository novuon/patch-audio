/*{
    "DESCRIPTION": "A slow nebula: layered gas lit from within, opening with the music's energy; distant stars drift past and the kick warms the core.",
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
    for (int i = 0; i < 6; i++) {
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

float stars(vec2 p, float scale, float seed) {
    vec2 g = p * scale;
    vec2 cell = floor(g);
    float r = h21(cell + seed);
    vec2 sp = fract(g) - 0.5 - (vec2(h21(cell + seed + 1.3), h21(cell + seed + 4.1)) - 0.5) * 0.7;
    return step(0.97, r) * smoothstep(0.07, 0.0, length(sp)) * (0.5 + 0.5 * r);
}

void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 p = (uv - 0.5) * vec2(RENDERSIZE.x / RENDERSIZE.y, 1.0);
    float t = beats * 0.01 + TIME * 0.008;
    vec3 c = vec3(0.004, 0.004, 0.009);
    c += vec3(0.7, 0.75, 0.9) * stars(p + vec2(t * 0.2, 0.0), 70.0, 0.0) * (0.35 + 0.4 * highs);
    c += vec3(0.9, 0.85, 0.8) * stars(p + vec2(t * 0.5, 0.0), 35.0, 9.0) * 0.6;

    vec2 q = p * 1.3 + vec2(2.0, 0.5);
    vec2 warp = vec2(fbm(q + vec2(0.0, t)), fbm(q + vec2(5.2, -t)));
    float gas = fbm(q + warp * (1.4 + bass * 0.6) + vec2(t * 0.6, 0.0));
    float dust = fbm(q * 2.2 - warp + 3.0);
    float core = exp(-length(p * vec2(0.7, 1.1)) * 1.6);
    float density = smoothstep(0.25, 0.85, gas) * (0.55 + 0.45 * core);
    float open = 0.45 + 0.55 * energy;

    vec3 violet = vec3(0.10, 0.04, 0.20);
    vec3 crimson = vec3(0.75, 0.09, 0.17);
    vec3 peach = vec3(1.0, 0.72, 0.58);
    vec3 neb = mix(violet, crimson, smoothstep(0.2, 0.7, density));
    neb = mix(neb, peach, smoothstep(0.65, 1.0, density * core * 1.6) * (0.5 + 0.5 * punch));
    neb *= density * open * 2.2;
    // Dark lanes of dust across the gas.
    neb *= 1.0 - 0.65 * smoothstep(0.55, 0.8, dust);
    c = c * (1.0 - density * 0.8) + neb;
    c += peach * core * core * 0.08 * (0.4 + punch);

    c = c / (1.0 + c);
    c = hueRotate(c, hue);
    gl_FragColor = vec4(max(c, 0.0), 1.0);
}
