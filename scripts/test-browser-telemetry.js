#!/usr/bin/env node
"use strict";

const fs = require("node:fs");
const vm = require("node:vm");

// Feasibility capture tooling lives in feasibility.js; match chrome in bootstrap.js.
const path = process.argv[2] ?? "packages/client/web/feasibility.js";
const source = fs.readFileSync(path, "utf8");
const bootstrapSource = fs.readFileSync(process.argv[3] ?? "packages/client/web/bootstrap.js", "utf8");
const startMarker = "function hasRequiredBackgroundIntervals(durations) {";
const endMarker = "function externalObservations() {";
const start = source.indexOf(startMarker);
const end = source.indexOf(endMarker, start);
if (start < 0 || end < 0) {
  throw new Error("cannot locate frame sampler in feasibility.js");
}

const rafCallbacks = [];
const context = {
  captureActive: false,
  lastFrameAt: null,
  frameWindowStartedAt: null,
  frameWindowCount: 0,
  telemetry: {
    captureStartedAt: null,
    captureStoppedAt: null,
    hiddenStartedAt: null,
    hiddenDurationMs: 0,
    frameCount: 0,
    frameGapSamplesMs: [],
    maximumFrameGapMs: 0,
    currentFps: null,
    fpsSamples: [],
    minimumFps: null,
    maximumFps: null,
  },
  feasibilityEnabled: false,
  performance: { now: () => 0, memory: null },
  refreshMetrics() {},
  window: {
    requestAnimationFrame(callback) {
      rafCallbacks.push(callback);
    },
  },
};
vm.createContext(context);
vm.runInContext(
  `${source.slice(start, end)}\nthis.frame = frame; this.resetFrameWindow = resetFrameWindow; this.captureDuration = captureDuration; this.hasRequiredBackgroundIntervals = hasRequiredBackgroundIntervals; this.captureTargets = captureTargets;`,
  context,
);

context.captureActive = true;
context.frame(0);
context.frame(16);
context.frame(1_016);
if (context.telemetry.maximumFrameGapMs !== 1_000) {
  throw new Error(`expected active frame gap to be sampled, got ${context.telemetry.maximumFrameGapMs}`);
}

context.resetFrameWindow();
context.frame(61_016);
context.frame(61_032);
if (context.telemetry.maximumFrameGapMs !== 1_000) {
  throw new Error("hidden interval leaked into maximum frame gap");
}
if (context.telemetry.frameGapSamplesMs.some((gap) => gap > 1_000)) {
  throw new Error("hidden interval leaked into frame-gap samples");
}
if (context.telemetry.minimumFps !== null && context.telemetry.minimumFps < 1) {
  throw new Error("hidden interval produced a false near-zero FPS sample");
}
if (rafCallbacks.length !== 5) {
  throw new Error("frame sampler stopped scheduling animation frames");
}

context.telemetry.captureStartedAt = 1_000;
context.telemetry.captureStoppedAt = 71_000;
context.telemetry.hiddenDurationMs = 10_000;
if (context.captureDuration() !== 60_000) {
  throw new Error("completed capture duration included hidden time");
}
context.telemetry.captureStoppedAt = null;
context.telemetry.hiddenStartedAt = 61_000;
context.performance.now = () => 71_000;
if (context.captureDuration() !== 50_000) {
  throw new Error("active capture duration included the current hidden interval");
}
if (!context.hasRequiredBackgroundIntervals([30_000, 45_000])) {
  throw new Error("two valid background intervals were rejected");
}
if (context.hasRequiredBackgroundIntervals([30_000, 29_999])) {
  throw new Error("a short background interval was accepted");
}
const interactionTargets = context.captureTargets("warm");
if (
  interactionTargets.visibilityChanges !== 0 ||
  interactionTargets.orientationChanges !== 0 ||
  interactionTargets.pageHides !== 0 ||
  interactionTargets.pageShows !== 0 ||
  interactionTargets.backgroundIntervals !== 0 ||
  interactionTargets.backgroundSuspensions !== 0 ||
  interactionTargets.explicitResumes !== 0
) {
  throw new Error("interaction metrics expose lifecycle evidence targets");
}
const lifecycleTargets = context.captureTargets("lifecycle");
if (
  lifecycleTargets.visibilityChanges !== 4 ||
  lifecycleTargets.orientationChanges !== 2 ||
  lifecycleTargets.pageHides !== 2 ||
  lifecycleTargets.pageShows !== 2 ||
  lifecycleTargets.backgroundIntervals !== 2 ||
  lifecycleTargets.backgroundSuspensions !== 2 ||
  lifecycleTargets.explicitResumes !== 2
) {
  throw new Error("lifecycle metrics do not expose exact required evidence targets");
}

const observationsStart = source.indexOf("function externalObservations() {");
const observationsEnd = source.indexOf("function physicalCheckResults() {", observationsStart);
if (observationsStart < 0 || observationsEnd < 0) {
  throw new Error("cannot locate external observation validation in feasibility.js");
}
function observationInput(value) {
  return {
    value,
    labels: [{ textContent: "fixture" }],
    reportValidity: () => true,
    focus() {},
  };
}
const observationsContext = {
  firstVisibleInput: observationInput("4000"),
  firstInputInput: observationInput("4200"),
  steadyMemoryInput: observationInput("120"),
  peakMemoryInput: observationInput("180"),
  thermalResultInput: observationInput("no-warning"),
  reloadObservedInput: observationInput("no"),
  platformInput: observationInput("iphone"),
  cacheStateInput: observationInput("cold"),
  telemetry: {
    captureStartedAt: 0,
    captureStoppedAt: 65_000,
    hiddenStartedAt: null,
    hiddenDurationMs: 0,
    clientReadyMs: 1000,
    firstCanvasContactMs: 1200,
  },
  captureActive: false,
  captureButton: { focus() {} },
  performance: { now: () => 65_000 },
  showStatus(message) {
    observationsContext.status = message;
  },
  Number,
  Object,
};
vm.createContext(observationsContext);
vm.runInContext(
  `${source.slice(start, end)}\n${source.slice(observationsStart, observationsEnd)}\nthis.requireExternalObservations = requireExternalObservations;`,
  observationsContext,
);
observationsContext.firstVisibleInput.value = "not-a-number";
if (observationsContext.requireExternalObservations()) {
  throw new Error("external observations accepted a non-finite numeric value");
}
if (!observationsContext.status.includes("valid finite values")) {
  throw new Error("invalid external observation lacked actionable guidance");
}
observationsContext.firstVisibleInput.value = "4000ms";
if (observationsContext.requireExternalObservations()) {
  throw new Error("external observations accepted a numeric prefix with trailing text");
}
observationsContext.firstVisibleInput.value = "4000";
observationsContext.cacheStateInput.value = "warm";
if (observationsContext.requireExternalObservations()) {
  throw new Error("external observations accepted a mobile warm-start budget overrun");
}
if (!observationsContext.status.includes("3-second warm budget")) {
  throw new Error("warm-start budget overrun lacked actionable guidance");
}
observationsContext.cacheStateInput.value = "cold";
observationsContext.telemetry.clientReadyMs = null;
if (observationsContext.requireExternalObservations()) {
  throw new Error("external observations accepted missing client timing evidence");
}
observationsContext.telemetry.clientReadyMs = 1_000;
observationsContext.firstVisibleInput.value = "66000";
if (observationsContext.requireExternalObservations()) {
  throw new Error("navigation-relative visible-table timing after capture stop was accepted");
}
if (!observationsContext.status.includes("before capture stop")) {
  throw new Error("visible-table timing after capture stop lacked actionable guidance");
}
observationsContext.firstVisibleInput.value = "4000";
observationsContext.firstInputInput.value = "66000";
if (observationsContext.requireExternalObservations()) {
  throw new Error("navigation-relative accepted-input timing after capture stop was accepted");
}
if (!observationsContext.status.includes("before capture stop")) {
  throw new Error("accepted-input timing after capture stop lacked actionable guidance");
}
observationsContext.firstInputInput.value = "4200";
observationsContext.thermalResultInput.value = "warning";
if (observationsContext.requireExternalObservations()) {
  throw new Error("external observations accepted a thermal warning");
}
if (!observationsContext.status.includes("invalidates this run")) {
  throw new Error("thermal warning lacked invalid-run guidance");
}
observationsContext.thermalResultInput.value = "no-warning";
observationsContext.reloadObservedInput.value = "yes";
if (observationsContext.requireExternalObservations()) {
  throw new Error("external observations accepted reload or eviction evidence");
}
if (!observationsContext.status.includes("reload or eviction invalidates")) {
  throw new Error("reload or eviction lacked invalid-run guidance");
}
observationsContext.reloadObservedInput.value = "no";
observationsContext.captureActive = true;
if (observationsContext.requireExternalObservations()) {
  throw new Error("external observations were validated during an active capture");
}
if (!observationsContext.status.includes("Stop the capture")) {
  throw new Error("active-capture observation validation lacked stop guidance");
}
observationsContext.captureActive = false;

const metadataStart = source.indexOf("function detectBrowserVersion(userAgent, family) {");
const metadataEnd = source.indexOf("const captureLockedInputs = [", metadataStart);
if (metadataStart < 0 || metadataEnd < 0) {
  throw new Error("cannot locate capture metadata validation in feasibility.js");
}

function input(value) {
  return {
    value,
    labels: [{ textContent: "fixture" }],
    reportValidity: () => true,
    focus() {},
  };
}

const metadataContext = {
  platformInput: input("desktop"),
  hardwareModelInput: input("fixture-hardware"),
  osVersionInput: input("fixture-os"),
  browserFamilyInput: input("chrome"),
  browserVersionInput: input("151.0.7922.138"),
  cacheStateInput: input("cold"),
  minimumVersionInput: input("no"),
  presentationTierInput: input("default"),
  runNumberInput: input("1"),
  detectedBrowserFamily: "chrome",
  detectedPlatform: "desktop",
  candidateWasmOptimization: "wasm-opt-Oz",
  candidateSourceHash: "a".repeat(64),
  candidateBuildId: `fixture-${"a".repeat(64)}`,
  candidateBundleHash: "b".repeat(64),
  candidateBundleHashAlgorithm: "sha256-length-prefixed-v1",
  candidateBevyVersion: "0.19.1",
  candidateRenderer: "WebGL2",
  allowedBrowserFamilies(platform) {
    return {
      iphone: ["safari"],
      ipad: ["safari"],
      "android-phone": ["chrome", "firefox", "samsung-internet"],
      "android-tablet": ["chrome"],
      desktop: ["safari", "chrome", "firefox", "edge"],
    }[platform] ?? [];
  },
  navigator: {
    userAgent:
      "Mozilla/5.0 AppleWebKit/537.36 (KHTML, like Gecko) Chrome/151.0.7922.138 Safari/537.36",
  },
  statusOutput: { textContent: "" },
  showStatus(message) {
    metadataContext.statusOutput.textContent = message;
  },
};
vm.createContext(metadataContext);
vm.runInContext(
  `${source.slice(metadataStart, metadataEnd)}\nthis.requireTestMetadata = requireTestMetadata; this.testMetadata = testMetadata;`,
  metadataContext,
);

if (!metadataContext.requireTestMetadata()) {
  throw new Error(
    `matching declared/detected browser was rejected: ${metadataContext.statusOutput.textContent}`,
  );
}
metadataContext.platformInput.value = "iphone";
if (metadataContext.requireTestMetadata()) {
  throw new Error("mismatched declared/detected platform was accepted");
}
if (!metadataContext.statusOutput.textContent.includes("platform")) {
  throw new Error("platform mismatch did not produce an actionable error");
}
metadataContext.platformInput.value = "desktop";
metadataContext.detectedPlatform = "iphone";
metadataContext.detectedBrowserFamily = "chrome";
metadataContext.browserFamilyInput.value = "chrome";
metadataContext.platformInput.value = "iphone";
if (metadataContext.requireTestMetadata()) {
  throw new Error("capture metadata accepted an unsupported platform/browser pair");
}
if (!metadataContext.statusOutput.textContent.includes("not supported")) {
  throw new Error("unsupported platform/browser pair lacked actionable guidance");
}
metadataContext.platformInput.value = "desktop";
metadataContext.detectedPlatform = "desktop";
metadataContext.browserFamilyInput.value = "firefox";
if (metadataContext.requireTestMetadata()) {
  throw new Error("mismatched declared/detected browser was accepted");
}
if (!metadataContext.statusOutput.textContent.includes("does not match")) {
  throw new Error("browser mismatch did not produce an actionable error");
}
metadataContext.browserFamilyInput.value = "chrome";
metadataContext.browserVersionInput.value = "150.0.0.0";
if (metadataContext.requireTestMetadata()) {
  throw new Error("mismatched declared/detected browser version was accepted");
}
if (!metadataContext.statusOutput.textContent.includes("version")) {
  throw new Error("browser-version mismatch did not produce an actionable error");
}
metadataContext.browserVersionInput.value = "0151.0.7922.138";
if (metadataContext.requireTestMetadata()) {
  throw new Error("non-canonical declared browser version was accepted");
}
metadataContext.browserVersionInput.value = "151.0.7922";
if (metadataContext.requireTestMetadata()) {
  throw new Error("partial declared browser version was accepted");
}
metadataContext.browserVersionInput.value = "151.0.7922.138";
metadataContext.hardwareModelInput.value = " fixture-hardware";
if (metadataContext.requireTestMetadata()) {
  throw new Error("capture metadata accepted surrounding whitespace");
}
metadataContext.hardwareModelInput.value = "fixture-hardware";
metadataContext.runNumberInput.value = "1.5";
if (metadataContext.requireTestMetadata()) {
  throw new Error("capture metadata accepted a non-integer run number");
}
metadataContext.runNumberInput.value = "1";
if (metadataContext.testMetadata().run_number !== 1) {
  throw new Error("test metadata did not preserve the validated integer run number");
}
metadataContext.candidateWasmOptimization = "not-applied";
if (metadataContext.requireTestMetadata()) {
  throw new Error("capture metadata accepted an unoptimized candidate");
}
metadataContext.candidateWasmOptimization = "wasm-opt-Oz";
metadataContext.candidateBuildId = `fixture-${"a".repeat(129)}`;
if (metadataContext.requireTestMetadata()) {
  throw new Error("capture metadata accepted an oversized build ID");
}
metadataContext.candidateBuildId = `fixture-${"a".repeat(64)}`;
metadataContext.candidateBundleHashAlgorithm = "unknown";
if (metadataContext.requireTestMetadata()) {
  throw new Error("capture metadata accepted an unsupported bundle-hash algorithm");
}
metadataContext.candidateBundleHashAlgorithm = "sha256-length-prefixed-v1";
metadataContext.candidateRenderer = "WebGPU";
if (metadataContext.requireTestMetadata()) {
  throw new Error("capture metadata accepted an unexpected renderer");
}
metadataContext.candidateRenderer = "WebGL2";
metadataContext.detectedBrowserFamily = "unknown";
if (metadataContext.requireTestMetadata()) {
  throw new Error("unknown detected browser was accepted");
}

void (async () => {
  const audioStart = source.indexOf("function updateAudioControls() {");
const audioEnd = source.indexOf("function toggleMute() {", audioStart);
if (audioStart < 0 || audioEnd < 0) {
  throw new Error("cannot locate audio probe lifecycle in feasibility.js");
}

function audioControl() {
  return { disabled: false, textContent: "" };
}

const audioContextFixture = {
  state: "suspended",
  currentTime: 1,
  destination: {},
  resumeCalls: 0,
  addEventListener() {},
  async resume() {
    this.resumeCalls += 1;
    this.state = "running";
  },
  createOscillator() {
    return {
      type: "",
      frequency: { setValueAtTime() {}, exponentialRampToValueAtTime() {} },
      connect() {},
      disconnect() {},
      start() {},
      stop() {},
      addEventListener() {},
    };
  },
  createGain() {
    return {
      gain: { setValueAtTime() {}, exponentialRampToValueAtTime() {} },
      connect() {},
      disconnect() {},
    };
  },
};
const audioContext = {
  audioContext: null,
  audioNeedsExplicitResume: false,
  audioProbeInFlight: false,
  audioLifecycleOperations: 0,
  audioLifecycleTransition: Promise.resolve(),
  audioButton: audioControl(),
  captureButton: { focus() {} },
  markEventButton: audioControl(),
  resetButton: audioControl(),
  downloadButton: audioControl(),
  toggleToolsButton: audioControl(),
  muteButton: audioControl(),
  masterGain: { gain: { setValueAtTime() {} } },
  captureActive: true,
  telemetry: {
    captureStoppedAt: null,
    audio: {
      supported: true,
      state: "suspended",
      gestureStarts: 0,
      backgroundSuspensions: 0,
      explicitResumes: 0,
      mutedPlaybackAttempts: 0,
      audiblePlaybackAttempts: 0,
      transitionFailures: 0,
      muted: false,
    },
  },
  refreshMetrics() {},
  updateCaptureControls() {},
  stoppedCaptureNeedsAudioResume() {
    return (
      !audioContext.captureActive &&
      audioContext.telemetry.captureStoppedAt !== null &&
      audioContext.audioContext !== null &&
      (audioContext.audioNeedsExplicitResume || audioContext.audioContext.state !== "running")
    );
  },
  Promise,
  window: {
    AudioContext: class FixtureAudioContext {
      constructor() {
        return audioContextFixture;
      }
    },
  },
  createAudioGraph: undefined,
  document: { hidden: false },
  showStatus(message) {
    audioContext.status = message;
  },
  Error,
};
vm.createContext(audioContext);
vm.runInContext(
  `${source.slice(audioStart, audioEnd)}\nthis.playAudioProbe = playAudioProbe;`,
  audioContext,
);
audioContext.audioNeedsExplicitResume = true;
await audioContext.playAudioProbe();
if (audioContextFixture.resumeCalls !== 1 || audioContext.telemetry.audio.explicitResumes !== 1) {
  throw new Error("explicit audio resume was not counted after a successful resume");
}
if (audioContext.telemetry.audio.gestureStarts !== 1 || audioContext.audioNeedsExplicitResume) {
  throw new Error("successful audio probe did not record a gesture and clear resume state");
}

const failedAudioContext = {
  ...audioContextFixture,
  state: "suspended",
  resumeCalls: 0,
  async resume() {
    this.resumeCalls += 1;
    throw new Error("fixture resume failure");
  },
};
audioContext.audioContext = failedAudioContext;
audioContext.audioNeedsExplicitResume = true;
await audioContext.playAudioProbe();
if (!audioContext.audioNeedsExplicitResume || !audioContext.status.includes("fixture resume failure")) {
  throw new Error("failed audio resume did not remain resumable with an actionable status");
}
if (audioContext.telemetry.audio.transitionFailures !== 1) {
  throw new Error("failed audio resume did not invalidate the active capture");
}
if (audioContext.telemetry.audio.gestureStarts !== 1) {
  throw new Error("failed audio resume was incorrectly counted as successful playback");
}

audioContext.captureActive = false;
audioContext.telemetry.audio.transitionFailures = 0;
await audioContext.playAudioProbe();
if (!audioContext.status.includes("Start a capture")) {
  throw new Error("audio probe was allowed outside an active capture");
}
if (audioContext.telemetry.audio.gestureStarts !== 1) {
  throw new Error("out-of-capture audio probe changed playback evidence");
}
audioContext.captureActive = true;
audioContext.telemetry.audio.transitionFailures = 1;

failedAudioContext.state = "suspended";
failedAudioContext.resumeCalls = 0;
failedAudioContext.resume = async function resume() {
  this.resumeCalls += 1;
  this.state = "running";
};
audioContext.audioNeedsExplicitResume = true;
audioContext.document.hidden = true;
await audioContext.playAudioProbe();
if (failedAudioContext.resumeCalls !== 0 || !audioContext.audioNeedsExplicitResume) {
  throw new Error("hidden page resumed audio or cleared explicit-resume state");
}
if (!audioContext.status.includes("foreground")) {
  throw new Error("hidden-page resume failure did not provide foreground guidance");
}
audioContext.document.hidden = false;

const stoppedCaptureAudioContext = {
  ...audioContextFixture,
  state: "interrupted",
  resumeCalls: 0,
  async resume() {
    this.resumeCalls += 1;
    this.state = "running";
  },
};
audioContext.audioContext = stoppedCaptureAudioContext;
audioContext.captureActive = false;
audioContext.telemetry.captureStoppedAt = 61_000;
audioContext.telemetry.audio.gestureStarts = 2;
audioContext.telemetry.audio.explicitResumes = 2;
audioContext.audioNeedsExplicitResume = true;
await audioContext.playAudioProbe();
if (
  stoppedCaptureAudioContext.resumeCalls !== 1 ||
  stoppedCaptureAudioContext.state !== "running" ||
  audioContext.telemetry.audio.gestureStarts !== 2 ||
  audioContext.telemetry.audio.explicitResumes !== 2 ||
  !audioContext.status.includes("ready for validation")
) {
  throw new Error("stopped capture could not recover audio without mutating evidence counters");
}

audioContext.captureActive = true;
audioContext.telemetry.captureStoppedAt = null;
audioContext.telemetry.audio.gestureStarts = 1;

const interruptedAudioContext = {
  ...audioContextFixture,
  state: "interrupted",
  resumeCalls: 0,
  async resume() {
    this.resumeCalls += 1;
    this.state = "running";
  },
};
audioContext.audioContext = interruptedAudioContext;
audioContext.captureActive = true;
audioContext.telemetry.audio.transitionFailures = 0;
audioContext.audioNeedsExplicitResume = true;
await audioContext.playAudioProbe();
if (
  interruptedAudioContext.resumeCalls !== 1 ||
  interruptedAudioContext.state !== "running" ||
  audioContext.telemetry.audio.explicitResumes !== 3
) {
  throw new Error("interrupted audio context did not explicitly resume");
}

const unknownStoppedAudioContext = {
  ...audioContextFixture,
  state: "closed-by-browser",
  resumeCalls: 0,
  async resume() {
    this.resumeCalls += 1;
    this.state = "running";
  },
};
audioContext.audioContext = unknownStoppedAudioContext;
audioContext.telemetry.audio.gestureStarts = 1;
audioContext.audioNeedsExplicitResume = true;
await audioContext.playAudioProbe();
if (
  unknownStoppedAudioContext.resumeCalls !== 1 ||
  unknownStoppedAudioContext.state !== "running" ||
  audioContext.telemetry.audio.explicitResumes !== 4
) {
  throw new Error("unknown non-running audio context did not explicitly resume");
}

const backgroundAudioContext = {
  state: "interrupted",
  suspendCalls: 0,
  async suspend() {
    this.suspendCalls += 1;
  },
};
audioContext.audioContext = backgroundAudioContext;
audioContext.audioNeedsExplicitResume = false;
audioContext.audioLifecycleOperations = 0;
audioContext.document.hidden = true;
audioContext.requestBackgroundAudioSuspension();
await audioContext.audioLifecycleTransition;
await Promise.resolve();
if (
  backgroundAudioContext.suspendCalls !== 0 ||
  audioContext.telemetry.audio.backgroundSuspensions !== 1 ||
  !audioContext.audioNeedsExplicitResume
) {
  throw new Error("browser-interrupted background audio was not recorded as suspended");
}

audioContext.document.hidden = false;
while (audioContext.audioLifecycleOperations > 0) {
  await Promise.resolve();
}

const resetLifecycleStart = source.indexOf("function updateAudioControls() {");
const resetLifecycleEnd = source.indexOf("function toggleTools() {", resetLifecycleStart);
if (resetLifecycleStart < 0 || resetLifecycleEnd < 0) {
  throw new Error("cannot locate audio close lifecycle in feasibility.js");
}
const resetLifecycleContext = {
  feasibilityEnabled: false,
  captureActive: false,
  audioContextPendingClose: null,
  audioNeedsExplicitResume: false,
  audioProbeInFlight: false,
  audioLifecycleOperations: 0,
  audioButton: audioControl(),
  muteButton: audioControl(),
  markEventButton: audioControl(),
  resetButton: audioControl(),
  downloadButton: audioControl(),
  toggleToolsButton: audioControl(),
  captureButton: { focus() {} },
  masterGain: {},
  telemetry: { audio: { supported: true, state: "running", transitionFailures: 0, muted: false } },
  physicalChecks: { querySelectorAll: () => [] },
  platformInput: { value: "desktop", focus() {} },
  hardwareModelInput: { value: "fixture" },
  osVersionInput: { value: "fixture" },
  browserFamilyInput: { value: "chrome" },
  browserVersionInput: { value: "151" },
  cacheStateInput: { value: "warm" },
  minimumVersionInput: { value: "no" },
  runNumberInput: { value: "1" },
  firstVisibleInput: { value: "1" },
  firstInputInput: { value: "2" },
  steadyMemoryInput: { value: "3" },
  peakMemoryInput: { value: "4" },
  thermalResultInput: { value: "no-warning" },
  reloadObservedInput: { value: "no" },
  eventLabelInput: { value: "fixture" },
  rejectForAudioLifecycle: () => false,
  setCaptureMetadataLocked() {
    resetLifecycleContext.metadataUnlocked = true;
  },
  showStatus(message) {
    resetLifecycleContext.status = message;
  },
  refreshMetrics() {},
  updateCaptureControls() {},
  stoppedCaptureNeedsAudioResume: () => false,
  Promise,
  Error,
};
resetLifecycleContext.clearCaptureEvidence = () => {
  resetLifecycleContext.evidenceCleared = true;
  resetLifecycleContext.telemetry.audio.transitionFailures = 0;
};
let resolveAudioClose;
const closingAudioContext = {
  state: "running",
  closeCalls: 0,
  close() {
    this.closeCalls += 1;
    return new Promise((resolve) => {
      resolveAudioClose = () => {
        this.state = "closed";
        resolve();
      };
    });
  },
};
resetLifecycleContext.audioContext = closingAudioContext;
vm.createContext(resetLifecycleContext);
vm.runInContext(
  `${source.slice(resetLifecycleStart, resetLifecycleEnd)}\nthis.resetReportForm = resetReportForm;`,
  resetLifecycleContext,
);
resetLifecycleContext.resetReportForm();
if (
  closingAudioContext.closeCalls !== 1 ||
  !resetLifecycleContext.metadataUnlocked ||
  resetLifecycleContext.audioLifecycleOperations !== 1 ||
  resetLifecycleContext.audioContextPendingClose !== closingAudioContext ||
  resetLifecycleContext.telemetry.audio.state !== "running"
) {
  throw new Error("report reset did not preserve an in-flight audio close state");
}
resolveAudioClose();
await Promise.resolve();
await Promise.resolve();
await Promise.resolve();
if (
  resetLifecycleContext.audioLifecycleOperations !== 0 ||
  resetLifecycleContext.audioContextPendingClose !== null ||
  resetLifecycleContext.telemetry.audio.state !== "not started" ||
  resetLifecycleContext.telemetry.audio.transitionFailures !== 0
) {
  throw new Error("successful audio close did not settle cleanly");
}

const resetCloseFailureContext = {
  ...resetLifecycleContext,
  audioContextPendingClose: null,
  audioLifecycleOperations: 0,
  telemetry: { audio: { supported: true, state: "running", transitionFailures: 0, muted: false } },
  showStatus(message) {
    resetCloseFailureContext.status = message;
  },
};
const closeFailureContext = {
  state: "running",
  async close() {
    throw new Error("fixture close failure");
  },
};
resetCloseFailureContext.audioContext = closeFailureContext;
vm.createContext(resetCloseFailureContext);
vm.runInContext(
  `${source.slice(resetLifecycleStart, resetLifecycleEnd)}\nthis.resetReportForm = resetReportForm;`,
  resetCloseFailureContext,
);
resetCloseFailureContext.resetReportForm();
await Promise.resolve();
await Promise.resolve();
await Promise.resolve();
await Promise.resolve();
await Promise.resolve();
if (
  resetCloseFailureContext.audioLifecycleOperations !== 0 ||
  resetCloseFailureContext.audioContextPendingClose !== null ||
  resetCloseFailureContext.telemetry.audio.transitionFailures !== 1 ||
  !resetCloseFailureContext.status.includes("fixture close failure")
) {
  throw new Error("failed audio close did not settle fail-closed");
}

const muteStart = source.indexOf("function toggleMute() {");
const muteEnd = source.indexOf("function markEvent() {", muteStart);
if (muteStart < 0 || muteEnd < 0) {
  throw new Error("cannot locate audio mute capture guard in feasibility.js");
}
const muteContext = {
  masterGain: { gain: { setValueAtTime() {} } },
  audioContext: { currentTime: 1 },
  captureActive: false,
  audioLifecycleBusy: () => false,
  showStatus(message) {
    muteContext.status = message;
  },
};
vm.createContext(muteContext);
vm.runInContext(`${source.slice(muteStart, muteEnd)}\nthis.toggleMute = toggleMute;`, muteContext);
muteContext.toggleMute();
if (!muteContext.status.includes("active capture")) {
  throw new Error("audio mute was allowed outside an active capture");
}

const startGuardStart = source.indexOf("function startCapture() {");
const startGuardEnd = source.indexOf("function stopCapture() {", startGuardStart);
if (startGuardStart < 0 || startGuardEnd < 0) {
  throw new Error("cannot locate capture-start audio-operation guard in feasibility.js");
}
const startGuardContext = {
  telemetry: { audio: { transitionFailures: 0 } },
  audioContext: null,
  audioProbeInFlight: false,
  audioLifecycleOperations: 1,
  audioButton: { focus() {} },
  rejectForAudioLifecycle() {
    startGuardContext.status = "Wait for audio operations to finish";
    return true;
  },
  showStatus(message) {
    startGuardContext.status = message;
  },
};
vm.createContext(startGuardContext);
vm.runInContext(
  `${source.slice(startGuardStart, startGuardEnd)}\nthis.startCapture = startCapture;`,
  startGuardContext,
);
startGuardContext.startCapture();
if (!startGuardContext.status.includes("audio operations")) {
  throw new Error("capture start did not reject an in-flight audio operation");
}
startGuardContext.rejectForAudioLifecycle = () => false;
startGuardContext.telemetry = { audio: { transitionFailures: 1 } };
startGuardContext.audioContext = null;
startGuardContext.resetButton = { focus() {} };
startGuardContext.startCapture();
if (!startGuardContext.status.includes("Reset the report form")) {
  throw new Error("capture start did not preserve a pre-capture audio transition failure");
}
startGuardContext.telemetry.audio.transitionFailures = 0;
startGuardContext.audioContext = { state: "running" };
startGuardContext.startCapture();
if (!startGuardContext.status.includes("reusing an existing audio context")) {
  throw new Error("capture start reused setup audio without explicit reset");
}
startGuardContext.audioContext = null;
startGuardContext.telemetry = { clientReadyMs: null, audio: { transitionFailures: 0 } };
startGuardContext.document = { hidden: false };
startGuardContext.shell = { dataset: { clientState: "loading" } };
startGuardContext.captureButton = { focus() {} };
startGuardContext.startCapture();
if (!startGuardContext.status.includes("finish loading")) {
  throw new Error("capture start accepted a client that was not ready");
}

const captureGuardStart = source.indexOf("function stopCapture() {");
const captureGuardEnd = source.indexOf("function updateAudioControls() {", captureGuardStart);
if (captureGuardStart < 0 || captureGuardEnd < 0) {
  throw new Error("cannot locate capture audio-operation guard in feasibility.js");
}
const captureGuardContext = {
  audioProbeInFlight: false,
  audioLifecycleOperations: 1,
  document: { hidden: false },
  audioButton: { focus() {} },
  rejectForAudioLifecycle() {
    captureGuardContext.status = "Wait for audio operations to finish";
    return true;
  },
  showStatus(message) {
    captureGuardContext.status = message;
  },
};
vm.createContext(captureGuardContext);
vm.runInContext(
  `${source.slice(captureGuardStart, captureGuardEnd)}\nthis.stopCapture = stopCapture;`,
  captureGuardContext,
);
captureGuardContext.stopCapture();
if (!captureGuardContext.status.includes("audio operations")) {
  throw new Error("capture stop did not reject an in-flight audio operation");
}

const stoppedCaptureContext = {
  audioProbeInFlight: false,
  audioLifecycleOperations: 0,
  captureActive: true,
  telemetry: {
    captureStoppedAt: null,
    hiddenStartedAt: null,
    finalOrientation: null,
  },
  performance: { now: () => 61_000 },
  window: { screen: { orientation: { type: "landscape-primary" } } },
  document: { hidden: false },
  tools: { classList: { remove() {} } },
  toggleToolsButton: { textContent: "", setAttribute() {} },
  captureButton: { textContent: "" },
  audioContext: { state: "interrupted" },
  audioNeedsExplicitResume: true,
  rejectForAudioLifecycle: () => false,
  updateAudioControls() {
    stoppedCaptureContext.controlsUpdated = true;
  },
  showStatus(message) {
    stoppedCaptureContext.status = message;
  },
  sampleMemory() {},
  refreshMetrics() {},
};
vm.createContext(stoppedCaptureContext);
vm.runInContext(
  `${source.slice(captureGuardStart, captureGuardEnd)}\nthis.stopCapture = stopCapture;`,
  stoppedCaptureContext,
);
stoppedCaptureContext.stopCapture();
if (
  !stoppedCaptureContext.controlsUpdated ||
  !stoppedCaptureContext.status.includes("Resume audio") ||
  stoppedCaptureContext.captureButton.textContent !== "Restart capture"
) {
  throw new Error("capture stop did not expose required post-stop audio recovery");
}

const resetGuardStart = source.indexOf("function resetReportForm() {");
const resetGuardEnd = source.indexOf("function toggleTools() {", resetGuardStart);
if (resetGuardStart < 0 || resetGuardEnd < 0) {
  throw new Error("cannot locate report-reset audio-operation guard in feasibility.js");
}
const resetGuardContext = {
  captureActive: false,
  audioProbeInFlight: false,
  audioLifecycleOperations: 1,
  captureButton: { focus() {} },
  rejectForAudioLifecycle() {
    resetGuardContext.status = "Wait for audio operations to finish";
    return true;
  },
  showStatus(message) {
    resetGuardContext.status = message;
  },
};
vm.createContext(resetGuardContext);
vm.runInContext(
  `${source.slice(resetGuardStart, resetGuardEnd)}\nthis.resetReportForm = resetReportForm;`,
  resetGuardContext,
);
resetGuardContext.resetReportForm();
if (!resetGuardContext.status.includes("audio operations")) {
  throw new Error("report reset did not reject an in-flight audio operation");
}

const markEventStart = source.indexOf("function appendVisibleCaptureEvent(");
const markEventEnd = source.indexOf("function report(", markEventStart);
if (markEventStart < 0 || markEventEnd < 0) {
  throw new Error("cannot locate event-marker capture guard in feasibility.js");
}
const markEventContext = {
  captureActive: false,
  audioLifecycleBusy: () => false,
  captureButton: { focus() {} },
  showStatus(message) {
    markEventContext.status = message;
  },
};
vm.createContext(markEventContext);
vm.runInContext(
  `${source.slice(markEventStart, markEventEnd)}\nthis.markEvent = markEvent;`,
  markEventContext,
);
markEventContext.markEvent();
if (!markEventContext.status.includes("Start a capture")) {
  throw new Error("event marker implicitly started or mutated capture evidence");
}
markEventContext.captureActive = true;
markEventContext.telemetry = { events: [] };
markEventContext.eventLabelInput = { value: "bad\nlabel", focus() {} };
markEventContext.document = { visibilityState: "visible" };
markEventContext.window = { screen: { orientation: { type: "landscape-primary" } } };
markEventContext.captureDuration = () => 10;
markEventContext.refreshMetrics = () => {};
markEventContext.markEvent();
if (markEventContext.telemetry.events.length !== 0 || !markEventContext.status.includes("control characters")) {
  throw new Error("event marker accepted a control-character label");
}
markEventContext.eventLabelInput.value = "fixture";
markEventContext.document.visibilityState = "hidden";
markEventContext.markEvent();
if (markEventContext.telemetry.events.length !== 0 || !markEventContext.status.includes("visible capture")) {
  throw new Error("event marker accepted hidden-page evidence");
}

const captureEvidenceStart = source.indexOf("function requireCaptureEvidence() {");
const captureEvidenceEnd = source.indexOf("function clearCaptureEvidence() {", captureEvidenceStart);
if (captureEvidenceStart < 0 || captureEvidenceEnd < 0) {
  throw new Error("cannot locate capture evidence validation in feasibility.js");
}
const captureEvidenceContext = {
  audioButton: { focus() {} },
  cacheStateInput: { value: "warm" },
  platformInput: { value: "desktop" },
  presentationTierInput: { value: "default" },
  captureButton: { focus() {} },
  canvas: { focus() {} },
  document: { hidden: false },
  telemetry: {
    audio: {
      state: "interrupted",
      transitionFailures: 0,
      audiblePlaybackAttempts: 1,
      mutedPlaybackAttempts: 1,
      gestureStarts: 2,
      muteChanges: 2,
      muted: false,
      backgroundSuspensions: 0,
      explicitResumes: 0,
    },
    frameCount: 1,
    fpsSamples: [60],
    minimumFps: 60,
    maximumFps: 60,
    frameGapSamplesMs: [16],
    pointerContacts: 1,
    captureStartedAt: 0,
    captureStoppedAt: 61_000,
    hiddenStartedAt: null,
    hiddenDurationMs: 0,
    hiddenDurationsMs: [],
    restoredFromPageCache: false,
    visibilityChanges: 0,
    orientationChanges: 0,
    pageHideCount: 0,
    pageShowCount: 0,
  },
  performance: { now: () => 61_000 },
  rejectForAudioLifecycle: () => false,
  showStatus(message) {
    captureEvidenceContext.status = message;
  },
};
vm.createContext(captureEvidenceContext);
vm.runInContext(
  `${source.slice(start, end)}\n${source.slice(captureEvidenceStart, captureEvidenceEnd)}\nthis.requireCaptureEvidence = requireCaptureEvidence;`,
  captureEvidenceContext,
);
if (captureEvidenceContext.requireCaptureEvidence()) {
  throw new Error("capture export accepted a non-running audio context");
}
if (!captureEvidenceContext.status.includes("Resume audio")) {
  throw new Error("non-running audio export rejection lacked recovery guidance");
}
captureEvidenceContext.telemetry.audio.state = "running";
captureEvidenceContext.telemetry.audio.backgroundSuspensions = -1;
captureEvidenceContext.telemetry.audio.explicitResumes = -1;
if (captureEvidenceContext.requireCaptureEvidence()) {
  throw new Error("interaction capture accepted negative audio lifecycle counters");
}
if (!captureEvidenceContext.status.includes("exactly zero")) {
  throw new Error("negative interaction audio counters lacked restart guidance");
}
captureEvidenceContext.telemetry.audio.backgroundSuspensions = 0;
captureEvidenceContext.telemetry.audio.explicitResumes = 0;
captureEvidenceContext.telemetry.captureStoppedAt = 1_800_001;
if (captureEvidenceContext.requireCaptureEvidence()) {
  throw new Error("capture export accepted an unbounded capture duration");
}
if (!captureEvidenceContext.status.includes("cannot exceed 30")) {
  throw new Error("unbounded capture duration lacked restart guidance");
}
captureEvidenceContext.telemetry.captureStoppedAt = 61_000;
captureEvidenceContext.telemetry.hiddenDurationMs = 2;
if (captureEvidenceContext.requireCaptureEvidence()) {
  throw new Error("capture export accepted an inconsistent hidden-duration sum");
}
if (!captureEvidenceContext.status.includes("Hidden interval timing is inconsistent")) {
  throw new Error("inconsistent hidden duration lacked restart guidance");
}
captureEvidenceContext.telemetry.hiddenDurationMs = 0;
captureEvidenceContext.telemetry.fpsSamples = [Number.NaN];
if (captureEvidenceContext.requireCaptureEvidence()) {
  throw new Error("capture export accepted non-finite frame-rate evidence");
}
if (!captureEvidenceContext.status.includes("consistent finite frame-rate samples")) {
  throw new Error("non-finite frame-rate evidence lacked actionable guidance");
}
captureEvidenceContext.telemetry.fpsSamples = [60];
captureEvidenceContext.platformInput.value = "iphone";
captureEvidenceContext.telemetry.minimumFps = 29;
if (captureEvidenceContext.requireCaptureEvidence()) {
  throw new Error("capture export accepted frame rate below the mobile floor");
}
if (!captureEvidenceContext.status.includes("30 FPS mobile floor")) {
  throw new Error("mobile frame floor failure lacked actionable guidance");
}
captureEvidenceContext.telemetry.minimumFps = 54;
if (captureEvidenceContext.requireCaptureEvidence()) {
  throw new Error("capture export accepted default tier below the full-quality threshold");
}
if (!captureEvidenceContext.status.includes("reduced presentation tier")) {
  throw new Error("quality-fallback requirement lacked actionable guidance");
}
captureEvidenceContext.platformInput.value = "desktop";
captureEvidenceContext.telemetry.minimumFps = 60;

const clearCaptureStart = source.indexOf("function clearCaptureEvidence() {");
const clearCaptureEnd = source.indexOf("function feasibilityFilename(", clearCaptureStart);
if (clearCaptureStart < 0 || clearCaptureEnd < 0) {
  throw new Error("cannot locate capture evidence reset in feasibility.js");
}
const clearCaptureContext = {
  captureActive: true,
  telemetry: {
    captureStartedAt: 1,
    captureStoppedAt: 2,
    hiddenStartedAt: 3,
    hiddenDurationMs: 4,
    hiddenDurationsMs: [4],
    frameCount: 5,
    minimumFps: 6,
    maximumFps: 7,
    fpsSamples: [6],
    frameGapSamplesMs: [8],
    maximumFrameGapMs: 8,
    currentFps: 6,
    currentJsHeapBytes: 9,
    peakJsHeapBytes: 10,
    pointerContacts: 11,
    visibilityChanges: 12,
    orientationChanges: 13,
    orientationStates: ["portrait-primary"],
    initialOrientation: "landscape-primary",
    finalOrientation: "landscape-secondary",
    pageHideCount: 14,
    pageShowCount: 15,
    restoredFromPageCache: true,
    events: [{ label: "fixture" }],
    audio: {
      gestureStarts: 16,
      backgroundSuspensions: 17,
      explicitResumes: 18,
      muteChanges: 19,
      mutedPlaybackAttempts: 20,
      audiblePlaybackAttempts: 21,
      transitionFailures: 22,
      muted: true,
    },
  },
  captureButton: { textContent: "Stop capture" },
  updateCaptureControls() {
    clearCaptureContext.controlsUpdated = true;
  },
};
vm.createContext(clearCaptureContext);
vm.runInContext(
  `${source.slice(clearCaptureStart, clearCaptureEnd)}\nthis.clearCaptureEvidence = clearCaptureEvidence;`,
  clearCaptureContext,
);
clearCaptureContext.clearCaptureEvidence();
if (
  clearCaptureContext.captureActive ||
  clearCaptureContext.telemetry.captureStartedAt !== null ||
  clearCaptureContext.telemetry.captureStoppedAt !== null ||
  clearCaptureContext.telemetry.hiddenStartedAt !== null ||
  clearCaptureContext.telemetry.hiddenDurationMs !== 0 ||
  clearCaptureContext.telemetry.hiddenDurationsMs.length !== 0 ||
  clearCaptureContext.telemetry.frameCount !== 0 ||
  clearCaptureContext.telemetry.minimumFps !== null ||
  clearCaptureContext.telemetry.maximumFps !== null ||
  clearCaptureContext.telemetry.fpsSamples.length !== 0 ||
  clearCaptureContext.telemetry.frameGapSamplesMs.length !== 0 ||
  clearCaptureContext.telemetry.maximumFrameGapMs !== 0 ||
  clearCaptureContext.telemetry.currentFps !== null ||
  clearCaptureContext.telemetry.currentJsHeapBytes !== null ||
  clearCaptureContext.telemetry.peakJsHeapBytes !== null ||
  clearCaptureContext.telemetry.pointerContacts !== 0 ||
  clearCaptureContext.telemetry.visibilityChanges !== 0 ||
  clearCaptureContext.telemetry.orientationChanges !== 0 ||
  clearCaptureContext.telemetry.orientationStates.length !== 0 ||
  clearCaptureContext.telemetry.initialOrientation !== null ||
  clearCaptureContext.telemetry.finalOrientation !== null ||
  clearCaptureContext.telemetry.pageHideCount !== 0 ||
  clearCaptureContext.telemetry.pageShowCount !== 0 ||
  clearCaptureContext.telemetry.restoredFromPageCache ||
  clearCaptureContext.telemetry.events.length !== 0 ||
  Object.values(clearCaptureContext.telemetry.audio).some((value) => value !== 0 && value !== false) ||
  clearCaptureContext.captureButton.textContent !== "Start capture" ||
  !clearCaptureContext.controlsUpdated
) {
  throw new Error("capture evidence reset retained data from the completed run");
}

const captureControlsStart = source.indexOf("function updateCaptureControls() {");
const stoppedCaptureStart = source.indexOf("function stoppedCaptureNeedsAudioResume() {", captureControlsStart);
const captureControlsEnd = source.indexOf("function updateAudioControls() {", stoppedCaptureStart);
if (captureControlsStart < 0 || stoppedCaptureStart < 0 || captureControlsEnd < 0) {
  throw new Error("cannot locate capture control state in feasibility.js");
}
const captureControlsContext = {
  captureActive: false,
  telemetry: {
    clientReadyMs: 1_000,
    captureStoppedAt: 61_000,
    audio: { transitionFailures: 0 },
  },
  shell: { dataset: { clientState: "ready" } },
  captureButton: {},
  markEventButton: {},
  resetButton: {},
  downloadButton: {},
  toggleToolsButton: {},
  audioLifecycleBusy: () => false,
  audioContext: { state: "interrupted" },
  audioNeedsExplicitResume: true,
};
vm.createContext(captureControlsContext);
vm.runInContext(
  `${source.slice(captureControlsStart, captureControlsEnd)}\nthis.updateCaptureControls = updateCaptureControls;`,
  captureControlsContext,
);
captureControlsContext.updateCaptureControls();
if (!captureControlsContext.downloadButton.disabled) {
  throw new Error("download control enabled before stopped-capture audio recovery");
}
captureControlsContext.audioContext.state = "running";
captureControlsContext.audioNeedsExplicitResume = false;
captureControlsContext.updateCaptureControls();
if (captureControlsContext.downloadButton.disabled) {
  throw new Error("download control remained disabled after stopped-capture audio recovery");
}
captureControlsContext.telemetry.audio.transitionFailures = 1;
captureControlsContext.updateCaptureControls();
if (!captureControlsContext.downloadButton.disabled) {
  throw new Error("download control enabled with a failed audio transition");
}
captureControlsContext.telemetry.audio.transitionFailures = 0;
captureControlsContext.telemetry.clientReadyMs = null;
captureControlsContext.shell.dataset.clientState = "loading";
captureControlsContext.updateCaptureControls();
if (!captureControlsContext.captureButton.disabled) {
  throw new Error("capture control enabled before client readiness");
}
captureControlsContext.captureActive = true;
captureControlsContext.updateCaptureControls();
if (captureControlsContext.captureButton.disabled) {
  throw new Error("active capture could not be stopped after client readiness state changed");
}

const downloadStart = source.indexOf("function downloadReport() {");
const downloadEnd = source.indexOf("function resetReportForm() {", downloadStart);
if (downloadStart < 0 || downloadEnd < 0) {
  throw new Error("cannot locate report download transaction in feasibility.js");
}
const downloadFunction = `${source.slice(downloadStart, downloadEnd)}\nthis.downloadReport = downloadReport;`;
function runDownloadScenario({
  stringify = JSON.stringify,
  createObjectURL = () => "blob:fixture",
  revokeObjectURL,
  createElement,
  filename = () => "fixture.json",
} = {}) {
  const state = {
    cleared: false,
    clicked: false,
    consoleErrors: 0,
    refreshed: false,
    revoked: false,
    status: "",
    unlocked: false,
  };
  const context = {
    requireTestMetadata: () => true,
    requirePhysicalChecks: () => true,
    requireExternalObservations: () => true,
    requireStoppedCapture: () => true,
    requireCaptureEvidence: () => true,
    testMetadata: () => ({ platform: "desktop" }),
    report: () => ({ evidence: 1 }),
    feasibilityFilename: filename,
    setCaptureMetadataLocked() {
      state.unlocked = true;
    },
    clearCaptureEvidence() {
      state.cleared = true;
    },
    refreshMetrics() {
      state.refreshed = true;
    },
    showStatus(message) {
      state.status = message;
    },
    document: {
      createElement:
        createElement ??
        (() => ({
          click() {
            state.clicked = true;
          },
        })),
    },
    console: {
      error() {
        state.consoleErrors += 1;
      },
    },
    Date,
    JSON: { stringify },
    Blob,
    URL: {
      createObjectURL,
      revokeObjectURL:
        revokeObjectURL ??
        (() => {
          state.revoked = true;
        }),
    },
    Error,
  };
  vm.createContext(context);
  vm.runInContext(downloadFunction, context);
  context.downloadReport();
  return state;
}

let downloadState = runDownloadScenario({
  stringify() {
    throw new Error("fixture serialization failure");
  },
});
if (
  !downloadState.status.includes("serialization failed") ||
  downloadState.unlocked ||
  downloadState.cleared ||
  downloadState.clicked ||
  downloadState.revoked
) {
  throw new Error("failed report serialization altered completed evidence");
}

downloadState = runDownloadScenario({
  createObjectURL() {
    throw new Error("fixture object URL failure");
  },
});
if (
  !downloadState.status.includes("preparation failed") ||
  downloadState.unlocked ||
  downloadState.cleared ||
  downloadState.clicked ||
  downloadState.revoked
) {
  throw new Error("failed report preparation altered completed evidence");
}

downloadState = runDownloadScenario({
  createElement() {
    throw new Error("fixture anchor failure");
  },
});
if (
  !downloadState.status.includes("download failed") ||
  downloadState.unlocked ||
  downloadState.cleared ||
  downloadState.clicked ||
  !downloadState.revoked
) {
  throw new Error("failed anchor preparation did not preserve evidence and revoke its URL");
}

downloadState = runDownloadScenario({
  filename() {
    throw new Error("fixture filename failure");
  },
});
if (
  !downloadState.status.includes("download failed") ||
  downloadState.unlocked ||
  downloadState.cleared ||
  downloadState.clicked ||
  !downloadState.revoked
) {
  throw new Error("failed filename construction did not preserve evidence and revoke its URL");
}

downloadState = runDownloadScenario({
  createElement: () => ({
    click() {
      throw new Error("fixture click failure");
    },
  }),
});
if (
  !downloadState.status.includes("download failed") ||
  downloadState.unlocked ||
  downloadState.cleared ||
  !downloadState.revoked
) {
  throw new Error("failed download dispatch did not preserve evidence and revoke its URL");
}

downloadState = runDownloadScenario();
if (
  downloadState.status !== "Downloaded fixture.json" ||
  !downloadState.unlocked ||
  !downloadState.cleared ||
  !downloadState.clicked ||
  !downloadState.revoked ||
  !downloadState.refreshed
) {
  throw new Error("successful report dispatch did not clear evidence transactionally");
}

downloadState = runDownloadScenario({
  revokeObjectURL() {
    throw new Error("fixture cleanup failure");
  },
});
if (
  downloadState.status !== "Downloaded fixture.json" ||
  !downloadState.unlocked ||
  !downloadState.cleared ||
  !downloadState.clicked ||
  !downloadState.refreshed ||
  downloadState.consoleErrors !== 1
) {
  throw new Error("URL cleanup failure prevented successful report finalization");
}

const lifecycleOrder = [];
audioContext.audioLifecycleTransition = Promise.resolve();
audioContext.telemetry.audio.gestureStarts = 0;
const firstTransition = audioContext.queueAudioLifecycleTransition(async () => {
  lifecycleOrder.push("first-start");
  await Promise.resolve();
  lifecycleOrder.push("first-end");
});
if (audioContext.audioLifecycleOperations !== 1 || !audioContext.audioButton.disabled) {
  throw new Error("queued audio lifecycle transition did not expose its in-flight state");
}
const secondTransition = audioContext.queueAudioLifecycleTransition(async () => {
  lifecycleOrder.push("second");
});
await Promise.all([firstTransition, secondTransition]);
await Promise.resolve();
if (audioContext.audioLifecycleOperations !== 0 || audioContext.audioButton.disabled) {
  throw new Error("completed audio lifecycle transitions retained in-flight state");
}
if (lifecycleOrder.join(",") !== "first-start,first-end,second") {
  throw new Error(`audio lifecycle transitions overlapped: ${lifecycleOrder.join(",")}`);
}

// Match chrome is driven only by WASM session events; it never polls or
// fetches match access, and its turn timer is local arithmetic.
const chromeStart = bootstrapSource.indexOf("let signedIn = false;");
const chromeEnd = bootstrapSource.indexOf("function renderLobbyList(lobbies) {", chromeStart);
if (chromeStart < 0 || chromeEnd < 0) {
  throw new Error("cannot locate match session chrome in bootstrap.js");
}
const chromeSource = bootstrapSource.slice(chromeStart, chromeEnd);
for (const forbidden of ["apiRequest(", "fetch(", "/api/matches/", "setTimeout("]) {
  if (chromeSource.includes(forbidden)) {
    throw new Error(`match session chrome must not use ${forbidden}`);
  }
}
const chromeListeners = new Map();
const chromeCalls = [];
const chromeIntervals = new Map();
let nextChromeInterval = 1;
const chromeElement = (extra = {}) => ({
  hidden: true,
  disabled: false,
  textContent: "",
  dataset: {},
  removeAttribute(name) {
    delete this.dataset[name.replace(/^data-/u, "").replace(/-([a-z])/gu, (_, letter) => letter.toUpperCase())];
  },
  addEventListener(name, listener) {
    chromeListeners.set(`element:${name}`, listener);
  },
  ...extra,
});
const chromeWasm = {};
for (const name of [
  "session_start",
  "session_set_active",
  "subscribe_match",
  "unsubscribe_match",
  "unsubscribe_lobby",
  "send_concession",
  "interrupt_match_gesture",
]) {
  chromeWasm[name] = (...args) => chromeCalls.push([name, ...args]);
}
const chromeContext = {
  URL,
  URLSearchParams,
  JSON,
  Number,
  Math,
  Object,
  Date: { now: () => 1_000 },
  console: { error() {} },
  navigator: { onLine: true },
  document: {
    visibilityState: "visible",
    addEventListener(name, listener) {
      chromeListeners.set(`document:${name}`, listener);
    },
  },
  window: {
    location: { href: "https://pwmtf.example/?player_one=1", search: "" },
    history: {
      pushState(state, title, url) {
        chromeCalls.push(["pushState", String(url)]);
      },
    },
    setInterval(callback, delay) {
      const id = nextChromeInterval;
      nextChromeInterval += 1;
      chromeIntervals.set(id, { callback, delay });
      return id;
    },
    clearInterval(id) {
      chromeIntervals.delete(id);
    },
    addEventListener(name, listener) {
      chromeListeners.set(`window:${name}`, listener);
    },
    confirm: () => true,
  },
  MATCH_ID_PATTERN: /^\d{1,39}$/u,
  wasmModule: chromeWasm,
  feasibility: null,
  localAudio: { context: null },
  playRevisionCue() {
    chromeCalls.push(["cue"]);
  },
  refreshSocialState: async () => {
    chromeCalls.push(["social"]);
  },
  refreshLobby: async () => {
    chromeCalls.push(["lobby"]);
  },
  clearLobby() {
    chromeCalls.push(["clearLobby"]);
  },
  socialPanel: { open: false },
  socialStatus: { textContent: "" },
  matchStatus: chromeElement(),
  concedeMatchButton: chromeElement(),
  offerRematchButton: chromeElement(),
  matchResult: chromeElement(),
  matchResultTitle: chromeElement(),
  matchResultDetail: chromeElement(),
  turnTimer: chromeElement(),
};
vm.createContext(chromeContext);
vm.runInContext(
  `${chromeSource}
this.handleSessionEvent = handleSessionEvent;
this.enterMatch = enterMatch;
this.ensureSession = ensureSession;
this.signIn = () => { signedIn = true; };`,
  chromeContext,
);
const sent = (name) => chromeCalls.filter((call) => call[0] === name);
const emit = (name, detail) => chromeContext.handleSessionEvent(name, JSON.stringify(detail));
const matchEvent = (overrides = {}) => ({
  match_id: "42",
  revision: 3,
  seat: 1,
  active_player: 1,
  winner: 0,
  completion_reason: "",
  ball_in_hand: false,
  pending: false,
  playing: false,
  deadline_at_ms: 10_000,
  server_offset_ms: 500,
  clock_uncertainty_ms: 20,
  ...overrides,
});
const status = () => chromeContext.matchStatus.textContent;

chromeContext.signIn();
chromeContext.ensureSession();
chromeContext.ensureSession();
if (sent("session_start").length !== 1) {
  throw new Error("session did not start exactly once");
}
chromeContext.enterMatch("42", { push: true });
if (
  sent("subscribe_match").length !== 1 ||
  sent("subscribe_match")[0][1] !== "42" ||
  sent("pushState").length !== 1 ||
  !sent("pushState")[0][1].endsWith("/?match=42")
) {
  throw new Error("entering a match did not push history and subscribe without reloading");
}
emit("connection", { status: "ready" });
if (sent("social").length !== 1 || status() !== "Loading match…") {
  throw new Error("ready connection did not catch up social lists while awaiting match state");
}
emit("match", matchEvent());
if (status() !== "Connected · revision 3 · your turn" || chromeContext.concedeMatchButton.hidden) {
  throw new Error(`match event did not render authoritative turn state: ${status()}`);
}
// remaining = deadline - (now + offset) + uncertainty = 10000 - 1500 + 20.
if (chromeContext.turnTimer.hidden || chromeContext.turnTimer.textContent !== "9s") {
  throw new Error(`turn timer did not use the event clock sample: ${chromeContext.turnTimer.textContent}`);
}
if ([...chromeIntervals.values()].some((interval) => interval.delay !== 250) || chromeIntervals.size !== 1) {
  throw new Error("turn timer did not run exactly one 250 ms local tick");
}
if (chromeContext.matchStatus.dataset.revision !== "3" || chromeContext.matchStatus.dataset.seat !== "1") {
  throw new Error("match status did not expose the authoritative revision and seat");
}
emit("match", matchEvent({ pending: true }));
if (!status().includes("waiting for authority") || !chromeContext.concedeMatchButton.disabled) {
  throw new Error("pending command did not disable duplicate controls");
}
emit("match", matchEvent({ revision: 4, active_player: 2, playing: true }));
if (!status().includes("revision 4 · shot in progress") || sent("cue").length !== 1) {
  throw new Error("accepted shot playback was not presented");
}
emit("match", matchEvent({ revision: 4, active_player: 2 }));
if (status() !== "Connected · revision 4 · player 2's turn") {
  throw new Error(`opponent turn was not presented: ${status()}`);
}
emit("rejected", { match_id: "42", reason: "stale-revision" });
if (!status().startsWith("The table changed before your move arrived.") || !status().includes("Connected · revision 4")) {
  throw new Error(`rejection reason was not presented: ${status()}`);
}
emit("rejected", { match_id: "7", reason: "wrong-turn" });
if (status().includes("not your turn")) {
  throw new Error("rejection for another match changed this match's chrome");
}
emit("match", matchEvent({ match_id: "7", revision: 99 }));
emit("match", matchEvent({ seat: 3, revision: 98 }));
if (chromeContext.matchStatus.dataset.revision !== "4") {
  throw new Error("foreign or invalid match events were not ignored");
}
emit("match", matchEvent({ revision: 5 }));
if (status().includes("The table changed")) {
  throw new Error("a newer revision did not clear the previous rejection");
}
const subscriptionsBeforeForeignStart = sent("subscribe_match").length;
emit("match-started", { lobby_id: "0", match_id: "44" });
if (sent("subscribe_match").length !== subscriptionsBeforeForeignStart || chromeContext.matchStatus.dataset.revision !== "5") {
  throw new Error("an unrelated match start pulled this tab out of its in-progress match");
}
emit("connection", { status: "reconnecting" });
if (status() !== "Connection lost · reconnecting" || !chromeContext.concedeMatchButton.hidden) {
  throw new Error("lost connection was not presented");
}
emit("connection", { status: "ready" });
chromeContext.document.visibilityState = "hidden";
chromeListeners.get("document:visibilitychange")();
const lastActivity = sent("session_set_active").at(-1);
if (lastActivity?.[1] !== false || sent("interrupt_match_gesture").length !== 1) {
  throw new Error("hidden tab did not interrupt gestures and deactivate the session");
}
chromeContext.document.visibilityState = "visible";
chromeListeners.get("document:visibilitychange")();
if (sent("session_set_active").at(-1)?.[1] !== true) {
  throw new Error("visible tab did not reactivate the session");
}
chromeListeners.get("element:click")();
if (sent("send_concession").at(-1)?.[1] !== 1) {
  throw new Error("concession did not use the seat from the match event");
}
emit("match", matchEvent({
  revision: 6,
  active_player: 0,
  winner: 1,
  completion_reason: "legal-eight-ball",
  deadline_at_ms: null,
}));
if (
  !chromeContext.matchResultTitle.textContent.includes("You win") ||
  chromeContext.matchResult.hidden ||
  !chromeContext.concedeMatchButton.hidden ||
  chromeContext.offerRematchButton.hidden ||
  !chromeContext.turnTimer.hidden ||
  chromeIntervals.size !== 0
) {
  throw new Error("authoritative completion did not present the result and stop the timer");
}
emit("match-started", { lobby_id: "11", match_id: "43" });
if (sent("subscribe_match").at(-1)?.[1] !== "43" || !sent("pushState").at(-1)[1].endsWith("?match=43")) {
  throw new Error("lobby match start did not enter the new match without reloading");
}
emit("subscription-denied", { kind: "match", id: "43" });
if (status() !== "You are not a participant in this match.") {
  throw new Error("denied match subscription was not presented");
}
emit("social-changed", {});
if (sent("social").length < 3) {
  throw new Error("social push did not refetch social lists");
}

  console.log("browser telemetry self-tests passed");
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
