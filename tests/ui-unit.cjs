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
const Toml = moduleBody('Toml');
assert.equal(Toml.parse('[keys]\nplay = "Space" # comment\n')['keys.play'], 'Space');
const Metar = moduleBody('Metar');
assert.equal(Metar.color('VFR'), '#00c000');
assert.equal(Metar.countFromConfig({'metar.count':999}), 0);
console.log('Pure UI location, config, key binding and METAR regressions PASS');
