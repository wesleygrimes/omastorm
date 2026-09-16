.pragma library
// Air quality helpers (issue #3, docs/protocol.md `aqi_*`). The engine
// carries raw concentrations and four labeled indices; the UI picks one
// scale and colors it. The band palette is the standard US EPA category
// colors — a data palette like the dBZ legend, not chrome — and every
// label names the scale it shows, since the scales are not comparable.
var SCALES = ["us", "european", "china", "india", "raw"];
var NAMES = { us: "US", european: "EU", china: "CN", india: "IN", raw: "RAW" };
// Six bands, best to worst: good, moderate, sensitive, unhealthy, very
// unhealthy, hazardous.
var BANDS = ["#00e400", "#ffde33", "#ff7e00", "#ff0000", "#8f3f97", "#7e0023"];
function validScale(value) { return SCALES.indexOf(String(value)) >= 0; }
// The band a US-scale index falls in (the other scales are close enough
// for color; the number beside the dot carries the exact meaning).
function band(index) {
    if (typeof index !== "number" || index < 0) return null;
    return BANDS[index <= 50 ? 0 : index <= 100 ? 1 : index <= 150 ? 2 : index <= 200 ? 3 : index <= 300 ? 4 : 5];
}
// The index a reading reports under one scale, or null for raw/absent.
function indexFor(aqi, scale) {
    if (!aqi) return null;
    if (scale === "us") return aqi.us_aqi != null ? aqi.us_aqi : null;
    if (scale === "european") return aqi.european_aqi != null ? aqi.european_aqi : null;
    if (scale === "china") return aqi.china_aqi != null ? aqi.china_aqi : null;
    if (scale === "india") return aqi.india_aqi != null ? aqi.india_aqi : null;
    return null;
}
// "AQI 148 (US)" or, with no index, "PM2.5 55".
function chipText(aqi, scale) {
    var index = indexFor(aqi, scale);
    if (index != null) return "AQI " + index + " (" + NAMES[scale] + ")";
    if (scale === "raw" && aqi.pm2_5 != null) return "PM2.5 " + Math.round(aqi.pm2_5);
    return scale.toUpperCase();
}
// µg/m³ by the reading's field names, in display order.
function pollutants(aqi) {
    var rows = [["PM2.5", aqi.pm2_5], ["PM10", aqi.pm10], ["O3", aqi.o3], ["NO2", aqi.no2], ["SO2", aqi.so2], ["CO", aqi.co]];
    var shown = [];
    for (var row of rows) if (typeof row[1] === "number") shown.push(row[0] + " " + Math.round(row[1] * 10) / 10);
    return shown.join(" · ");
}
