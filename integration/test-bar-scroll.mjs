#!/usr/bin/env node
// Exercise the scroll function from an installed/generated Patch audio panel.
// node integration/test-bar-scroll.mjs /path/to/patch.audio/Panel.qml
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';

const path = process.argv[2];
assert(path, 'pass the generated Panel.qml');
const qml = fs.readFileSync(path, 'utf8');
const start = qml.indexOf('function ensureCursorVisible(item) {');
const end = qml.indexOf('function clampCursor()', start);
assert(start >= 0 && end > start, 'audio panel scroll function not found');
const flick = { contentY: 350, contentHeight: 1400, height: 200, contentItem: {} };
const scrollArea = { contentItem: flick };
const root = { patchKeyboardNavigation: false, focusSection: 'output', selectedIndex: -1 };
// Both eager (4.4) and lazy (4.5) panel layouts must work.
const context = { root, scrollArea, panel: { loadedContent: { scrollArea } }, Style: { space: n => n } };
vm.createContext(context);
vm.runInContext(qml.slice(start, end), context);
function reveal(keyboard, y, expected, message) {
    root.patchKeyboardNavigation = keyboard;
    flick.contentY = 350;
    context.ensureCursorVisible({ height: 40, mapToItem: () => ({ y }) });
    assert.equal(flick.contentY, expected, message);
}
reveal(false, 500, 350, 'hovering the output slider must not undo wheel scrolling');
reveal(false, 1000, 350, 'hovering a lower row must not take control of scrolling');
reveal(true, 500, 350, 'an already visible output slider must not jump to the top');
reveal(true, 1000, 846, 'keyboard navigation must reveal a lower row');
reveal(true, 100, 94, 'keyboard navigation must reveal an upper row');
root.focusSection = 'input';
reveal(true, 1000, 846, 'input keyboard navigation must reveal its row');
// Small overflow still needs to scroll; the old 24 px exception cut it off.
flick.contentHeight = 220;
flick.contentY = 0;
context.ensureCursorVisible({ height: 20, mapToItem: () => ({ y: 200 }) });
assert.equal(flick.contentY, 20, 'the last few pixels must remain reachable');
assert(qml.includes('root.patchKeyboardNavigation = true'), 'keyboard handler must enable reveal');
for (const hook of ['onHoveredChanged: if (hovered) {', 'onContainsMouseChanged: if (containsMouse) {']) {
    const callbacks = qml.split(hook).slice(1);
    assert(callbacks.length > 0, 'hover callback not found: ' + hook);
    for (const callback of callbacks) {
        assert.equal(callback.trimStart().split('\n')[0], 'root.patchKeyboardNavigation = false',
            'hover must yield scroll ownership before changing the cursor');
    }
}
const cap = qml.match(/readonly property real patchMenuHeightCap:\s*\{([\s\S]*?)\n  \}/);
assert(cap, 'available-height binding not found');
function heightCap(panel) {
    return vm.runInNewContext('(function() {' + cap[1] + '})()', { panel, Style: { space: n => n } });
}
for (const [height, available] of [[1624, 1580], [1350, 1306], [1080, 1036], [800, 756], [640, 596]]) {
    assert.equal(heightCap({ screen: { height }, availableCardHeight: available }), Math.min(820, available),
        'menu stays bounded and scrollable while fitting smaller screens');
}
assert.equal(heightCap({ screen: { height: 810 } }), 730, 'older shells reserve screen margins');
assert.equal(heightCap({ screen: null }), 820, 'unmapped screen has a bounded fallback');
console.log('Bar scrolling: 7 behavior checks and 7 bounded screen-height checks passed; keyboard/mouse ownership verified.');
