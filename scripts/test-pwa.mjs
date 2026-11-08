// Bounded installation UI tests, without depending on browser install eligibility.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import vm from 'node:vm';
const source = readFileSync(new URL('../packages/client/web/install.js', import.meta.url), 'utf8');
function setup({ios = false, installed = false} = {}) {
  const events = {};
  const locks = [];
  const screen = {orientation: {lock: orientation => {locks.push(orientation); return Promise.resolve();}}};
  const elements = Object.fromEntries(['#install-app', '#install-app-button', '#install-app-hint'].map(id => [id, {hidden: true, addEventListener(name, handler) {this[name] = handler;}}]));
  const media = {matches: installed, addEventListener() {}};
  vm.runInNewContext(source, {document: {querySelector: id => elements[id]}, navigator: {userAgent: ios ? 'iPhone' : 'Android', platform: '', maxTouchPoints: 0}, window: {screen, matchMedia: () => media, addEventListener: (name, handler) => {events[name] = handler;}}});
  return {events, elements, locks};
}
const apple = setup({ios: true});
assert.match(apple.elements['#install-app-hint'].textContent, /Share → Add to Home Screen/u);
assert.equal(apple.elements['#install-app-button'].hidden, true);
const installedApp = setup({installed: true});
assert.equal(installedApp.elements['#install-app'].hidden, true);
// An installed launch holds landscape; a browser tab is left alone.
assert.deepEqual(installedApp.locks, ['landscape']);
assert.deepEqual(apple.locks, []);
const android = setup();
let prompts = 0;
android.events.beforeinstallprompt({preventDefault() {}, async prompt() {prompts++;}, userChoice: Promise.resolve({outcome: 'dismissed'})});
assert.equal(android.elements['#install-app-button'].hidden, false);
await android.elements['#install-app-button'].click();
await android.elements['#install-app-button'].click();
assert.equal(prompts, 1);
assert.equal(android.elements['#install-app-button'].hidden, true);
android.events.appinstalled();
assert.equal(android.elements['#install-app'].hidden, true);
const manifest = JSON.parse(readFileSync(new URL('../packages/client/web/manifest.webmanifest', import.meta.url), 'utf8'));
assert.equal(manifest.display, 'standalone');
assert.equal(manifest.start_url, '/');
assert.equal(manifest.orientation, 'landscape');
for (const icon of manifest.icons) {
  const png = readFileSync(new URL(`../packages/client/web${icon.src}`, import.meta.url));
  const size = Number(icon.sizes.split('x')[0]);
  assert.equal(png.readUInt32BE(16), size);
  assert.equal(png.readUInt32BE(20), size);
}
console.log('PWA installation UI and manifest tests passed');
