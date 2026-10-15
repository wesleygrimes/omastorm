// Pure QML JS modules: Node evaluates the body after Qt's library directive.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const os = require('node:os');
const path = require('node:path');
const {spawnSync} = require('node:child_process');
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

// Run the picker's actual paste entry point without starting a QML process.
const pickerSource = fs.readFileSync('ui/LocationPicker.qml', 'utf8');
const pasteFunction = pickerSource.match(/^    function pasteQuery\(primary\) \{[\s\S]*?^    \}/m);
assert.ok(pasteFunction, 'Picker paste entry point missing');
const picker = {open: false, manual: true, field: {activeFocus: true}, paste: {running: false}, pastePending: false};
const pasteQuery = vm.runInNewContext(`(${pasteFunction[0]})`, picker);
for (const state of [
    {open: false, manual: true, focused: true, running: false},
    {open: true, manual: false, focused: true, running: false},
    {open: true, manual: true, focused: false, running: false},
    {open: true, manual: true, focused: true, running: true}
]) {
    Object.assign(picker, {open: state.open, manual: state.manual, pastePending: false});
    picker.field.activeFocus = state.focused;
    picker.paste = {running: state.running};
    pasteQuery(false);
    assert.equal(picker.paste.command, undefined, 'Inactive or busy picker read clipboard');
    assert.equal(picker.pastePending, false);
}
Object.assign(picker, {open: true, manual: true});
picker.field.activeFocus = true;
picker.paste = {running: false};
pasteQuery(false);
assert.equal(picker.paste.running, true);
assert.equal(picker.pastePending, true);
const command = Array.from(picker.paste.command);
picker.paste.running = false;
pasteQuery(true);
const primaryCommand = Array.from(picker.paste.command);

// Exercise the exact production pipeline with owned fake clipboard producers.
// Omarchy supplies GNU timeout; macOS can still run the pure UI regressions.
if (spawnSync(command[0], ['--version']).error?.code === 'ENOENT') {
    console.log('Clipboard pipeline regressions unavailable: GNU timeout is required');
} else {
    const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'omastorm-clipboard-'));
    try {
        const clipboard = path.join(scratch, 'wl-paste');
        const runPaste = (body, argv = command) => {
            fs.writeFileSync(clipboard, '#!/bin/sh\n' + body + '\n', {mode: 0o700});
            const result = spawnSync(argv[0], argv.slice(1), {
                env: {...process.env, PATH: scratch + path.delimiter + process.env.PATH},
                timeout: 3000, maxBuffer: 8192
            });
            assert.ifError(result.error);
            return result;
        };
        assert.equal(runPaste('printf "36.23708, -79.97948"').stdout.toString(), '36.23708, -79.97948');
        const oversized = runPaste('exec yes coordinates');
        assert.equal(oversized.status, 0);
        assert.equal(oversized.stdout.length, 4096, 'Clipboard crossed the byte cap');
        const stalled = runPaste('exec sleep 60');
        assert.notEqual(stalled.status, 0, 'Stalled clipboard did not hit its deadline');
        assert.equal(stalled.stdout.length, 0);
        const stubborn = runPaste("trap '' TERM\nexec sleep 60");
        assert.notEqual(stubborn.status, 0, 'Clipboard ignoring TERM escaped the kill deadline');
        assert.equal(runPaste('printf "%s" "$*"', primaryCommand).stdout.toString(), '--no-newline --type text --primary');
        console.log('Bounded clipboard bytes, deadlines and primary paste regressions PASS');
    } finally {
        fs.rmSync(scratch, {recursive: true, force: true});
    }
}
console.log('Pure UI location, config, key binding and METAR regressions PASS');
