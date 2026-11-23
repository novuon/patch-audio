/*{
    "DESCRIPTION": "Stage light through haze: slow beams sweep from above, dust drifts through them; the bass widens them and the kick flares the rig.",
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
    for (int i = 0; i < 4; i++) {
        v += a * vnoise(p);
        p = p * 2.07 + vec2(3.1, 1.7);
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
    vec2 p = vec2((uv.x - 0.5) * aspect, uv.y - 0.5);
    float t = beats * 0.0625;
    vec2 o = vec2(0.0, 0.62);
    vec2 d = p - o;
    float ang = atan(d.x, -d.y);
    float dist = length(d);

    float haze = 0.55 + 0.45 * fbm(p * 2.2 + vec2(TIME * 0.03, -TIME * 0.05));
    float width = 0.035 + 0.035 * bass;
    float beams = 0.0;
    float warm = 0.0;
    for (int i = 0; i < 7; i++) {
        float fi = float(i);
        float a = (fi - 3.0) * 0.16 + 0.22 * sin(t * 3.14159 * 0.5 + fi * 1.7) * (0.6 + 0.4 * sin(fi));
        float b = exp(-pow((ang - a) / width, 2.0)) * (0.6 + 0.4 * vnoise(vec2(fi * 7.0, TIME * 0.4)));
        beams += b;
        warm += b * step(0.5, fract(fi * 0.5));
    }
    float fall = exp(-dist * 0.9) * smoothstep(0.0, 0.08, dist);
    float flare = 0.55 + 0.35 * energy + 0.9 * punch;
    float light = beams * fall * haze * flare;

    vec3 white = vec3(1.0, 0.93, 0.86);
    vec3 red = vec3(0.94, 0.14, 0.23);
    vec3 c = vec3(0.008, 0.008, 0.014);
    c += mix(white, red, clamp(warm / max(beams, 1e-3), 0.0, 1.0) * 0.75) * light * 1.1;
    c += vec3(0.05, 0.02, 0.03) * haze * exp(-dist * 1.2);
    // The rig glows where the beams leave it.
    c += red * 0.25 * exp(-length(vec2(d.x * 0.5, d.y * 3.0)) * 6.0) * (0.4 + punch);

    // Dust in the light.
    vec2 g = (p + vec2(0.0, TIME * 0.012)) * 55.0;
    vec2 cell = floor(g);
    float r = h21(cell);
    vec2 sp = fract(g) - 0.5 - (vec2(h21(cell + 1.3), h21(cell + 4.1)) - 0.5) * 0.7;
    float mote = step(0.93, r) * smoothstep(0.09, 0.0, length(sp));
    c += white * mote * light * (1.2 + 1.5 * highs);

    c *= 1.0 - 0.4 * dot(uv - 0.5, uv - 0.5) * 2.0;
    c = c / (1.0 + c);
    c = hueRotate(c, hue);
    gl_FragColor = vec4(max(c, 0.0), 1.0);
}
