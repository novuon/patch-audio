/*{
    "DESCRIPTION": "Three glossy forms orbit and merge under studio light, a half turn every eight bars; the bass swells them, the kick lights their red rim.",
    "CREDIT": "Patch (MIT)",
    "ISFVSN": "2",
    "CATEGORIES": ["Generator"],
    "INPUTS": [
        {"NAME": "energy", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.5},
        {"NAME": "bass", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.3},
        {"NAME": "beats", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0},
        {"NAME": "punch", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0},
        {"NAME": "hue", "TYPE": "float", "MIN": 0.0, "MAX": 1.0, "DEFAULT": 0.0}
    ]
}*/

float smin(float a, float b, float k) {
    float h = clamp(0.5 + 0.5 * (b - a) / k, 0.0, 1.0);
    return mix(b, a, h) - k * h * (1.0 - h);
}

vec3 hueRotate(vec3 c, float h) {
    float a = h * 6.2831853;
    vec3 k = vec3(0.57735);
    return c * cos(a) + cross(k, c) * sin(a) + k * dot(k, c) * (1.0 - cos(a));
}

float orbitAngle() { return beats * 3.14159265 / 32.0 + TIME * 0.02; }

float sdf(vec3 p) {
    float a = orbitAngle();
    float rad = 0.52 + 0.12 * bass;
    float d = 1e3;
    for (int i = 0; i < 3; i++) {
        float fi = float(i);
        float b = a + fi * 2.0944;
        vec3 c = vec3(cos(b) * 0.85, sin(a * 1.3 + fi * 1.7) * 0.35, sin(b) * 0.6);
        d = smin(d, length(p - c) - rad * (0.85 + 0.15 * fi), 0.55);
    }
    return d;
}

vec3 normalAt(vec3 p) {
    vec2 e = vec2(0.0015, -0.0015);
    return normalize(e.xyy * sdf(p + e.xyy) + e.yyx * sdf(p + e.yyx)
        + e.yxy * sdf(p + e.yxy) + e.xxx * sdf(p + e.xxx));
}

// A dark studio with two soft boxes, seen in reflections.
vec3 studio(vec3 d) {
    vec3 c = vec3(0.012, 0.012, 0.018) + vec3(0.02, 0.02, 0.03) * max(d.y, 0.0);
    c += vec3(1.0, 0.95, 0.9) * smoothstep(0.75, 0.9, d.y) * smoothstep(0.6, 0.2, abs(d.x)) * 1.4;
    c += vec3(0.94, 0.14, 0.23) * smoothstep(0.5, 0.8, -d.z) * smoothstep(0.5, 0.0, abs(d.y - 0.1)) * 0.6;
    return c;
}

void main() {
    vec2 uv = isf_FragNormCoord;
    vec2 p = (uv - 0.5) * vec2(RENDERSIZE.x / RENDERSIZE.y, 1.0);
    vec3 ro = vec3(0.0, 0.15, 3.6);
    vec3 rd = normalize(vec3(p, -1.55));
    // Background: a dim floor glow and a faint halo behind the forms.
    vec3 c = vec3(0.008, 0.008, 0.012) + vec3(0.05, 0.012, 0.02) * exp(-length(p) * 2.5) * (0.5 + 0.5 * energy);
    float t = 0.0;
    bool hit = false;
    for (int i = 0; i < 72; i++) {
        float d = sdf(ro + rd * t);
        if (d < 0.001) { hit = true; break; }
        t += d * 0.9;
        if (t > 8.0) break;
    }
    if (hit) {
        vec3 pos = ro + rd * t;
        vec3 n = normalAt(pos);
        vec3 l = normalize(vec3(-0.4, 0.9, 0.5));
        float diff = clamp(dot(n, l), 0.0, 1.0);
        float fres = pow(1.0 - clamp(dot(n, -rd), 0.0, 1.0), 3.0);
        vec3 base = vec3(0.035, 0.03, 0.04);
        vec3 refl = studio(reflect(rd, n));
        c = base * (0.3 + diff) + refl * (0.12 + 0.75 * fres);
        float spec = pow(clamp(dot(reflect(-l, n), -rd), 0.0, 1.0), 60.0);
        c += vec3(1.0, 0.95, 0.9) * spec * 0.9;
        c += vec3(0.94, 0.14, 0.23) * fres * (0.25 + 0.9 * punch + 0.3 * energy);
    }
    c *= 1.0 - 0.45 * dot(uv - 0.5, uv - 0.5) * 2.0;
    c = c / (1.0 + c);
    c = hueRotate(c, hue);
    gl_FragColor = vec4(max(c, 0.0), 1.0);
}
