/*{
    "DESCRIPTION": "Fine rings leave the centre on every beat across a tilted plane, cooling from white to red as they travel; the bass thickens them, the kick lights the core.",
    "CREDIT": "Patch (MIT)",
    "ISFVSN": "2",
    "CATEGORIES": ["Generator"],
    "INPUTS": [
        {"NAME": "phase", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0},
        {"NAME": "bass", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.3},
        {"NAME": "level", "TYPE": "float", "MIN": 0.2, "MAX": 1.0, "DEFAULT": 0.6},
        {"NAME": "punch", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0},
        {"NAME": "rings", "TYPE": "long", "DEFAULT": 8},
        {"NAME": "hue", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0}
    ]
}*/

vec3 hueRotate(vec3 c, float h) {
    float a = h * 6.2831853;
    vec3 k = vec3(0.57735);
    return c * cos(a) + cross(k, c) * sin(a) + k * dot(k, c) * (1.0 - cos(a));
}

void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 p = (uv - 0.5) * vec2(RENDERSIZE.x / RENDERSIZE.y, 1.0);
    // A plane seen from slightly above: rings become ellipses, nearer below.
    vec2 q = vec2(p.x, (p.y + 0.08) * 2.6);
    float r = length(q);
    float px = 1.5 / RENDERSIZE.y;
    vec3 c = vec3(0.008, 0.008, 0.013);
    vec3 white = vec3(1.0, 0.94, 0.9);
    vec3 red = vec3(0.94, 0.14, 0.23);
    int n = rings > 0 ? rings : 8;
    for (int i = 0; i < 12; i++) {
        if (i >= n) break;
        float age = (float(i) + phase) / float(n);
        float radius = age * 1.5;
        float width = px * (1.2 + 3.0 * bass) * (1.0 + radius * 2.0);
        float d = abs(r - radius);
        float lineA = smoothstep(width, 0.0, d);
        float halo = exp(-d * 40.0) * 0.12;
        float fade = pow(1.0 - age, 1.6) * smoothstep(0.0, 0.04, age);
        vec3 col = mix(white, red, smoothstep(0.1, 0.7, age));
        c += col * (lineA * 1.6 + halo * 1.5) * fade * level;
    }
    // The core.
    c += mix(red, white, 0.4) * exp(-r * 18.0) * (0.25 + 1.2 * punch);
    // Horizon fade so the plane recedes.
    c *= smoothstep(0.62, 0.35, uv.y) * 0.85 + 0.15;
    c = c / (1.0 + c);
    c = hueRotate(c, hue);
    gl_FragColor = vec4(max(c, 0.0), 1.0);
}
