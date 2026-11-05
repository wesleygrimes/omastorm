// Pure QML JS modules: Node evaluates the body after Qt's library directive.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
function moduleBody(name) {
    const context = vm.createContext({});
    const source = fs.readFileSync(`ui/${name}.js`, 'utf8');
    assert.equal(source.split('\n')[0], '.pragma library');
    vm.runInContext(source.slice(source.indexOf('\n') + 1), context, {filename: name});
    return context;
}
const Location = moduleBody('Location');
assert.equal(Location.validPair(90, 180), true);
assert.equal(Location.validPair(null, 0), false);
assert.equal(Location.validPair(91, 0), false);
assert.equal(Location.parseState('{"lat":35.4,"lon":-97.5}').lat, 35.4);
assert.equal(Location.parseState('{"lat":"south","lock":1}').lat, undefined);
// A zoom keeps the centre (a Mercator round-trip is not a move); a pan moves it.
assert.equal(Location.sameCenter(35.4, -97.5, 35.39999999999999, -97.5), true);
assert.equal(Location.sameCenter(35.4, -97.5, 35.4, -97.6), false);
assert.equal(Location.parseWttrHome('{"nearest_area":[{"latitude":"41.05","longitude":"-73.54","areaName":[{"value":"Stamford"}]}]}').lat, 41.05);
assert.equal(Location.parseWttrHome('{}'), null);
const mosaics = [{id: 'opera', kind: 'mosaic', coverage: {kind: 'box', north: 72, south: 31, east: 45, west: -30}},
    {id: 'mrms-conus', kind: 'mosaic', coverage: {kind: 'box', north: 55, south: 20, east: -60, west: -130}}];
assert.equal(Location.rankMosaics(mosaics, '', 35.4, -97.5, true, '', 4, false).length, 0);
assert.equal(Location.rankMosaics(mosaics, 'mrms', 35.4, -97.5, true, '', 4, false)[0].id, 'mrms-conus');
assert.equal(Location.rankMosaics(mosaics, '', 35.4, -97.5, true, 'mrms-conus', 4, false)[0].id, 'mrms-conus');
assert.equal(Location.rankMosaics(mosaics, '', 50, 10, true, '', 4, false)[0].id, 'opera');
assert.equal(Location.coveringMosaic(mosaics, 35.4, -97.5).id, 'mrms-conus');
assert.equal(Location.coveringMosaic(mosaics, 61, -150), null);
const Keys = moduleBody('Keys');
assert.equal(Keys.resolve({play:'Space',unknown:'x'}, x=>x).errors.length, 1);
assert.equal(Keys.resolve({play:'h'}, x=>x).errors.length, 1);
assert.equal(Keys.envFloor('off'), null);
// gpsd: absent is off, a boolean passes, anything else is named and off.
for (const [value, on, errors] of [[undefined, false, 0], [true, true, 0], [false, false, 0], ['yes', false, 1], [1, false, 1]]) {
    const list = [];
    assert.equal(Keys.gpsd(value, list), on);
    assert.equal(list.length, errors);
}
const Toml = moduleBody('Toml');
// A quoted "true" is a string, so gpsd stays off and is named.
const quoted = [];
assert.equal(Keys.gpsd(Toml.parse('gpsd = "true"\n').gpsd, quoted), false);
assert.equal(quoted.length, 1);
assert.equal(Toml.parse('[keys]\nplay = "Space" # comment\n')['keys.play'], 'Space');
// popover_scale: absent is 1, valid passes, out of range clamps, anything else is 1; only absent and valid are silent.
function popoverScale(toml) {
    const errors = [];
    return {scale: Keys.popoverScale(Toml.parse(toml).popover_scale, errors), errors: errors.length};
}
assert.deepEqual(popoverScale(''), {scale: 1, errors: 0});
assert.deepEqual(popoverScale('treatment = "PIXELS"\n'), {scale: 1, errors: 0});
assert.deepEqual(popoverScale('popover_scale = 1.5\n'), {scale: 1.5, errors: 0});
assert.deepEqual(popoverScale('popover_scale = 2 # ultrawide\n'), {scale: 2, errors: 0});
assert.deepEqual(popoverScale('popover_scale = 0.75\n'), {scale: 0.75, errors: 0});
assert.deepEqual(popoverScale('popover_scale = 2.5\n'), {scale: 2.5, errors: 0});
assert.deepEqual(popoverScale('popover_scale = 9\n'), {scale: 2.5, errors: 1});
assert.deepEqual(popoverScale('popover_scale = 0.2\n'), {scale: 0.75, errors: 1});
assert.deepEqual(popoverScale('popover_scale = -1\n'), {scale: 0.75, errors: 1});
assert.deepEqual(popoverScale('popover_scale = "1.5"\n'), {scale: 1, errors: 1});
assert.deepEqual(popoverScale('popover_scale = true\n'), {scale: 1, errors: 1});
// Not in the scalar subset Toml.js reads, so it is absent rather than an error.
assert.deepEqual(popoverScale('popover_scale = [1.5]\n'), {scale: 1, errors: 0});
assert.equal(Keys.popoverScale(NaN, []), 1);
assert.equal(Keys.popoverScale(Infinity, []), 1);
assert.equal(Keys.popoverScale(null, []), 1);
// Whole pixels, and today's card and panel exactly at 1.
assert.equal(Keys.scaled(Keys.POPOVER_WIDTH, 1), 308);
assert.equal(Keys.scaled(Keys.POPOVER_HEIGHT, 1) + 28, 400);
assert.equal(Keys.scaled(Keys.POPOVER_WIDTH, 1.5), 462);
assert.equal(Keys.scaled(Keys.POPOVER_HEIGHT, 2), 744);
assert.equal(Keys.scaled(10, 1.25), 13);
const Metar = moduleBody('Metar');
assert.equal(Metar.color('VFR'), '#00c000');
assert.equal(Metar.countFromConfig({'metar.count':999}), 0);
// View export (docs/configuration.md): overlay writes only site/scan/live over the file on disk.
// Compared as JSON strings: the vm context has its own Object prototype, so deepEqual would fail on realm.
const json = (o) => JSON.stringify(o);
assert.equal(json(Location.overlay({lat: 35.4, lon: -97.5, span: 50, lock: {a: 1}, name: 'OKC', site: 'KFCX', scan: 'old', live: true}, 'KTLX', 'new', false, null)),
  json({lat: 35.4, lon: -97.5, span: 50, lock: {a: 1}, name: 'OKC', site: 'KTLX', live: false, scan: 'new'}), 'overlay keeps camera, lock and name, and key order');
assert.equal(json(Location.overlay({lat: 35.4, lon: -97.5, span: 50, live: true}, null, null, true, null)),
  json({lat: 35.4, lon: -97.5, span: 50}), 'a missing site drops live and scan');
assert.equal(json(Location.overlay({lat: 35.4, lon: -97.5, span: 50}, 'KTLX', 'new', true, {lat: 1, lon: 2, span: 9})),
  json({lat: 35.4, lon: -97.5, span: 50, site: 'KTLX', live: true, scan: 'new'}), 'a fallback centre is not applied over an existing centre');
assert.equal(json(Location.overlay({}, 'KTLX', 'new', true, {lat: 1, lon: 2, span: 9})),
  json({lat: 1, lon: 2, span: 9, site: 'KTLX', live: true, scan: 'new'}), 'the fallback centre applies when there is no existing centre');
assert.equal(json(Location.stateObject(35.4, -97.5, 50, null, 'OKC', 'KTLX', 'new', true)),
  json({lat: 35.4, lon: -97.5, span: 50, name: 'OKC', site: 'KTLX', scan: 'new', live: true}), 'stateObject writes site, scan and live when a site is set');
assert.equal(json(Location.stateObject(35.4, -97.5, 50, null, 'OKC', null, null, true)),
  json({lat: 35.4, lon: -97.5, span: 50, name: 'OKC'}), 'stateObject writes no live without a site');
console.log('Pure UI location, config, key binding and METAR regressions PASS');
