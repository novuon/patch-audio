#!/usr/bin/env node
// Exercise the real menu slider input handlers without a live audio device.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
const qml = fs.readFileSync(process.argv[2] || 'integration/omarchy-shell/DragPanelSlider.qml', 'utf8');
function body(marker) {
  const start = qml.indexOf(marker);
  assert(start >= 0, 'missing slider handler: ' + marker);
  const open = qml.indexOf('{', start);
  let depth = 1, end = open + 1;
  for (; depth && end < qml.length; end++) {
    if (qml[end] === '{') depth++;
    if (qml[end] === '}') depth--;
  }
  assert.equal(depth, 0);
  return qml.slice(open + 1, end - 1);
}
const moved = [], released = [];
const root = {minimum: 0, maximum: 1, range: 1, value: 0.4, liveValue: 0.4,
  integer: false, mirrored: false, dragging: false,
  moved: value => moved.push(value), released: value => released.push(value)};
const context = {root, track: {x: 0, width: 100}, armed: false, pressX: 0, pressY: 0,
  Qt: {LeftButton: 1, RightButton: 2, styleHints: {startDragDistance: 10}}};
vm.createContext(context);
for (const [name, args, marker] of [
  ['valueFromX','x','function valueFromX(x)'],
  ['press','mouse','onPressed: function(mouse)'],
  ['move','mouse','onPositionChanged: function(mouse)'],
  ['release','mouse','onReleased: function(mouse)'],
  ['wheel','wheel','onWheel: function(wheel)'],
  ['cancel','','onCanceled: {'],
]) vm.runInContext(`function ${name}(${args}) {${body(marker)}}`, context);
for (const angleDelta of [{x: 0, y: 120}, {x: 120, y: 0}, {x: 0, y: -120}]) {
  const wheel = {angleDelta, accepted: true};
  context.wheel(wheel);
  assert.equal(wheel.accepted, false, 'scroll must reach the surrounding menu');
}
assert.deepEqual(moved, []);
assert.deepEqual(released, []);
assert.equal(root.liveValue, 0.4);
context.press({button: 1, x: 20, y: 5});
context.move({buttons: 1, x: 22, y: 60});
context.cancel();
assert.deepEqual(moved, [], 'vertical scrolling gestures must not change volume');
assert.deepEqual(released, []);
context.press({button: 1, x: 90, y: 5});
context.release({button: 1});
assert.deepEqual(moved, [], 'a click alone is not a horizontal drag');
context.press({button: 1, x: 40, y: 5});
context.move({buttons: 1, x: 45, y: 6});
assert.deepEqual(moved, [], 'small pointer jitter must not change volume');
context.move({buttons: 1, x: 80, y: 7});
assert.equal(root.dragging, true);
assert.deepEqual(moved, [0.8], 'horizontal dragging changes volume');
context.release({button: 1});
assert.deepEqual(released, [0.8], 'release commits the drag once');
assert.equal(root.dragging, false);
console.log('Menu slider: wheel pass-through, vertical gestures, clicks, jitter, horizontal drag and release passed.');
