#version 440
layout(location = 0) in vec2 qt_TexCoord0;
layout(location = 0) out vec4 fragColor;
// The map frame is defined once, here: Web Mercator over the whole network,
// shared with the tile layer and the overlay. The camera arrives as the view
// centre's offset from the radar site in Mercator units (the unit square is
// the world) so that a pixel's position relative to the site keeps float
// precision; the site's latitude turns that offset into ground distance and
// azimuth on a sphere.
layout(std140, binding = 0) uniform buf {
    mat4 qt_Matrix;
    float qt_Opacity;
    vec2 viewport;
    vec2 centerOffset;
    float unitsPerPixel;
    float siteLatDeg;
    int treatment;
    int bands;
    // Sweep geometry from frame state (docs/protocol.md).
    int rays;
    int gates;
    float firstGateM;
    float gateSpacingM;
    float elevationDeg;
    // Weak-return floor: measured codes 2..weakBelow-1 draw nothing (the
    // legend names the hidden dBZ). 0 draws every measured return.
    int weakBelow;
};
// The sweep: one row per radial in ascending azimuth, one texel per gate.
// R is palette class + 1 (0 draws nothing), G holds status bits (1 folded,
// 2 below threshold). Nearest sampling, no mipmaps.
layout(binding = 1) uniform sampler2D sweep;
// The frame's palette from socket state as a bands x 1 strip, sampled at texel
// centers so the radar and the legend share one source of color.
layout(binding = 2) uniform sampler2D swatches;
// 3600 x 1: entry i covers azimuth i / 10 degrees and names the row nearest
// its center as a little-endian 16-bit value in R and G.
layout(binding = 3) uniform sampler2D azimuthLut;
// Gates sit at slant range along the beam; the map is ground distance. The
// two are related on the 4/3 effective-radius earth exactly as pyart's
// antenna_to_cartesian places gates, which is how the golden reference is built.
// The sphere that turns longitude and latitude into ground distance, and the
// 4/3 effective radius the beam bends over.
const float R_M = 6371000.0;
const float EARTH_M = R_M * 4.0 / 3.0;
const float PI = 3.14159265358979;
// GLSL ES 100 (used by Qt on Wayland/EGL) cannot initialize constant arrays.
// Spell out the same 3x3 density mask so every packaged target compiles.
int densityAt(int slot) {
    if (slot == 0) return 0;
    if (slot == 1) return 7;
    if (slot == 2) return 3;
    if (slot == 3) return 6;
    if (slot == 4) return 4;
    if (slot == 5) return 8;
    if (slot == 6) return 2;
    if (slot == 7) return 5;
    return 1;
}
// Hyperbolics spelled with exp so every GLSL target qsb emits has them. For
// small arguments exp(x) - exp(-x) cancels to a few significant bits, so the
// small terms below take their series instead; the rendering test replays
// the same polynomials, and the GPU and CPU then agree past the row epsilon.
float cosh_(float x) { return 0.5 * (exp(x) + exp(-x)); }
float sinh_(float x) {
    if (abs(x) >= 1.0) return 0.5 * (exp(x) - exp(-x));
    float x2 = x * x;
    return x * (1.0 + x2 * (1.0 / 6.0 + x2 * (1.0 / 120.0 + x2 * (1.0 / 5040.0 + x2 / 362880.0))));
}
float sin_(float x) {
    if (abs(x) >= 1.0) return sin(x);
    float x2 = x * x;
    return x * (1.0 - x2 * (1.0 / 6.0 - x2 * (1.0 / 120.0 - x2 * (1.0 / 5040.0 - x2 / 362880.0))));
}
// Some GPU atan implementations move a bearing across a 0.1-degree LUT
// boundary. Reduce to |t| <= tan(pi/8); the alternating series' next term
// is < 1.9e-8 radians. This keeps the existing rendering-test tolerance.
float bearingAtan(float y, float x) {
    float ax = abs(x), ay = abs(y);
    float t = min(ax, ay) / max(max(ax, ay), 1e-30);
    bool reduce = t > 0.414213562373095;
    if (reduce) t = (t - 1.0) / (t + 1.0);
    float t2 = t*t;
    float a = t*(1.0-t2*(1.0/3.0-t2*(1.0/5.0-t2*(1.0/7.0-t2*(1.0/9.0-t2*(1.0/11.0-t2*(1.0/13.0-t2/15.0)))))));
    if (reduce) a += PI*.25;
    if (ay > ax) a = PI*.5-a;
    if (x < 0.0) a = PI-a;
    return y < 0.0 ? -a : a;
}
void main() {
    // Every treatment paints 3 px screen cells; each cell samples the gate
    // under its center, so the lookup below runs once per cell, not per texel.
    vec2 pixel = qt_TexCoord0 * viewport;
    vec2 samplePixel = floor(pixel / 3.0) * 3.0 + 1.5;
    if (gates <= 0 || rays <= 0) { fragColor=vec4(0); return; }
    // The cell's Mercator offset from the site: x east, y south (tile rows
    // grow southward). In radians of longitude and of isometric latitude.
    vec2 d = centerOffset + (samplePixel - viewport * .5) * unitsPerPixel;
    float dLon = d.x * 2.0 * PI;
    float dPsi = -d.y * 2.0 * PI;
    // Latitude difference without subtracting two large latitudes:
    // atan(a) - atan(b) = atan((a - b) / (1 + a b)) with a = sinh(psi),
    // b = sinh(psi0) = tan(lat0), and sinh(psi) - sinh(psi0) factored.
    float lat0 = radians(siteLatDeg);
    float psi0 = log(tan(lat0) + 1.0 / cos(lat0));
    float dLat = bearingAtan(2.0 * cosh_(psi0 + dPsi * .5) * sinh_(dPsi * .5),
                            1.0 + tan(lat0) * sinh_(psi0 + dPsi));
    // Far from the site subtraction is well-conditioned and avoids the
    // quotient identity's lost quadrant in the opposite hemisphere.
    if (abs(dPsi) >= 1.0) dLat = atan(sinh_(psi0 + dPsi)) - lat0;
    float lat = lat0 + dLat;
    // Great-circle distance (haversine) and initial bearing from the site,
    // both written in the small differences so nearby cells stay exact.
    float sdLat = sin_(dLat * .5), sdLon = sin_(dLon * .5);
    float h = clamp(sdLat * sdLat + cos(lat0) * cos(lat) * sdLon * sdLon, 0.0, 1.0);
    float groundM = 2.0 * R_M * atan(sqrt(h), sqrt(max(0.0, 1.0 - h)));
    float azimuth = degrees(bearingAtan(sin_(dLon) * cos(lat),
                                 sin_(dLat) + sin(lat0) * cos(lat) * 2.0 * sdLon * sdLon));
    if (azimuth < 0.0) azimuth += 360.0;
    // Ground distance to slant range, closed form: r = R sin(s/R) / cos(e + s/R).
    float arc = groundM / EARTH_M;
    if (radians(elevationDeg) + arc >= PI * .5) { fragColor=vec4(0); return; }
    float slantM = EARTH_M * sin(arc) / cos(radians(elevationDeg) + arc);
    float gate = (slantM - firstGateM) / gateSpacingM;
    // Nearest gate. More than half a gate before the first or past the last
    // is outside the sweep: nothing to draw, never a weak return.
    if (gate < -0.5 || gate >= float(gates) - 0.5) { fragColor=vec4(0); return; }
    // Azimuth clockwise from north, in tenths of a degree, names the row.
    float entry = clamp(floor(azimuth * 10.0), 0.0, 3599.0);
    vec4 lut = texture(azimuthLut, vec2((entry + .5) / 3600.0, .5));
    float row = floor(lut.r * 255.0 + .5) + 256.0 * floor(lut.g * 255.0 + .5);
    vec2 uv = vec2((floor(gate + .5) + .5) / float(gates), (row + .5) / float(rays));
    vec4 code = texture(sweep, uv);
    // The raw moment byte in B decides the floor, so a floor can sit inside a
    // palette band; folded and below-threshold codes (0, 1) are never weak.
    int raw = int(round(code.b * 255.0));
    if (weakBelow > 0 && raw >= 2 && raw < weakBelow) { fragColor=vec4(0); return; }
    int value = int(round(code.r * 255.0));
    int status = int(round(code.g * 255.0));
    vec2 phase = mod(pixel,3.0);
    if (value == 0) {
        // Folded is a two-tone X in every treatment, never an intensity swatch.
        // Its opaque dark backing keeps the marker legible in light themes too.
        // Bit 1 without bitwise operators, which the legacy GLSL targets lack.
        if (status - 2 * (status / 2) == 1) {
            ivec2 p = ivec2(floor(phase));
            bool cross = p.x == p.y || p.x + p.y == 2;
            vec3 color = cross ? vec3(245) : vec3(24);
            fragColor=vec4(color/255.0,1.0)*qt_Opacity;
        } else {
            fragColor=vec4(0);
        }
        return;
    }
    int b=value-1;
    if (b >= bands) { fragColor=vec4(0); return; }
    // Treatments grade coverage by quartile of the palette, whatever its length.
    int group=(b*4)/bands;
    float alpha=1.0;
    if (treatment == 1) {
        int count=group==0 ? 2 : group==1 ? 4 : group==2 ? 7 : 9;
        int slot=int(floor(phase.y))*3+int(floor(phase.x));
        alpha=densityAt(slot)<count ? 1.0 : 0.0;
    } else if (treatment == 2) {
        float side=group==0 ? 1.75 : group==1 ? 2.0 : group==2 ? 2.25 : 2.5;
        vec2 coverage=clamp(vec2(side*.5+.5)-abs(phase-1.5),0.0,1.0);
        alpha=coverage.x*coverage.y;
    }
    vec3 color=texture(swatches, vec2((float(b)+.5)/float(bands), .5)).rgb;
    fragColor=vec4(color*alpha,alpha)*qt_Opacity;
}
