#!/usr/bin/env node
// Isolated Chrome contexts exercise the actual WASM client against a seeded native match.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawn} from 'node:child_process';
import crypto from 'node:crypto';

const [browser, port, seedPath] = process.argv.slice(2);
const seed = Object.fromEntries(fs.readFileSync(seedPath, 'utf8').trim().split('\n').map(line => {
  const index = line.indexOf('=');
  return [line.slice(0, index), line.slice(index + 1)];
}));
const base = `http://127.0.0.1:${port}`;
const profile = fs.mkdtempSync(path.join(os.tmpdir(), 'pwmtf-two-browser-'));
const child = spawn(browser, [
  '--headless=new', '--remote-debugging-pipe', `--user-data-dir=${profile}`,
  '--disable-background-networking', '--disable-background-timer-throttling',
  '--disable-renderer-backgrounding', '--disable-backgrounding-occluded-windows',
  '--disable-component-update', '--disable-extensions', '--disable-gpu-sandbox',
  '--enable-webgl', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist',
  '--no-first-run', '--window-size=1280,720', 'about:blank',
], {detached: true, stdio: ['ignore', 'ignore', 'pipe', 'pipe', 'pipe']});
let nextId = 0;
let buffer = '';
const pending = new Map();
const frames = new Map();
let browserError;
child.on('error', error => { browserError = error; });
child.on('exit', (code, signal) => { browserError = new Error(`Chrome exited ${code}/${signal}`); });
child.stdio[4].on('data', chunk => {
  buffer += chunk.toString();
  let end;
  while ((end = buffer.indexOf('\0')) !== -1) {
    const message = JSON.parse(buffer.slice(0, end));
    buffer = buffer.slice(end + 1);
    if (message.id) {
      const item = pending.get(message.id);
      if (!item) continue;
      clearTimeout(item.timer);
      pending.delete(message.id);
      if (message.error) item.reject(new Error(`${item.method}: ${message.error.message}`));
      else item.resolve(message.result);
    } else if (message.sessionId) {
      if (message.method === 'Runtime.exceptionThrown') {
        browserError = new Error(message.params.exceptionDetails.exception?.description ?? message.params.exceptionDetails.text);
      }
      if (message.method === 'Network.webSocketFrameReceived' || message.method === 'Network.webSocketFrameSent') {
        const list = frames.get(message.sessionId) ?? [];
        list.push({method: message.method, opcode: message.params.response.opcode,
          payload: message.params.response.payloadData});
        frames.set(message.sessionId, list);
      }
    }
  }
});
function send(method, params = {}, sessionId) {
  if (browserError) return Promise.reject(browserError);
  return new Promise((resolve, reject) => {
    const id = ++nextId;
    const timer = setTimeout(() => { pending.delete(id); reject(new Error(`CDP timeout: ${method}`)); }, 20000);
    pending.set(id, {resolve, reject, timer, method});
    child.stdio[3].write(JSON.stringify({id, method, params, sessionId}) + '\0');
  });
}
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
async function until(label, action, timeout = 15000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    if (browserError) throw browserError;
    const result = await action();
    if (result) return result;
    await sleep(50);
  }
  throw new Error(`timed out waiting for ${label}`);
}
async function evaluate(session, expression) {
  const value = await send('Runtime.evaluate', {expression, returnByValue: true, awaitPromise: true}, session);
  if (value.exceptionDetails) throw new Error(value.exceptionDetails.exception?.description ?? value.exceptionDetails.text);
  return value.result.value;
}
async function page(cookie, matchId = seed.live_match_id) {
  const {browserContextId} = await send('Target.createBrowserContext');
  const {targetId} = await send('Target.createTarget', {url: 'about:blank', browserContextId});
  const {sessionId} = await send('Target.attachToTarget', {targetId, flatten: true});
  await send('Runtime.enable', {}, sessionId);
  await send('Network.enable', {}, sessionId);
  await send('Page.enable', {}, sessionId);
  const [, token] = cookie.split('=');
  if (!token) throw new Error('seed cookie missing');
  await send('Network.setCookie', {name: 'pwmtf_dev_session', value: token, url: base, httpOnly: true, sameSite: 'Lax'}, sessionId);
  await send('Page.navigate', {url: `${base}/?match=${matchId}`}, sessionId);
  await until('match ready', () => evaluate(sessionId, 'document.querySelector("#game-shell")?.dataset.clientState === "ready" && document.querySelector("#match-status")?.textContent.includes("Connected")'), 60000);
  if (await evaluate(sessionId, 'document.querySelector("#shot-control-mode")?.value') !== '1') {
    throw new Error('new match context did not use default aim + power bar');
  }
  return {sessionId, targetId, browserContextId};
}
async function practicePage() {
  const {browserContextId} = await send('Target.createBrowserContext');
  const {targetId} = await send('Target.createTarget', {url: 'about:blank', browserContextId});
  const {sessionId} = await send('Target.attachToTarget', {targetId, flatten: true});
  await send('Runtime.enable', {}, sessionId);
  await send('Page.enable', {}, sessionId);
  await send('Page.navigate', {url: `${base}/`}, sessionId);
  await until('practice ready', () => evaluate(sessionId,
    'document.querySelector("#game-shell")?.dataset.clientState === "ready"'), 60000);
  if (await evaluate(sessionId, 'document.querySelector("#shot-control-mode")?.value') !== '1') {
    throw new Error('new practice context did not use default aim + power bar');
  }
  return {sessionId, targetId, browserContextId};
}
async function strangerPage() {
  const {browserContextId} = await send('Target.createBrowserContext');
  const {targetId} = await send('Target.createTarget', {url: 'about:blank', browserContextId});
  const {sessionId} = await send('Target.attachToTarget', {targetId, flatten: true});
  await send('Runtime.enable', {}, sessionId);
  await send('Network.enable', {}, sessionId);
  await send('Page.enable', {}, sessionId);
  await send('Page.navigate', {url: `${base}/?match=${seed.live_match_id}`}, sessionId);
  return {sessionId, targetId};
}
// Session v2 frames are binary with a leading tag: the client sends
// Preview 0x07 / Command 0x06, the server relays Preview 0x84. A preview
// message is tag + u128 match id + the fixed 32-byte ShotPreview.
const PREVIEW_TAGS = {'Network.webSocketFrameSent': 0x07, 'Network.webSocketFrameReceived': 0x84};
function frameBytes(frame) {
  frame.bytes ??= Buffer.from(frame.payload, 'base64');
  return frame.bytes;
}
function previews(session, method) {
  return (frames.get(session) ?? []).filter(frame => {
    if (frame.method !== method || frame.opcode !== 2) return false;
    const bytes = frameBytes(frame);
    return bytes.length === 49 && bytes[0] === PREVIEW_TAGS[method] && bytes[17] === 0xff;
  });
}
function commandsSent(session) {
  return (frames.get(session) ?? []).filter(frame => frame.method === 'Network.webSocketFrameSent' &&
    frame.opcode === 2 && frameBytes(frame)[0] === 0x06);
}
function textFrames(session) {
  return (frames.get(session) ?? []).filter(frame => frame.opcode === 1);
}
function preview(frame) {
  const bytes = frameBytes(frame).subarray(17);
  if (bytes.length !== 32) throw new Error('unexpected preview length');
  return {aim: bytes.readUInt16BE(23), power: bytes.readUInt16BE(25), cancel: bytes[27], sequence: bytes.readUInt32BE(19)};
}
function u128Hex(value) {
  return BigInt(value).toString(16).padStart(32, '0');
}
function matchDataset(session) {
  return evaluate(session, 'JSON.stringify(document.querySelector("#match-status")?.dataset ?? {})').then(JSON.parse);
}
async function imageHash(session) {
  const {data} = await send('Page.captureScreenshot', {format: 'png', captureBeyondViewport: false}, session);
  return crypto.createHash('sha256').update(data).digest('hex');
}
async function mouse(session, type, x, y, buttons) {
  await send('Input.dispatchMouseEvent', {type, x, y,
    button: type === 'mouseMoved' ? 'none' : 'left', buttons,
    clickCount: type === 'mousePressed' ? 1 : 0}, session);
}
async function touch(session, type, points) {
  await send('Input.dispatchTouchEvent', {type, touchPoints: points.map(([id, x, y]) => ({id, x, y}))}, session);
}
// A second authenticated socket replays a captured ClientMessage::Preview
// after a full Hello/Welcome + SubscribeMatch/MatchState handshake.
async function replayPreview(session, payload) {
  const subscribe = `02${u128Hex(seed.live_match_id)}`;
  await evaluate(session, `new Promise((resolve, reject) => {
    const socket = new WebSocket('ws://127.0.0.1:${port}/ws');
    socket.binaryType = 'arraybuffer';
    const hex = value => Uint8Array.from(value.match(/../g), byte => parseInt(byte, 16));
    const timeout = setTimeout(() => { socket.close(); reject(new Error('preview replay handshake timed out')); }, 10000);
    socket.onopen = () => socket.send(Uint8Array.of(0x01, 0x01, 0x00, 0x02));
    socket.onerror = () => { clearTimeout(timeout); reject(new Error('preview replay socket failed')); };
    let stage = 'hello';
    socket.onmessage = event => {
      if (typeof event.data === 'string') { clearTimeout(timeout); reject(new Error('server sent a text frame')); return; }
      const tag = new Uint8Array(event.data)[0];
      if (stage === 'hello' && tag === 0x81) {
        stage = 'subscribe';
        socket.send(hex('${subscribe}'));
      } else if (stage === 'subscribe' && tag === 0x82) {
        stage = 'replayed';
        clearTimeout(timeout);
        socket.send(Uint8Array.from(atob('${payload}'), char => char.charCodeAt(0)));
        setTimeout(() => { socket.close(); resolve(true); }, 250);
      }
    };
  })`);
}
let one, two, stranger;
try {
  await send('Browser.getVersion');
  const practice = await practicePage();
  const focusShooterPage = await page(seed.player_one_cookie, seed.focus_match_id);
  const focusOpponentPage = await page(seed.player_two_cookie, seed.focus_match_id);
  const deadlineShooterPage = await page(seed.player_one_cookie, seed.deadline_match_id);
  const deadlineOpponentPage = await page(seed.player_two_cookie, seed.deadline_match_id);
  await send('Target.activateTarget', {targetId: practice.targetId});
  const practiceSession = practice.sessionId;
  const practiceCanvasRect = JSON.parse(await evaluate(practiceSession, 'JSON.stringify((({x,y,width,height}) => ({x,y,width,height}))(document.querySelector("#pwmtf-canvas").getBoundingClientRect()))'));
  const practiceScale = Math.min(practiceCanvasRect.width / 1220, practiceCanvasRect.height / 590);
  const practiceOrigin = {x: practiceCanvasRect.x + practiceCanvasRect.width / 2 - 295 * practiceScale,
    y: practiceCanvasRect.y + practiceCanvasRect.height / 2};
  await sleep(300);
  // The default is aim + power bar; the legacy gesture remains selectable.
  if (await evaluate(practiceSession, 'document.querySelector("#shot-control-mode").value') !== '1') {
    throw new Error('aim + power bar is not the default shot control');
  }
  await evaluate(practiceSession, `(() => {
    const control = document.querySelector('#shot-control-mode');
    control.value = '0';
    control.dispatchEvent(new Event('change', {bubbles: true}));
  })()`);
  await evaluate(practiceSession, `(() => {
    const control = document.querySelector('#shot-control-mode');
    control.value = '1';
    control.dispatchEvent(new Event('change', {bubbles: true}));
    const guide = document.querySelector('#aim-mode');
    guide.value = '2'; guide.dispatchEvent(new Event('change', {bubbles: true}));
    const range = document.querySelector('#unlimited-aim-range');
    range.checked = true; range.dispatchEvent(new Event('change', {bubbles: true}));
  })()`);
  const savedPreferences = await evaluate(practiceSession, `JSON.parse(localStorage.getItem('pwmtf.presentation.v1'))`);
  if (savedPreferences.version !== 1 || savedPreferences.shotControl !== 1 || savedPreferences.aimGuide !== 2 || savedPreferences.unlimitedRange !== true) {
    throw new Error('presentation preferences did not save valid selections');
  }
  await send('Page.reload', {ignoreCache: true}, practiceSession);
  await until('saved preferences restore before practice is ready', () => evaluate(practiceSession,
    `document.querySelector('#game-shell')?.dataset.clientState === 'ready' &&
    document.querySelector('#shot-control-mode')?.value === '1' &&
    document.querySelector('#aim-mode')?.value === '2' &&
    document.querySelector('#unlimited-aim-range')?.checked === true`), 60000);
  await evaluate(practiceSession, `(() => {
    const guide = document.querySelector('#aim-mode');
    guide.value = '1'; guide.dispatchEvent(new Event('change', {bubbles: true}));
    const range = document.querySelector('#unlimited-aim-range');
    range.checked = false; range.dispatchEvent(new Event('change', {bubbles: true}));
  })()`);
  // Exercise separate controls without firing or changing the practice rack.
  const practiceIdle = await imageHash(practiceSession);
  const alternateOrigin = {x: practiceOrigin.x - 110 * practiceScale, y: practiceOrigin.y};
  await mouse(practiceSession, 'mouseMoved', alternateOrigin.x, alternateOrigin.y, 0);
  await mouse(practiceSession, 'mousePressed', alternateOrigin.x, alternateOrigin.y, 1);
  await until('separate aim contact left of cue changes cue immediately', async () => (await imageHash(practiceSession)) !== practiceIdle);
  await mouse(practiceSession, 'mouseMoved', alternateOrigin.x + 25 * practiceScale, alternateOrigin.y, 1);
  await mouse(practiceSession, 'mouseReleased', alternateOrigin.x + 25 * practiceScale, alternateOrigin.y, 0);
  const bar = {x: practiceCanvasRect.x + practiceCanvasRect.width / 2 + 565 * practiceScale,
    y: practiceCanvasRect.y + practiceCanvasRect.height / 2};
  await mouse(practiceSession, 'mouseMoved', bar.x, bar.y, 0);
  await mouse(practiceSession, 'mousePressed', bar.x, bar.y, 1);
  await sleep(120);
  const beforeBarDrag = await imageHash(practiceSession);
  await mouse(practiceSession, 'mouseMoved', bar.x, bar.y + 100 * practiceScale, 1);
  await until('separate power-bar drag changes cue and guide', async () => (await imageHash(practiceSession)) !== beforeBarDrag);
  await mouse(practiceSession, 'mouseMoved', bar.x, bar.y, 1);
  await mouse(practiceSession, 'mouseReleased', bar.x, bar.y, 0);
  await touch(practiceSession, 'touchStart', [[19, bar.x, bar.y]]);
  await sleep(120);
  await touch(practiceSession, 'touchMove', [[19, bar.x, bar.y + 110 * practiceScale]]);
  await sleep(180);
  await touch(practiceSession, 'touchCancel', []);
  await sleep(200);
  const touchBarCanceled = await imageHash(practiceSession);
  await sleep(250);
  if (await imageHash(practiceSession) !== touchBarCanceled) throw new Error('separate touch cancel fired a practice shot');
  // Combined drag remains available; the mode change is stored on this device.
  await evaluate(practiceSession, `(() => {
    const control = document.querySelector('#shot-control-mode');
    control.value = '0';
    control.dispatchEvent(new Event('change', {bubbles: true}));
  })()`);
  await sleep(250);
  const combinedIdle = await imageHash(practiceSession);
  await sleep(250);
  if (await imageHash(practiceSession) !== combinedIdle) throw new Error('separate aim or uncharged power release fired');
  // A canceled touch blocks rearming until a fresh released contact.
  await touch(practiceSession, 'touchStart', [[20, bar.x, bar.y]]);
  await touch(practiceSession, 'touchEnd', []);
  await sleep(120);
  // The gesture begins well away from the cue ball, near the right rail.
  const practiceDragStart = {x: practiceOrigin.x + 560 * practiceScale, y: practiceOrigin.y - 100 * practiceScale};
  await mouse(practiceSession, 'mouseMoved', practiceDragStart.x, practiceDragStart.y, 0);
  await mouse(practiceSession, 'mousePressed', practiceDragStart.x, practiceDragStart.y, 1);
  await sleep(120);
  await mouse(practiceSession, 'mouseMoved', practiceDragStart.x - 90, practiceDragStart.y, 1);
  await until('practice remote-start drag changes local canvas', async () => (await imageHash(practiceSession)) !== practiceIdle);
  await mouse(practiceSession, 'mouseMoved', practiceDragStart.x, practiceDragStart.y, 1);
  await mouse(practiceSession, 'mouseReleased', practiceDragStart.x, practiceDragStart.y, 0);
  await sleep(350);
  const practiceRemoteStartCanceled = await imageHash(practiceSession);
  await sleep(250);
  if (await imageHash(practiceSession) !== practiceRemoteStartCanceled) throw new Error('uncharged remote-start drag fired');
  await touch(practiceSession, 'touchStart', [[8, practiceDragStart.x, practiceDragStart.y]]);
  await sleep(120);
  await touch(practiceSession, 'touchMove', [[8, practiceDragStart.x - 100, practiceDragStart.y]]);
  await until('practice anywhere-start touch charges', async () => (await imageHash(practiceSession)) !== practiceRemoteStartCanceled);
  await touch(practiceSession, 'touchMove', [[8, practiceDragStart.x, practiceDragStart.y]]);
  await sleep(120);
  await touch(practiceSession, 'touchEnd', []);
  await sleep(300);
  const remoteTouchCanceled = await imageHash(practiceSession);
  await sleep(250);
  if (await imageHash(practiceSession) !== remoteTouchCanceled) throw new Error('uncharged anywhere-start touch fired');
  await mouse(practiceSession, 'mouseMoved', practiceOrigin.x, practiceOrigin.y, 0);
  await mouse(practiceSession, 'mousePressed', practiceOrigin.x, practiceOrigin.y, 1);
  await sleep(120);
  await mouse(practiceSession, 'mouseMoved', practiceOrigin.x - 90, practiceOrigin.y, 1);
  await until('practice drag changes local canvas', async () => (await imageHash(practiceSession)) !== remoteTouchCanceled);
  const practiceCharged = await imageHash(practiceSession);
  await mouse(practiceSession, 'mouseMoved', practiceOrigin.x - 65, practiceOrigin.y + 35, 1);
  await until('practice drag updates direction and power', async () => (await imageHash(practiceSession)) !== practiceCharged);
  await mouse(practiceSession, 'mouseMoved', practiceOrigin.x, practiceOrigin.y, 1);
  await mouse(practiceSession, 'mouseReleased', practiceOrigin.x, practiceOrigin.y, 0);
  await sleep(200);
  const practiceCanceled = await imageHash(practiceSession);
  await sleep(250);
  if (await imageHash(practiceSession) !== practiceCanceled) throw new Error('practice dead-zone release started a shot');
  await touch(practiceSession, 'touchStart', [[1, practiceOrigin.x, practiceOrigin.y]]);
  await sleep(120);
  await touch(practiceSession, 'touchMove', [[1, practiceOrigin.x - 105, practiceOrigin.y + 20]]);
  await until('practice touch drag charges', async () => (await imageHash(practiceSession)) !== practiceCanceled);
  await touch(practiceSession, 'touchCancel', []);
  await sleep(350);
  const practiceTouchCanceled = await imageHash(practiceSession);
  await sleep(250);
  if (await imageHash(practiceSession) !== practiceTouchCanceled) throw new Error('canceled touch fired a practice shot');
  await touch(practiceSession, 'touchStart', [[3, practiceOrigin.x, practiceOrigin.y]]);
  await sleep(120);
  await touch(practiceSession, 'touchMove', [[3, practiceOrigin.x - 105, practiceOrigin.y + 20]]);
  await until('practice multitouch gesture charges', async () => (await imageHash(practiceSession)) !== practiceTouchCanceled);
  await touch(practiceSession, 'touchStart', [
    [3, practiceOrigin.x - 105, practiceOrigin.y + 20],
    [4, practiceOrigin.x + 105, practiceOrigin.y + 20],
  ]);
  await sleep(120);
  await touch(practiceSession, 'touchEnd', [[3, practiceOrigin.x - 105, practiceOrigin.y + 20]]);
  await sleep(120);
  await touch(practiceSession, 'touchEnd', []);
  await sleep(350);
  const practiceMultitouchCanceled = await imageHash(practiceSession);
  await sleep(250);
  if (await imageHash(practiceSession) !== practiceMultitouchCanceled) throw new Error('second finger allowed a practice shot');
  // A real tab switch must interrupt a charged mouse gesture. Do not count a
  // synthetic blur or CDP focus-emulation setting as a device interruption.
  const {targetId: otherTab} = await send('Target.createTarget', {url: 'about:blank', browserContextId: practice.browserContextId});
  await send('Target.activateTarget', {targetId: practice.targetId});
  await until('practice tab gains focus', () => evaluate(practiceSession, 'document.hasFocus()'));
  await mouse(practiceSession, 'mouseMoved', practiceOrigin.x, practiceOrigin.y, 0);
  await mouse(practiceSession, 'mousePressed', practiceOrigin.x, practiceOrigin.y, 1);
  await sleep(120);
  await mouse(practiceSession, 'mouseMoved', practiceOrigin.x - 100, practiceOrigin.y + 15, 1);
  await until('practice focus-loss drag charges', async () => (await imageHash(practiceSession)) !== practiceMultitouchCanceled);
  const chargedBeforeFocusLoss = await imageHash(practiceSession);
  await send('Target.activateTarget', {targetId: otherTab});
  await until('practice tab really loses focus', () => evaluate(practiceSession, '!document.hasFocus()'));
  await send('Target.activateTarget', {targetId: practice.targetId});
  await until('practice tab regains focus', () => evaluate(practiceSession, 'document.hasFocus()'));
  await until('focus loss clears charged cue', async () => (await imageHash(practiceSession)) !== chargedBeforeFocusLoss);
  await mouse(practiceSession, 'mouseReleased', practiceOrigin.x - 100, practiceOrigin.y + 15, 0);
  await sleep(350);
  const focusCanceled = await imageHash(practiceSession);
  await sleep(250);
  if (await imageHash(practiceSession) !== focusCanceled) throw new Error('tab switch fired an interrupted practice shot');
  await mouse(practiceSession, 'mouseMoved', practiceOrigin.x, practiceOrigin.y, 0);
  await mouse(practiceSession, 'mousePressed', practiceOrigin.x, practiceOrigin.y, 1);
  await sleep(120);
  await mouse(practiceSession, 'mouseMoved', practiceOrigin.x - 110, practiceOrigin.y + 25, 1);
  await until('practice second drag charges', async () => (await imageHash(practiceSession)) !== practiceCanceled);
  const practiceSecondCharged = await imageHash(practiceSession);
  await mouse(practiceSession, 'mouseReleased', practiceOrigin.x - 110, practiceOrigin.y + 25, 0);
  await until('practice charged release clears cue', async () => (await imageHash(practiceSession)) !== practiceSecondCharged);
  await sleep(200);
  const practiceMoving = await imageHash(practiceSession);
  await sleep(200);
  if (await imageHash(practiceSession) === practiceMoving) throw new Error('practice charged release did not move the rack');
  // Independent multiplayer match: do not perturb the later shot/reconciliation
  // scenario with Chrome tab-switch focus and pointer state.
  const focusShooter = focusShooterPage.sessionId;
  const focusOpponent = focusOpponentPage.sessionId;
  await send('Emulation.setFocusEmulationEnabled', {enabled: true}, focusShooter);
  await send('Emulation.setFocusEmulationEnabled', {enabled: true}, focusOpponent);
  await send('Target.activateTarget', {targetId: focusShooterPage.targetId});
  await until('focus shooter is player one', () => evaluate(focusShooter, 'document.querySelector("#match-status")?.textContent.includes("your turn")'), 20000);
  // Keep the established combined-gesture interruption regression; separate
  // mode is checked below on the same match after reselecting the new default.
  await evaluate(focusShooter, `(() => {
    const control = document.querySelector('#shot-control-mode');
    control.value = '0'; control.dispatchEvent(new Event('change', {bubbles: true}));
  })()`);
  const focusCanvas = JSON.parse(await evaluate(focusShooter, 'JSON.stringify((({x,y,width,height}) => ({x,y,width,height}))(document.querySelector("#pwmtf-canvas").getBoundingClientRect()))'));
  const focusScale = Math.min(focusCanvas.width / 1220, focusCanvas.height / 590);
  const focusOrigin = {x: focusCanvas.x + focusCanvas.width / 2 - 295 * focusScale, y: focusCanvas.y + focusCanvas.height / 2};
  const focusBefore = previews(focusOpponent, 'Network.webSocketFrameReceived').length;
  await mouse(focusShooter, 'mouseMoved', focusOrigin.x, focusOrigin.y, 0);
  await mouse(focusShooter, 'mousePressed', focusOrigin.x, focusOrigin.y, 1);
  await sleep(120);
  await mouse(focusShooter, 'mouseMoved', focusOrigin.x - 100, focusOrigin.y + 15, 1);
  await until('focus match opponent sees charged drag', () => previews(focusOpponent, 'Network.webSocketFrameReceived').slice(focusBefore).some(frame => !preview(frame).cancel && preview(frame).power > 1000));
  await send('Target.activateTarget', {targetId: focusOpponentPage.targetId});
  const focusChargedImage = await imageHash(focusOpponent);
  const {targetId: focusInterruptionTab} = await send('Target.createTarget', {url: 'about:blank', browserContextId: focusShooterPage.browserContextId});
  await send('Emulation.setFocusEmulationEnabled', {enabled: false}, focusShooter);
  await send('Target.activateTarget', {targetId: focusInterruptionTab});
  await until('match shooter actually loses focus', () => evaluate(focusShooter, '!document.hasFocus()'));
  await until('match focus interruption reaches opponent', () => previews(focusOpponent, 'Network.webSocketFrameReceived').slice(focusBefore).some(frame => preview(frame).cancel === 1), 1500);
  await until('opponent drops interrupted cue', async () => (await imageHash(focusOpponent)) !== focusChargedImage, 5000);
  await send('Target.activateTarget', {targetId: focusShooterPage.targetId});
  await until('match shooter regains focus', () => evaluate(focusShooter, 'document.hasFocus()'));
  await mouse(focusShooter, 'mouseReleased', focusOrigin.x, focusOrigin.y, 0);
  // Session v2 closes the socket while the tab is hidden; a contact that
  // starts before the session is ready again is deliberately ignored.
  await until('match shooter session reconnects', () => evaluate(focusShooter, 'document.querySelector("#match-status")?.textContent.startsWith("Connected") && document.querySelector("#match-status")?.textContent.includes("your turn")'));
  await sleep(200);
  const resumedBefore = previews(focusOpponent, 'Network.webSocketFrameReceived').length;
  await mouse(focusShooter, 'mouseMoved', focusOrigin.x, focusOrigin.y, 0);
  await mouse(focusShooter, 'mousePressed', focusOrigin.x, focusOrigin.y, 1);
  await sleep(120);
  await mouse(focusShooter, 'mouseMoved', focusOrigin.x - 95, focusOrigin.y + 30, 1);
  await until('same-match drag resumes after tab interruption', () => previews(focusOpponent, 'Network.webSocketFrameReceived').slice(resumedBefore).some(frame => !preview(frame).cancel && preview(frame).power > 1000));
  await mouse(focusShooter, 'mouseMoved', focusOrigin.x, focusOrigin.y, 1);
  await until('resumed match drag cancels at origin', () => previews(focusOpponent, 'Network.webSocketFrameReceived').slice(resumedBefore).some(frame => preview(frame).cancel === 1));
  await mouse(focusShooter, 'mouseReleased', focusOrigin.x, focusOrigin.y, 0);
  if (commandsSent(focusShooter).length) {
    throw new Error('match tab interruption submitted a shot');
  }
  // Same seeded match, separate control mode: aim-only must reach the opponent
  // at zero power, and the bar must change power without altering that aim.
  await evaluate(focusShooter, `(() => {
    const control = document.querySelector('#shot-control-mode');
    control.value = '1'; control.dispatchEvent(new Event('change', {bubbles: true}));
  })()`);
  const separateBefore = previews(focusOpponent, 'Network.webSocketFrameReceived').length;
  // Contact location relative to the cue ball must determine aim immediately,
  // regardless of the direction in which the pointer subsequently moves.
  const leftOfCue = {x: focusOrigin.x - 110, y: focusOrigin.y};
  const rightOfCue = {x: focusOrigin.x + 120, y: focusOrigin.y};
  await mouse(focusShooter, 'mouseMoved', leftOfCue.x, leftOfCue.y, 0);
  await mouse(focusShooter, 'mousePressed', leftOfCue.x, leftOfCue.y, 1);
  const leftAim = await until('opponent sees contact left of cue ball aim left immediately', () => previews(focusOpponent, 'Network.webSocketFrameReceived').slice(separateBefore).map(preview).find(value => !value.cancel && value.power === 0 && value.aim > 4000 && value.aim < 6000));
  await mouse(focusShooter, 'mouseMoved', leftOfCue.x + 25, leftOfCue.y, 1);
  await sleep(120);
  const afterLeft = previews(focusOpponent, 'Network.webSocketFrameReceived').slice(separateBefore).map(preview).filter(value => !value.cancel && value.sequence > leftAim.sequence);
  if (afterLeft.some(value => value.aim < 4000 || value.aim > 6000)) throw new Error('moving right while left of cue ball reversed aim');
  await mouse(focusShooter, 'mouseMoved', rightOfCue.x, rightOfCue.y, 1);
  const rightAim = await until('opponent sees pointer cross to right of cue ball', () => previews(focusOpponent, 'Network.webSocketFrameReceived').slice(separateBefore).map(preview).find(value => !value.cancel && value.power === 0 && value.sequence > leftAim.sequence && (value.aim < 800 || value.aim > 9200)));
  await mouse(focusShooter, 'mouseMoved', leftOfCue.x, leftOfCue.y, 1);
  const aimed = await until('opponent sees pointer return left of cue ball', () => previews(focusOpponent, 'Network.webSocketFrameReceived').slice(separateBefore).map(preview).find(value => !value.cancel && value.power === 0 && value.sequence > rightAim.sequence && value.aim > 4000 && value.aim < 6000));
  await mouse(focusShooter, 'mouseReleased', leftOfCue.x, leftOfCue.y, 0);
  await send('Target.activateTarget', {targetId: focusOpponentPage.targetId});
  const separateAimImage = await imageHash(focusOpponent);
  const separateBar = {x: focusCanvas.x + focusCanvas.width / 2 + 565 * focusScale,
    y: focusCanvas.y + focusCanvas.height / 2};
  await mouse(focusShooter, 'mouseMoved', separateBar.x, separateBar.y, 0);
  await mouse(focusShooter, 'mousePressed', separateBar.x, separateBar.y, 1);
  await sleep(120);
  await mouse(focusShooter, 'mouseMoved', separateBar.x, separateBar.y + 100 * focusScale, 1);
  await until('opponent sees separate bar power with fixed aim', () => previews(focusOpponent, 'Network.webSocketFrameReceived').slice(separateBefore).map(preview).some(value => !value.cancel && value.power > 1000 && value.aim === aimed.aim));
  await until('opponent visibly sees separate power planning', async () => (await imageHash(focusOpponent)) !== separateAimImage);
  await mouse(focusShooter, 'mouseMoved', separateBar.x, separateBar.y, 1);
  await until('opponent sees separate bar return cancel', () => previews(focusOpponent, 'Network.webSocketFrameReceived').slice(separateBefore).map(preview).some(value => value.cancel === 1));
  await mouse(focusShooter, 'mouseReleased', separateBar.x, separateBar.y, 0);
  if (commandsSent(focusShooter).length) {
    throw new Error('uncharged separate mode sent a shot');
  }
  // Charge once more: releasing the bar, not the aim drag, submits one shot.
  await mouse(focusShooter, 'mouseMoved', separateBar.x, separateBar.y, 0);
  await mouse(focusShooter, 'mousePressed', separateBar.x, separateBar.y, 1);
  await sleep(120);
  await mouse(focusShooter, 'mouseMoved', separateBar.x, separateBar.y + 110 * focusScale, 1);
  await until('separate mode charged before release', () => previews(focusOpponent, 'Network.webSocketFrameReceived').slice(separateBefore).map(preview).some(value => !value.cancel && value.power > 1000 && value.aim === aimed.aim));
  await mouse(focusShooter, 'mouseReleased', separateBar.x, separateBar.y + 110 * focusScale, 0);
  await until('separate power release reconciles authoritative shot', () => evaluate(focusOpponent,
    'document.querySelector("#match-status")?.textContent.includes("revision 1")'), 30000);
  const separateCommands = commandsSent(focusShooter);
  if (separateCommands.length !== 1) throw new Error(`separate bar release sent ${separateCommands.length} canonical shots`);
  one = await page(seed.player_one_cookie);
  two = await page(seed.player_two_cookie);
  stranger = await strangerPage();
  const outsider = stranger.sessionId;
  await until('signed-out outsider is asked to sign in instead of subscribing', () => evaluate(outsider,
    'document.querySelector("#match-status")?.textContent.includes("Sign in")'), 60000);
  await sleep(400);
  if (previews(outsider, 'Network.webSocketFrameReceived').length) {
    throw new Error('outsider received a preview before match subscription');
  }
  const shooter = one.sessionId;
  const opponent = two.sessionId;
  const deadlineShooter = deadlineShooterPage.sessionId;
  const deadlineOpponent = deadlineOpponentPage.sessionId;
  await send('Emulation.setFocusEmulationEnabled', {enabled: true}, deadlineShooter);
  await send('Emulation.setFocusEmulationEnabled', {enabled: true}, deadlineOpponent);
  await send('Target.activateTarget', {targetId: deadlineShooterPage.targetId});
  const deadlineCanvas = JSON.parse(await evaluate(deadlineShooter, 'JSON.stringify((({x,y,width,height}) => ({x,y,width,height}))(document.querySelector("#pwmtf-canvas").getBoundingClientRect()))'));
  const deadlineScale = Math.min(deadlineCanvas.width / 1220, deadlineCanvas.height / 590);
  const deadlineOrigin = {x: deadlineCanvas.x + deadlineCanvas.width / 2 - 295 * deadlineScale,
    y: deadlineCanvas.y + deadlineCanvas.height / 2};
  await evaluate(deadlineShooter, `(() => {
    const control = document.querySelector('#shot-control-mode');
    control.value = '0'; control.dispatchEvent(new Event('change', {bubbles: true}));
  })()`);
  await mouse(deadlineShooter, 'mouseMoved', deadlineOrigin.x, deadlineOrigin.y, 0);
  await mouse(deadlineShooter, 'mousePressed', deadlineOrigin.x, deadlineOrigin.y, 1);
  await sleep(120);
  await mouse(deadlineShooter, 'mouseMoved', deadlineOrigin.x - 100, deadlineOrigin.y + 15, 1);
  await until('deadline opponent sees charged drag', () => previews(deadlineOpponent, 'Network.webSocketFrameReceived').some(frame => {
    const value = preview(frame);
    return !value.cancel && value.power > 1000;
  }));
  await send('Target.activateTarget', {targetId: deadlineOpponentPage.targetId});
  const beforeDeadlineImage = await imageHash(deadlineOpponent);
  await send('Target.activateTarget', {targetId: deadlineShooterPage.targetId});
  await until('authoritative deadline advances both clients', async () =>
    (await evaluate(deadlineShooter, 'document.querySelector("#match-status")?.textContent.includes("revision 1")')) &&
    (await evaluate(deadlineOpponent, 'document.querySelector("#match-status")?.textContent.includes("revision 1")')), 145000);
  // The turn and its new deadline arrive only through the session MatchState.
  for (const [label, session, seat] of [['shooter', deadlineShooter, '1'], ['opponent', deadlineOpponent, '2']]) {
    const deadlineAccess = await matchDataset(session);
    if (deadlineAccess.revision !== '1' || deadlineAccess.activePlayer !== '2' || deadlineAccess.seat !== seat ||
        !/^\d+$/.test(deadlineAccess.deadlineAtMs ?? '')) {
      throw new Error(`authoritative deadline did not transfer the turn for the ${label}: ${JSON.stringify(deadlineAccess)}`);
    }
  }
  if (!(await evaluate(deadlineOpponent, 'document.querySelector("#match-status")?.textContent.includes("your turn")'))) {
    throw new Error('opponent was not shown the transferred turn');
  }
  await send('Target.activateTarget', {targetId: deadlineOpponentPage.targetId});
  await until('deadline clears opponent planning cue', async () => (await imageHash(deadlineOpponent)) !== beforeDeadlineImage);
  await send('Target.activateTarget', {targetId: deadlineShooterPage.targetId});
  const beforeLatePreview = previews(deadlineOpponent, 'Network.webSocketFrameReceived').length;
  await mouse(deadlineShooter, 'mouseMoved', deadlineOrigin.x - 130, deadlineOrigin.y + 35, 1);
  await mouse(deadlineShooter, 'mouseReleased', deadlineOrigin.x - 130, deadlineOrigin.y + 35, 0);
  await sleep(400);
  if (previews(deadlineOpponent, 'Network.webSocketFrameReceived').length !== beforeLatePreview ||
      commandsSent(deadlineShooter).length) {
    throw new Error('expired drag sent a preview or shot after authoritative deadline');
  }
  const afterDeadlineAccess = await matchDataset(deadlineOpponent);
  if (afterDeadlineAccess.revision !== '1' || afterDeadlineAccess.activePlayer !== '2') {
    throw new Error('late release changed canonical state after deadline');
  }
  await send('Target.activateTarget', {targetId: one.targetId});
  await evaluate(shooter, `(() => {
    const control = document.querySelector('#shot-control-mode');
    control.value = '0'; control.dispatchEvent(new Event('change', {bubbles: true}));
  })()`);
  await send('Emulation.setFocusEmulationEnabled', {enabled: true}, shooter);
  await send('Emulation.setFocusEmulationEnabled', {enabled: true}, opponent);
  await send('Target.activateTarget', {targetId: two.targetId});
  const untouched = await imageHash(opponent);
  await send('Target.activateTarget', {targetId: one.targetId});
  const canvas = JSON.parse(await evaluate(shooter, 'JSON.stringify((({x,y,width,height}) => ({x,y,width,height}))(document.querySelector("#pwmtf-canvas").getBoundingClientRect()))'));
  const scale = Math.min(canvas.width / 1220, canvas.height / 590);
  const origin = {x: canvas.x + canvas.width / 2 - 295 * scale, y: canvas.y + canvas.height / 2};
  const matchDragStart = {x: origin.x + 540 * scale, y: origin.y - 95 * scale};
  await mouse(shooter, 'mouseMoved', matchDragStart.x, matchDragStart.y, 0);
  await mouse(shooter, 'mousePressed', matchDragStart.x, matchDragStart.y, 1);
  await sleep(100);
  await mouse(shooter, 'mouseMoved', matchDragStart.x - 90, matchDragStart.y, 1);
  await until('opponent sees preview from anywhere on table', () => previews(opponent, 'Network.webSocketFrameReceived').length > 0);
  await mouse(shooter, 'mouseMoved', matchDragStart.x, matchDragStart.y, 1);
  await mouse(shooter, 'mouseReleased', matchDragStart.x, matchDragStart.y, 0);
  await until('opponent sees anywhere-start cancel', () => previews(opponent, 'Network.webSocketFrameReceived').some(frame => preview(frame).cancel));
  if (await evaluate(shooter, 'document.querySelector("#match-status")?.textContent.includes("revision 1")')) throw new Error('anywhere-start dead-zone release fired a match shot');
  const previewStart = previews(opponent, 'Network.webSocketFrameReceived').length;
  await mouse(shooter, 'mouseMoved', origin.x, origin.y, 0);
  await mouse(shooter, 'mousePressed', origin.x, origin.y, 1);
  await sleep(100);
  await mouse(shooter, 'mouseMoved', origin.x - 90, origin.y, 1);
  await until('first charged preview', () => previews(opponent, 'Network.webSocketFrameReceived').slice(previewStart).some(frame => {
    const value = preview(frame);
    return !value.cancel && value.power > 1000;
  }));
  const first = preview(previews(opponent, 'Network.webSocketFrameReceived').slice(previewStart).find(frame => {
    const value = preview(frame);
    return !value.cancel && value.power > 1000;
  }));
  if (first.cancel || first.power < 1000) throw new Error('first drag did not charge');
  await sleep(120);
  if (previews(outsider, 'Network.webSocketFrameReceived').length) {
    throw new Error('unauthenticated browser received private planning traffic');
  }
  await send('Target.activateTarget', {targetId: two.targetId});
  await until('opponent renders charged drag', async () => (await imageHash(opponent)) !== untouched);
  const chargedImage = await imageHash(opponent);
  // Freeze the sender while keeping its socket open: the remote preview must expire
  // without relying on a cancel packet or a canonical turn change.
  await send('Debugger.enable', {}, shooter);
  await send('Debugger.pause', {}, shooter);
  await sleep(950);
  await until('idle remote preview expires', async () => (await imageHash(opponent)) !== chargedImage);
  await send('Debugger.resume', {}, shooter);
  await send('Debugger.disable', {}, shooter);
  await send('Target.activateTarget', {targetId: one.targetId});
  await send('Network.emulateNetworkConditions', {offline: false, latency: 180, downloadThroughput: -1, uploadThroughput: -1}, opponent);
  const delayedStart = previews(opponent, 'Network.webSocketFrameReceived').length;
  await mouse(shooter, 'mouseMoved', origin.x - 120, origin.y + 20, 1);
  await until('changed preview under network delay', () => previews(opponent, 'Network.webSocketFrameReceived').slice(delayedStart).some(frame => {
    const value = preview(frame);
    return !value.cancel && value.aim !== first.aim && value.power !== first.power && value.sequence > first.sequence;
  }));
  const second = preview(previews(opponent, 'Network.webSocketFrameReceived').slice(delayedStart).find(frame => {
    const value = preview(frame);
    return !value.cancel && value.aim !== first.aim && value.power !== first.power && value.sequence > first.sequence;
  }));
  await send('Network.emulateNetworkConditions', {offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1}, opponent);
  if (second.aim === first.aim || second.power === first.power || second.sequence <= first.sequence) {
    throw new Error('direction and power did not update together');
  }
  await mouse(shooter, 'mouseMoved', origin.x - 65, origin.y + 35, 1);
  await until('shortened preview', () => previews(opponent, 'Network.webSocketFrameReceived').some(frame => {
    const value = preview(frame);
    return !value.cancel && value.aim !== second.aim && value.power < second.power && value.sequence > second.sequence;
  }));
  const shortened = preview(previews(opponent, 'Network.webSocketFrameReceived').find(frame => {
    const value = preview(frame);
    return !value.cancel && value.aim !== second.aim && value.power < second.power && value.sequence > second.sequence;
  }));
  if (shortened.aim === second.aim || shortened.power >= second.power) {
    throw new Error('shortening drag did not adjust aim and power');
  }
  const beforeReversal = previews(opponent, 'Network.webSocketFrameReceived').length;
  await mouse(shooter, 'mouseMoved', origin.x + 75, origin.y - 30, 1);
  await until('opponent sees reversal without a new press', () => previews(opponent, 'Network.webSocketFrameReceived').slice(beforeReversal).some(frame => {
    const value = preview(frame);
    return !value.cancel && value.sequence > shortened.sequence && value.aim !== shortened.aim && value.power > 1000;
  }));
  const reversed = preview(previews(opponent, 'Network.webSocketFrameReceived').slice(beforeReversal).find(frame => {
    const value = preview(frame);
    return !value.cancel && value.sequence > shortened.sequence && value.aim !== shortened.aim && value.power > 1000;
  }));
  const beforeReturn = previews(opponent, 'Network.webSocketFrameReceived').length;
  await mouse(shooter, 'mouseMoved', origin.x - 65, origin.y + 35, 1);
  await until('opponent sees direction return and power readjust', () => previews(opponent, 'Network.webSocketFrameReceived').slice(beforeReturn).some(frame => {
    const value = preview(frame);
    return !value.cancel && value.sequence > reversed.sequence && value.aim !== reversed.aim && value.power !== reversed.power;
  }));
  // Once a newer preview has reached the observer, a delayed old frame from
  // another authenticated socket must not replace the visible planning state.
  await send('Target.activateTarget', {targetId: two.targetId});
  const shortenedImage = await imageHash(opponent);
  const receivedBeforeStale = previews(opponent, 'Network.webSocketFrameReceived').length;
  const staleWithinTurn = previews(shooter, 'Network.webSocketFrameSent').find(frame => {
    const value = preview(frame);
    return !value.cancel && value.sequence === first.sequence;
  });
  if (!staleWithinTurn) throw new Error('no old preview to replay inside the turn');
  await replayPreview(shooter, staleWithinTurn.payload);
  const lateFrames = previews(opponent, 'Network.webSocketFrameReceived').slice(receivedBeforeStale);
  const imageAfterStale = await imageHash(opponent);
  if (lateFrames.some(frame => frame.payload === staleWithinTurn.payload) || imageAfterStale !== shortenedImage) {
    throw new Error('delayed same-revision preview replaced newer planning');
  }
  const beforeReconnect = await imageHash(opponent);
  // Reload the observer during a live gesture; the old socket must not replay
  // ephemeral state, and the new subscription must recover from a snapshot.
  await send('Debugger.enable', {}, shooter);
  await send('Debugger.pause', {}, shooter);
  const receivedBeforeReload = previews(opponent, 'Network.webSocketFrameReceived').length;
  await send('Network.emulateNetworkConditions', {offline: true, latency: 0, downloadThroughput: 0, uploadThroughput: 0}, opponent);
  await until('observer socket closes offline', () => evaluate(opponent,
    '!document.querySelector("#match-status")?.textContent.includes("Connected")'), 15000);
  await send('Target.activateTarget', {targetId: two.targetId});
  await until('offline observer clears charged cue', async () => (await imageHash(opponent)) !== beforeReconnect);
  // CDP may deliver frames already in flight after the socket's UI changes to
  // disconnected. Establish the baseline only after that transition settles.
  await sleep(150);
  const receivedWhileOffline = previews(opponent, 'Network.webSocketFrameReceived').length;
  if (receivedWhileOffline > receivedBeforeReload && previews(opponent, 'Network.webSocketFrameReceived').slice(receivedBeforeReload).some(frame => preview(frame).cancel)) {
    throw new Error('observer received a cancel while disconnecting');
  }
  // Cancel while the observer is offline: the cancel frame cannot reach it.
  await send('Debugger.resume', {}, shooter);
  await send('Target.activateTarget', {targetId: one.targetId});
  await mouse(shooter, 'mouseMoved', origin.x, origin.y, 1);
  await until('shooter sends offline cancel', () => previews(shooter, 'Network.webSocketFrameSent').some(frame => {
    const value = preview(frame);
    return value.cancel === 1 && value.sequence > shortened.sequence;
  }));
  await sleep(100);
  if (previews(opponent, 'Network.webSocketFrameReceived').length !== receivedWhileOffline) {
    throw new Error(`offline observer received a frame after cancel: ${JSON.stringify(previews(opponent, 'Network.webSocketFrameReceived').slice(receivedWhileOffline).map(preview))}`);
  }
  await send('Debugger.pause', {}, shooter);
  await send('Network.emulateNetworkConditions', {offline: false, latency: 0, downloadThroughput: -1, uploadThroughput: -1}, opponent);
  await send('Page.reload', {ignoreCache: true}, opponent);
  await until('opponent reconnects to canonical match', () => evaluate(opponent,
    'document.querySelector("#game-shell")?.dataset.clientState === "ready" && document.querySelector("#match-status")?.textContent.includes("Connected")'), 60000);
  await sleep(800);
  if (previews(opponent, 'Network.webSocketFrameReceived').length !== receivedWhileOffline) {
    throw new Error('reconnected observer received a replay of stale preview');
  }
  const reconnected = await imageHash(opponent);
  if (reconnected === beforeReconnect) throw new Error('reconnected opponent retained old gesture');
  await send('Debugger.resume', {}, shooter);
  await send('Debugger.disable', {}, shooter);
  await send('Target.activateTarget', {targetId: one.targetId});
  await mouse(shooter, 'mouseMoved', origin.x - 115, origin.y + 22, 1);
  await until('reconnected opponent receives new preview', () => previews(opponent, 'Network.webSocketFrameReceived').some(frame => {
    const value = preview(frame);
    return !value.cancel && value.sequence > shortened.sequence;
  }));
  await send('Target.activateTarget', {targetId: two.targetId});
  const beforeCancel = await imageHash(opponent);
  await send('Target.activateTarget', {targetId: one.targetId});
  await mouse(shooter, 'mouseMoved', origin.x, origin.y, 1);
  await until('cancel preview', () => previews(opponent, 'Network.webSocketFrameReceived').some(frame => preview(frame).cancel === 1));
  await send('Target.activateTarget', {targetId: two.targetId});
  await until('opponent preview clears', async () => (await imageHash(opponent)) !== beforeCancel);
  await send('Target.activateTarget', {targetId: one.targetId});
  await mouse(shooter, 'mouseMoved', origin.x - 115, origin.y + 22, 1);
  await until('resumed preview', () => previews(opponent, 'Network.webSocketFrameReceived').some(frame => {
    const value = preview(frame);
    return value.sequence > shortened.sequence && value.cancel === 0;
  }));
  const resumed = previews(opponent, 'Network.webSocketFrameReceived').find(frame => {
    const value = preview(frame);
    return value.sequence > shortened.sequence && value.cancel === 0;
  });
  if (!resumed || preview(resumed).power < 1000) throw new Error('resumed drag did not charge');
  // Sample the observer's rendered balls through the shot. Accepted shots are
  // replayed tick-by-tick from the authoritative table, so mid-shot positions
  // must differ from both the starting rack and the settled table.
  await evaluate(opponent, `(async () => {
    const module = await import('/pwmtf_client.js');
    if (typeof module.match_ball_position !== 'function') throw new Error('match_ball_position debug export is missing');
    const status = document.querySelector('#match-status');
    window.__pwmtfSamples = [];
    window.__pwmtfSampler = setInterval(() => {
      const balls = [];
      for (let ball = 0; ball < 16; ball += 1) {
        const position = module.match_ball_position(ball);
        balls.push(position === undefined || position === null ? null : Array.from(position));
      }
      window.__pwmtfSamples.push({revision: status.dataset.revision, playing: status.dataset.playing === 'true', balls});
      if (window.__pwmtfSamples.length >= 3000) clearInterval(window.__pwmtfSampler);
    }, 20);
    return true;
  })()`);
  await mouse(shooter, 'mouseReleased', origin.x - 120, origin.y + 20, 0);
  await send('Target.activateTarget', {targetId: two.targetId});
  await until('authoritative revision 1', () => evaluate(opponent, 'document.querySelector("#match-status")?.textContent.includes("revision 1")'), 30000);
  await until('observer shot playback settles', async () => {
    const data = await matchDataset(opponent);
    return data.revision === '1' && data.playing === 'false';
  }, 30000);
  await sleep(200);
  const samples = JSON.parse(await evaluate(opponent, 'clearInterval(window.__pwmtfSampler), JSON.stringify(window.__pwmtfSamples)'));
  const before = samples.filter(sample => sample.revision === '0').at(-1);
  const playing = samples.filter(sample => sample.revision === '1' && sample.playing);
  const settled = samples.at(-1);
  if (!before || !settled || settled.playing || playing.length < 3) {
    throw new Error(`observer did not play the accepted shot back (${playing.length} playing samples of ${samples.length})`);
  }
  const moved = (left, right) => left !== null && right !== null && Math.hypot(left[0] - right[0], left[1] - right[1]) > 1e-3;
  const midShot = playing.filter(sample => sample.balls.some((position, ball) =>
    moved(position, before.balls[ball]) && moved(position, settled.balls[ball])));
  const distinctFrames = new Set(midShot.map(sample => JSON.stringify(sample.balls))).size;
  if (midShot.length === 0 || distinctFrames < 3) {
    throw new Error(`observer did not render ball movement between release and settle (${midShot.length} mid-shot samples, ${distinctFrames} distinct)`);
  }
  await send('Target.activateTarget', {targetId: one.targetId});
  await until('shooter reconciled', () => evaluate(shooter, 'document.querySelector("#match-status")?.textContent.includes("revision 1")'), 30000);
  const receivedAfterShot = previews(opponent, 'Network.webSocketFrameReceived').length;
  // A late preview from the former turn must not cross the authoritative revision.
  const staleFrame = previews(shooter, 'Network.webSocketFrameSent').find(frame => !preview(frame).cancel);
  if (!staleFrame) throw new Error('no original preview to replay after turn transition');
  await replayPreview(shooter, staleFrame.payload);
  await sleep(120);
  if (previews(opponent, 'Network.webSocketFrameReceived').length !== receivedAfterShot) {
    throw new Error('stale preview from prior revision reached the opponent');
  }
  await send('Target.activateTarget', {targetId: two.targetId});
  await sleep(800);
  if (!(await evaluate(opponent, 'document.querySelector("#match-status")?.textContent.includes("revision 1")'))) {
    throw new Error('opponent lost canonical state after preview expiration');
  }
  if (commandsSent(shooter).length !== 1) {
    throw new Error('release did not submit exactly one canonical command');
  }
  for (const session of [shooter, opponent, focusShooter, focusOpponent, deadlineShooter, deadlineOpponent]) {
    if (textFrames(session).length) throw new Error('a session WebSocket carried a text frame');
  }
  console.log('practice combined and separate controls (mouse aim/bar, touch cancellation), combined mouse/touch/multitouch and real tab-switch cancel, drag/dead-zone/local shot; two-browser separate aim/power preview and one authoritative bar-release shot, combined live aim/power, match tab-switch interruption, reversal, deadline interruption, delay, stale rejection, lost cancel, idle expiry, offline reconnect, observer mid-shot playback and outsider isolation passed');
} catch (error) {
  console.error(`two-browser shot failed: ${error.stack ?? error}`);
  process.exitCode = 1;
} finally {
  try { await send('Browser.close'); } catch {}
  for (let attempt = 0; child.exitCode === null && attempt < 20; attempt += 1) await sleep(100);
  if (child.exitCode === null) { try { process.kill(-child.pid, 'SIGTERM'); } catch {} }
  try { fs.rmSync(profile, {recursive: true, force: true, maxRetries: 5, retryDelay: 100}); } catch {}
}
