#version 440
layout(location = 0) in vec2 qt_TexCoord0;
layout(location = 0) out vec4 fragColor;
// GridFamily sampling (docs/grid-adapters.md): Web Mercator cell → WGS84
// lon/lat → optional inverse Helmert → native CRS → inverse affine → pixel.
// Reject outside [0, width) × [0, height). Polar lookup lives in radar.frag.
layout(std140, binding = 0) uniform buf {
    mat4 qt_Matrix;
    float qt_Opacity;
    vec2 viewport;
    vec2 viewCenter;
    float unitsPerPixel;
    int treatment;
    int bands;
    int weakBelow;
    int crsKind;
    int gridWidth;
    int gridHeight;
    int hasDatum;
    float semiMajorM;
    float invFlattening;
    float lon0Deg;
    float lat0Deg;
    float stdParallel1Deg;
    float stdParallel2Deg;
    float projScale;
    float falseEastingM;
    float falseNorthingM;
    float helmertS;
    vec3 helmertT;
    vec3 helmertR;
    vec3 geoA;
    vec3 geoB;
};
layout(binding = 1) uniform sampler2D sweep;
layout(binding = 2) uniform sampler2D swatches;

const float PI = 3.14159265358979;
const float WGS84_A = 6378137.0;
const float WGS84_F = 1.0 / 298.257223563;
const float ARCSEC = PI / (180.0 * 3600.0);

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

float sinh_(float x) {
    if (abs(x) >= 1.0) return 0.5 * (exp(x) - exp(-x));
    float x2 = x * x;
    return x * (1.0 + x2 * (1.0 / 6.0 + x2 * (1.0 / 120.0 + x2 * (1.0 / 5040.0 + x2 / 362880.0))));
}

float eccentricity2(float f) { return f * (2.0 - f); }

float conformalT(float phi, float e) {
    float s = clamp(sin(phi), -0.999999, 0.999999);
    float es = e * s;
    return tan(PI * 0.25 - phi * 0.5) / pow((1.0 - es) / (1.0 + es), e * 0.5);
}

float meridianM(float phi, float a, float e2) {
    float e4 = e2 * e2;
    float e6 = e4 * e2;
    float A0 = 1.0 - e2 / 4.0 - 3.0 * e4 / 64.0 - 5.0 * e6 / 256.0;
    float A2 = 3.0 / 8.0 * (e2 + e4 / 4.0 + 15.0 * e6 / 128.0);
    float A4 = 15.0 / 256.0 * (e4 + 3.0 * e6 / 4.0);
    float A6 = 35.0 * e6 / 3072.0;
    return a * (A0 * phi - A2 * sin(2.0 * phi) + A4 * sin(4.0 * phi) - A6 * sin(6.0 * phi));
}

vec3 geodeticToEcef(float lat, float lon, float a, float f) {
    float e2 = eccentricity2(f);
    float s = sin(lat), c = cos(lat);
    float n = a / sqrt(max(1e-12, 1.0 - e2 * s * s));
    return vec3((n) * c * cos(lon), (n) * c * sin(lon), (n * (1.0 - e2)) * s);
}

vec2 ecefToGeodetic(vec3 p, float a, float f) {
    float b = a * (1.0 - f);
    float e2 = eccentricity2(f);
    float ep2 = (a * a - b * b) / max(1e-12, b * b);
    float rho = length(p.xy);
    float theta = atan(p.z * a, rho * b);
    float st = sin(theta), ct = cos(theta);
    float lat = atan(p.z + ep2 * b * st * st * st, rho - e2 * a * ct * ct * ct);
    float lon = atan(p.y, p.x);
    return vec2(lat, lon);
}

vec2 applyInverseHelmert(float lat, float lon, float a, float f) {
    if (hasDatum == 0) return vec2(lat, lon);
    vec3 xyz = geodeticToEcef(lat, lon, WGS84_A, WGS84_F);
    vec3 t = helmertT;
    vec3 r = helmertR * ARCSEC;
    float k = 1.0 + helmertS * 1e-6;
    vec3 d = xyz - t;
    // Inverse of position-vector X_wgs = k R X + T, with R small-angle.
    vec3 src = vec3(
        d.x + r.z * d.y - r.y * d.z,
        -r.z * d.x + d.y + r.x * d.z,
        r.y * d.x - r.x * d.y + d.z
    ) / k;
    return ecefToGeodetic(src, a, f);
}

vec2 projectCrs(float lat, float lon, float a, float f, float e) {
    float e2 = e * e;
    float k0 = projScale;
    float fe = falseEastingM;
    float fn = falseNorthingM;
    float lam0 = radians(lon0Deg);
    float phi0 = radians(lat0Deg);
    float lam = lon - lam0;
    // Wrap longitude difference to (-pi, pi].
    lam = lam - 2.0 * PI * floor((lam + PI) / (2.0 * PI));

    if (crsKind == 0) {
        return vec2(degrees(lon), degrees(lat));
    }
    if (crsKind == 1) {
        // Mercator: y = fn - k0 a ln(t), t = tan(π/4-φ/2)/[(1-e s)/(1+e s)]^(e/2).
        float t = conformalT(lat, e);
        return vec2(fe + k0 * a * lam, fn - k0 * a * log(max(t, 1e-30)));
    }
    if (crsKind == 2) {
        float s = sin(lat), c = cos(lat);
        float n = a / sqrt(max(1e-12, 1.0 - e2 * s * s));
        float t2 = s / max(abs(c), 1e-12);
        t2 = t2 * t2;
        float ep2 = e2 / max(1e-12, 1.0 - e2);
        float C = ep2 * c * c;
        float A = lam * c;
        float M = meridianM(lat, a, e2);
        float M0 = meridianM(phi0, a, e2);
        float A2 = A * A, A3 = A2 * A, A4 = A2 * A2, A5 = A4 * A, A6 = A4 * A2;
        float x = fe + k0 * n * (A + (1.0 - t2 + C) * A3 / 6.0
            + (5.0 - 18.0 * t2 + t2 * t2 + 72.0 * C - 58.0 * ep2) * A5 / 120.0);
        float y = fn + k0 * (M - M0 + n * tan(lat) * (A2 / 2.0
            + (5.0 - t2 + 9.0 * C + 4.0 * C * C) * A4 / 24.0
            + (61.0 - 58.0 * t2 + t2 * t2 + 600.0 * C - 330.0 * ep2) * A6 / 720.0));
        return vec2(x, y);
    }
    if (crsKind == 3) {
        // South-pole origin uses Snyder's negated latitudes so t grows toward
        // the pole; easting keeps +sin(λ) and northing +cos(λ).
        float phi = phi0 < 0.0 ? -lat : lat;
        float phiOrigin = phi0 < 0.0 ? -phi0 : phi0;
        float t = conformalT(phi, e);
        float t0 = conformalT(phiOrigin, e);
        float m0 = cos(phiOrigin) / sqrt(max(1e-12, 1.0 - e2 * sin(phiOrigin) * sin(phiOrigin)));
        float rho = abs(m0) < 1e-8
            ? 2.0 * a * k0 * t / sqrt(pow(1.0 + e, 1.0 + e) * pow(1.0 - e, 1.0 - e))
            : a * k0 * m0 * t / max(t0, 1e-30);
        if (phi0 < 0.0) {
            return vec2(fe + rho * sin(lam), fn + rho * cos(lam));
        }
        return vec2(fe + rho * sin(lam), fn - rho * cos(lam));
    }
    if (crsKind == 4) {
        float phi1 = radians(stdParallel1Deg);
        float phi2 = radians(stdParallel2Deg);
        float m1 = cos(phi1) / sqrt(max(1e-12, 1.0 - e2 * sin(phi1) * sin(phi1)));
        float m2 = cos(phi2) / sqrt(max(1e-12, 1.0 - e2 * sin(phi2) * sin(phi2)));
        float t0 = conformalT(phi0, e);
        float t1 = conformalT(phi1, e);
        float t2 = conformalT(phi2, e);
        float n;
        if (abs(phi1 - phi2) < 1e-12) {
            n = sin(phi1);
        } else {
            n = log(m1 / max(m2, 1e-30)) / log(max(t1, 1e-30) / max(t2, 1e-30));
        }
        float F = m1 / (n * pow(max(t1, 1e-30), n));
        float rho = a * F * pow(max(conformalT(lat, e), 1e-30), n);
        float rho0 = a * F * pow(max(t0, 1e-30), n);
        float theta = n * lam;
        return vec2(fe + rho * sin(theta), fn + rho0 - rho * cos(theta));
    }
    // lambertAzimuthalEqualArea
    float qp = (1.0 - e2) * (1.0 / (1.0 - e2) - (1.0 / (2.0 * max(e, 1e-12)))
        * log((1.0 - e) / (1.0 + e)));
    if (e < 1e-8) qp = 2.0;
    float q_at = (1.0 - e2) * (sin(lat) / max(1e-12, 1.0 - e2 * sin(lat) * sin(lat))
        - (1.0 / (2.0 * max(e, 1e-12))) * log((1.0 - e * sin(lat)) / (1.0 + e * sin(lat))));
    float q0 = (1.0 - e2) * (sin(phi0) / max(1e-12, 1.0 - e2 * sin(phi0) * sin(phi0))
        - (1.0 / (2.0 * max(e, 1e-12))) * log((1.0 - e * sin(phi0)) / (1.0 + e * sin(phi0))));
    if (e < 1e-8) {
        q_at = 2.0 * sin(lat);
        q0 = 2.0 * sin(phi0);
    }
    float beta = asin(clamp(q_at / max(qp, 1e-12), -1.0, 1.0));
    float beta0 = asin(clamp(q0 / max(qp, 1e-12), -1.0, 1.0));
    float rq = a * sqrt(max(qp * 0.5, 1e-12));
    float m1 = cos(phi0) / sqrt(max(1e-12, 1.0 - e2 * sin(phi0) * sin(phi0)));
    // Oblique D = a m1 / (Rq cos β1). At a polar origin m1→0; use D=1 so the
    // polar aspect does not explode.
    float D = abs(cos(beta0)) < 1e-8 ? 1.0 : a * m1 / (rq * cos(beta0));
    float denom = 1.0 + sin(beta0) * sin(beta) + cos(beta0) * cos(beta) * cos(lam);
    float B = rq * sqrt(max(2.0 / max(denom, 1e-12), 0.0));
    return vec2(fe + B * D * cos(beta) * sin(lam),
                fn + (B / D) * (cos(beta0) * sin(beta) - sin(beta0) * cos(beta) * cos(lam)));
}

vec2 inverseAffine(vec2 xy) {
    float x0 = geoA.x, dx = geoA.y, rx = geoA.z;
    float y0 = geoB.x, ry = geoB.y, dy = geoB.z;
    float det = dx * dy - rx * ry;
    if (abs(det) < 1e-18) return vec2(-1.0);
    float dx_ = xy.x - x0;
    float dy_ = xy.y - y0;
    return vec2((dy * dx_ - rx * dy_) / det, (-ry * dx_ + dx * dy_) / det);
}

void main() {
    vec2 pixel = qt_TexCoord0 * viewport;
    vec2 samplePixel = floor(pixel / 3.0) * 3.0 + 1.5;
    if (gridWidth <= 0 || gridHeight <= 0 || crsKind < 0 || crsKind > 5) {
        fragColor = vec4(0);
        return;
    }
    vec2 merc = viewCenter + (samplePixel - viewport * 0.5) * unitsPerPixel;
    float lonDeg = merc.x * 360.0 - 180.0;
    float lat = degrees(atan(sinh_(PI * (1.0 - 2.0 * merc.y))));
    float lon = radians(lonDeg);
    lat = radians(lat);

    float a = max(semiMajorM, 1.0);
    float f = invFlattening > 1.0 ? 1.0 / invFlattening : 0.0;
    float e2 = eccentricity2(f);
    float e = sqrt(max(e2, 0.0));

    vec2 geodetic = applyInverseHelmert(lat, lon, a, f);
    vec2 xy = projectCrs(geodetic.x, geodetic.y, a, f, e);
    vec2 colrow = inverseAffine(xy);
    float col = colrow.x, row = colrow.y;
    if (col < 0.0 || row < 0.0 || col >= float(gridWidth) || row >= float(gridHeight)) {
        fragColor = vec4(0);
        return;
    }
    vec2 uv = vec2((floor(col) + 0.5) / float(gridWidth), (floor(row) + 0.5) / float(gridHeight));
    vec4 code = texture(sweep, uv);
    int raw = int(round(code.b * 255.0));
    if (weakBelow > 0 && raw >= 2 && raw < weakBelow) { fragColor = vec4(0); return; }
    int value = int(round(code.r * 255.0));
    // G bit 0 missing / bit 1 undetect — both draw nothing on a grid
    // (docs/grid-adapters.md: nodata draws nothing). Polar folded markers
    // stay in radar.frag; a continental mosaic must not hatch the oceans.
    if (value == 0) {
        fragColor = vec4(0);
        return;
    }
    vec2 phase = mod(pixel, 3.0);
    int b = value - 1;
    if (b >= bands) { fragColor = vec4(0); return; }
    int group = (b * 4) / bands;
    float alpha = 1.0;
    if (treatment == 1) {
        int count = group == 0 ? 2 : group == 1 ? 4 : group == 2 ? 7 : 9;
        int slot = int(floor(phase.y)) * 3 + int(floor(phase.x));
        alpha = densityAt(slot) < count ? 1.0 : 0.0;
    } else if (treatment == 2) {
        float side = group == 0 ? 1.75 : group == 1 ? 2.0 : group == 2 ? 2.25 : 2.5;
        vec2 coverage = clamp(vec2(side * 0.5 + 0.5) - abs(phase - 1.5), 0.0, 1.0);
        alpha = coverage.x * coverage.y;
    }
    vec3 color = texture(swatches, vec2((float(b) + 0.5) / float(bands), 0.5)).rgb;
    fragColor = vec4(color * alpha, alpha) * qt_Opacity;
}
