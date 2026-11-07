// Physical-device feasibility capture tooling. Loaded only by dynamic import
// when the page URL contains `?feasibility`; never part of normal play.
// Candidate identity stays injected in bootstrap.js and arrives through
// `startFeasibility`, so identity tooling keeps a single source of truth.
const shell = document.querySelector("#game-shell");
const canvas = document.querySelector("#pwmtf-canvas");
const tools = document.querySelector("#feasibility-tools");
const platformInput = document.querySelector("#test-platform");
const hardwareModelInput = document.querySelector("#hardware-model");
const osVersionInput = document.querySelector("#os-version");
const browserFamilyInput = document.querySelector("#browser-family");
const browserVersionInput = document.querySelector("#browser-version");
const cacheStateInput = document.querySelector("#cache-state");
const minimumVersionInput = document.querySelector("#minimum-version-run");
const presentationTierInput = document.querySelector("#presentation-tier");
const runNumberInput = document.querySelector("#run-number");
const physicalChecks = document.querySelector("#physical-checks");
const firstVisibleInput = document.querySelector("#first-visible-ms");
const firstInputInput = document.querySelector("#first-input-ms");
const steadyMemoryInput = document.querySelector("#steady-memory-mib");
const peakMemoryInput = document.querySelector("#peak-memory-mib");
const thermalResultInput = document.querySelector("#thermal-result");
const reloadObservedInput = document.querySelector("#reload-observed");
const metricsOutput = document.querySelector("#feasibility-metrics");
const candidateOutput = document.querySelector("#candidate-identity");
const captureButton = document.querySelector("#capture-toggle");
const audioButton = document.querySelector("#audio-probe");
const muteButton = document.querySelector("#audio-mute");
const markEventButton = document.querySelector("#mark-event");
const eventLabelInput = document.querySelector("#event-label");
const downloadButton = document.querySelector("#download-report");
const resetButton = document.querySelector("#reset-report");
const toggleToolsButton = document.querySelector("#toggle-tools");
const statusOutput = document.querySelector("#feasibility-status");
let candidateBuildId = "";
let candidateSourceHash = "";
let candidateBundleHash = "";
let candidateBundleHashAlgorithm = "";
let candidateWasmOptimization = "";
let candidateBevyVersion = "";
let candidateRenderer = "";
let activePresentationTier = "default";
let navigationStartedAt = 0;
// True once startFeasibility has wired the capture panel.
let feasibilityEnabled = false;
const detectedBrowserFamily = detectBrowserFamily(navigator.userAgent);
const detectedPlatform = detectPlatform(navigator.userAgent, navigator.maxTouchPoints);
const PHYSICAL_CHECKS = [
  ["first_load", "First/warm load reaches the table"],
  ["aiming", "Held-contact aiming is continuous"],
  ["power", "Power drag is bounded"],
  ["touch_reset", "New touch has no stale state"],
  ["resize", "Landscape resize preserves the table"],
  ["safe_area", "Chrome and safe areas do not hide controls"],
  ["orientation", "Portrait notice and landscape restore work"],
  ["background", "Background/foreground restores render and input"],
  ["browser_chrome", "Browser chrome expansion/collapse keeps controls visible"],
  ["audio", "Gesture, mute, suspend, and explicit resume work"],
  ["input_response", "No visible delayed aiming"],
  ["memory", "No reload/eviction; memory reaches a plateau"],
  ["thermal", "No severe throttling or thermal warning"],
];

const telemetry = {
  schemaVersion: 12,
  clientReadyMs: null,
  firstCanvasContactMs: null,
  pointerContacts: 0,
  captureStartedAt: null,
  captureStoppedAt: null,
  hiddenStartedAt: null,
  hiddenDurationMs: 0,
  hiddenDurationsMs: [],
  frameCount: 0,
  minimumFps: null,
  maximumFps: null,
  fpsSamples: [],
  frameGapSamplesMs: [],
  maximumFrameGapMs: 0,
  currentFps: null,
  currentJsHeapBytes: null,
  peakJsHeapBytes: null,
  visibilityChanges: 0,
  orientationChanges: 0,
  orientationStates: [],
  initialOrientation: null,
  finalOrientation: null,
  pageHideCount: 0,
  pageShowCount: 0,
  restoredFromPageCache: false,
  events: [],
  audio: {
    supported: Boolean(window.AudioContext || window.webkitAudioContext),
    state: Boolean(window.AudioContext || window.webkitAudioContext) ? "not started" : "unsupported",
    gestureStarts: 0,
    backgroundSuspensions: 0,
    explicitResumes: 0,
    muteChanges: 0,
    mutedPlaybackAttempts: 0,
    audiblePlaybackAttempts: 0,
    transitionFailures: 0,
    muted: false,
  },
};

let captureActive = false;
let lastFrameAt = null;
let frameWindowStartedAt = null;
let frameWindowCount = 0;
let audioContext = null;
let audioContextPendingClose = null;
let masterGain = null;
let audioNeedsExplicitResume = false;
let audioProbeInFlight = false;
let audioLifecycleOperations = 0;
let audioLifecycleTransition = Promise.resolve();

function detectBrowserFamily(userAgent) {
  if (/SamsungBrowser\//u.test(userAgent)) {
    return "samsung-internet";
  }
  if (/(?:Edg|EdgA|EdgiOS)\//u.test(userAgent)) {
    return "edge";
  }
  if (/(?:Firefox|FxiOS)\//u.test(userAgent)) {
    return "firefox";
  }
  if (/(?:Chrome|CriOS)\//u.test(userAgent)) {
    return "chrome";
  }
  if (/Version\/.+Safari\//u.test(userAgent)) {
    return "safari";
  }
  return "unknown";
}

function detectPlatform(userAgent, maxTouchPoints) {
  if (/iPhone|iPod/u.test(userAgent)) {
    return "iphone";
  }
  if (/iPad/u.test(userAgent) || (/Macintosh/u.test(userAgent) && maxTouchPoints > 1)) {
    return "ipad";
  }
  if (/Android/u.test(userAgent)) {
    return /Mobile/u.test(userAgent) ? "android-phone" : "android-tablet";
  }
  return "desktop";
}

function detectBrowserVersion(userAgent, family) {
  const patterns = {
    "samsung-internet": /SamsungBrowser\/(\d+(?:\.\d+)*)/u,
    edge: /(?:Edg|EdgA|EdgiOS)\/(\d+(?:\.\d+)*)/u,
    firefox: /(?:Firefox|FxiOS)\/(\d+(?:\.\d+)*)/u,
    chrome: /(?:Chrome|CriOS)\/(\d+(?:\.\d+)*)/u,
    safari: /Version\/(\d+(?:\.\d+)*)/u,
  };
  return patterns[family]?.exec(userAgent)?.[1] ?? null;
}

function normalizedBrowserVersion(version) {
  const parts = version.trim().split(".");
  if (parts.length === 0 || parts.some((part) => !/^\d+$/u.test(part))) {
    return null;
  }
  const normalizedParts = parts.map((part) => String(Number.parseInt(part, 10)));
  while (normalizedParts.length > 1 && normalizedParts.at(-1) === "0") {
    normalizedParts.pop();
  }
  return normalizedParts.join(".");
}

function allowedBrowserFamilies(platform) {
  return {
    iphone: ["safari"],
    ipad: ["safari"],
    "android-phone": ["chrome", "firefox", "samsung-internet"],
    "android-tablet": ["chrome"],
    desktop: ["safari", "chrome", "firefox", "edge"],
  }[platform] ?? [];
}

function updateBrowserFamilyOptions() {
  const allowed = new Set(allowedBrowserFamilies(platformInput.value));
  for (const option of browserFamilyInput.options) {
    option.hidden = option.value !== "" && !allowed.has(option.value);
    option.disabled = option.hidden;
  }
  if (!allowed.has(browserFamilyInput.value)) {
    browserFamilyInput.value = "";
  }
  if (allowed.has(detectedBrowserFamily)) {
    browserFamilyInput.value = detectedBrowserFamily;
  }
  const isDesktop = platformInput.value === "desktop";
  if (isDesktop && activePresentationTier !== "default") {
    showStatus("Desktop compatibility captures require the default-tier URL.");
  }
}

function formatDuration(milliseconds) {
  if (milliseconds === null) {
    return "pending";
  }
  return `${(milliseconds / 1000).toFixed(2)} s`;
}

function formatBytes(bytes) {
  if (bytes === null) {
    return "unavailable";
  }
  return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
}

function percentile(samples, percentage) {
  if (samples.length === 0) {
    return null;
  }
  const sorted = [...samples].sort((left, right) => left - right);
  const index = Math.ceil((percentage / 100) * sorted.length) - 1;
  return sorted[Math.max(0, index)];
}

function hasRequiredBackgroundIntervals(durations) {
  return durations.filter((duration) => duration >= 30_000).length >= 2;
}

function captureDuration() {
  if (telemetry.captureStartedAt === null) {
    return null;
  }
  const endedAt = telemetry.captureStoppedAt ?? performance.now();
  const currentHiddenDuration = telemetry.hiddenStartedAt === null ? 0 : endedAt - telemetry.hiddenStartedAt;
  return endedAt - telemetry.captureStartedAt - telemetry.hiddenDurationMs - currentHiddenDuration;
}

function captureTargets(cacheState = cacheStateInput.value) {
  return cacheState === "lifecycle"
    ? {
        visibilityChanges: 4,
        orientationChanges: 2,
        pageHides: 2,
        pageShows: 2,
        backgroundIntervals: 2,
        backgroundSuspensions: 2,
        explicitResumes: 2,
      }
    : {
        visibilityChanges: 0,
        orientationChanges: 0,
        pageHides: 0,
        pageShows: 0,
        backgroundIntervals: 0,
        backgroundSuspensions: 0,
        explicitResumes: 0,
      };
}

function refreshMetrics() {
  if (!feasibilityEnabled) {
    return;
  }

  const duration = captureDuration();
  const targets = captureTargets();
  metricsOutput.textContent = [
    `build: ${candidateBuildId}`,
    `tier: ${activePresentationTier}`,
    `client ready: ${formatDuration(telemetry.clientReadyMs)}`,
    `first canvas contact: ${formatDuration(telemetry.firstCanvasContactMs)}`,
    `capture: ${captureActive ? "running" : "stopped"} (${formatDuration(duration)})`,
    `frame rate: ${telemetry.currentFps === null ? "pending" : `${telemetry.currentFps.toFixed(1)} FPS`}`,
    `minimum/median FPS: ${telemetry.minimumFps === null ? "pending" : `${telemetry.minimumFps.toFixed(1)} / ${percentile(telemetry.fpsSamples, 50).toFixed(1)}`}`,
    `p95/worst frame time: ${telemetry.frameGapSamplesMs.length === 0 ? "pending" : `${percentile(telemetry.frameGapSamplesMs, 95).toFixed(1)} / ${telemetry.maximumFrameGapMs.toFixed(1)} ms`}`,
    `JS heap: ${formatBytes(telemetry.currentJsHeapBytes)} (peak ${formatBytes(telemetry.peakJsHeapBytes)})`,
    `canvas contacts/events: ${telemetry.pointerContacts}/${telemetry.events.length}`,
    `visibility/orientation changes: ${telemetry.visibilityChanges}/${telemetry.orientationChanges}`,
    `capture target: ${telemetry.visibilityChanges}/${targets.visibilityChanges} visibility · ${telemetry.orientationChanges}/${targets.orientationChanges} orientation`,
    `page hide/show: ${telemetry.pageHideCount}/${targets.pageHides} · ${telemetry.pageShowCount}/${targets.pageShows}`,
    `background intervals: ${telemetry.hiddenDurationsMs.length}/${targets.backgroundIntervals}`,
    `restored from page cache: ${telemetry.restoredFromPageCache ? "yes" : "no"}`,
    `audio: ${telemetry.audio.state}${telemetry.audio.muted ? " (muted)" : ""}`,
    `audio evidence: ${telemetry.audio.audiblePlaybackAttempts}/1 audible · ${telemetry.audio.mutedPlaybackAttempts}/1 muted · ${telemetry.audio.muteChanges}/2 mute changes · ${telemetry.audio.backgroundSuspensions}/${targets.backgroundSuspensions} suspend · ${telemetry.audio.explicitResumes}/${targets.explicitResumes} resume · ${telemetry.audio.transitionFailures} failures`,
    `viewport: ${window.innerWidth}×${window.innerHeight} @ ${window.devicePixelRatio.toFixed(2)}x`,
  ].join("\n");
}

function sampleMemory() {
  const memory = performance.memory;
  if (!memory || typeof memory.usedJSHeapSize !== "number") {
    return;
  }
  telemetry.currentJsHeapBytes = memory.usedJSHeapSize;
  telemetry.peakJsHeapBytes = Math.max(telemetry.peakJsHeapBytes ?? 0, memory.usedJSHeapSize);
}

function resetFrameWindow() {
  lastFrameAt = null;
  frameWindowStartedAt = null;
  frameWindowCount = 0;
}

function frame(timestamp) {
  if (captureActive) {
    telemetry.frameCount += 1;
    frameWindowCount += 1;

    if (lastFrameAt !== null) {
      const frameGap = timestamp - lastFrameAt;
      telemetry.frameGapSamplesMs.push(frameGap);
      telemetry.maximumFrameGapMs = Math.max(telemetry.maximumFrameGapMs, frameGap);
    }
    lastFrameAt = timestamp;
    frameWindowStartedAt ??= timestamp;

    const windowDuration = timestamp - frameWindowStartedAt;
    if (windowDuration >= 1000) {
      const fps = (frameWindowCount * 1000) / windowDuration;
      telemetry.currentFps = fps;
      telemetry.fpsSamples.push(fps);
      telemetry.minimumFps = Math.min(telemetry.minimumFps ?? fps, fps);
      telemetry.maximumFps = Math.max(telemetry.maximumFps ?? fps, fps);
      frameWindowStartedAt = timestamp;
      frameWindowCount = 0;
      sampleMemory();
      refreshMetrics();
    }
  }

  window.requestAnimationFrame(frame);
}

function externalObservations() {
  const numericValue = (input) => {
    const text = input.value.trim();
    if (text === "") {
      return null;
    }
    const value = Number(text);
    return Number.isFinite(value) ? value : null;
  };
  return {
    first_visible_table_ms: numericValue(firstVisibleInput),
    first_accepted_input_ms: numericValue(firstInputInput),
    steady_memory_mib: numericValue(steadyMemoryInput),
    peak_memory_mib: numericValue(peakMemoryInput),
    thermal_result: thermalResultInput.value || null,
    reload_or_eviction_observed: reloadObservedInput.value || null,
  };
}

function requireExternalObservations() {
  const inputs = [
    firstVisibleInput,
    firstInputInput,
    steadyMemoryInput,
    peakMemoryInput,
    thermalResultInput,
    reloadObservedInput,
  ];
  for (const input of inputs) {
    if (!input.reportValidity()) {
      showStatus(`Complete the required observation: ${input.labels?.[0]?.textContent.trim() ?? input.id}`);
      return false;
    }
  }
  const observations = externalObservations();
  if (Object.values(observations).some((value) => value === null)) {
    showStatus("All external observations must be valid finite values or selected outcomes.");
    return false;
  }
  if (
    telemetry.captureStartedAt === null ||
    telemetry.captureStoppedAt === null ||
    captureActive
  ) {
    showStatus("Stop the capture before validating external observations.");
    captureButton.focus();
    return false;
  }
  if (telemetry.clientReadyMs === null || telemetry.firstCanvasContactMs === null) {
    showStatus("Client readiness and the first canvas contact must be recorded before export.");
    return false;
  }
  const duration = captureDuration();
  if (duration === null || duration <= 0) {
    showStatus("Capture duration must be available before validating external observations.");
    return false;
  }
  if (observations.first_visible_table_ms < telemetry.clientReadyMs) {
    showStatus("First visible table cannot precede client readiness.");
    firstVisibleInput.focus();
    return false;
  }
  if (observations.first_visible_table_ms > telemetry.captureStoppedAt) {
    showStatus("First visible table must occur before capture stop.");
    firstVisibleInput.focus();
    return false;
  }
  if (observations.first_accepted_input_ms < telemetry.firstCanvasContactMs) {
    showStatus("First accepted input cannot precede the first canvas contact.");
    firstInputInput.focus();
    return false;
  }
  if (observations.first_accepted_input_ms > telemetry.captureStoppedAt) {
    showStatus("First accepted input must occur before capture stop.");
    firstInputInput.focus();
    return false;
  }
  if (observations.first_accepted_input_ms < observations.first_visible_table_ms) {
    showStatus("First accepted input cannot precede the first visible table.");
    firstInputInput.focus();
    return false;
  }
  if (observations.steady_memory_mib <= 0 || observations.peak_memory_mib <= 0) {
    showStatus("Steady and peak memory must both be greater than zero.");
    steadyMemoryInput.focus();
    return false;
  }
  if (observations.peak_memory_mib < observations.steady_memory_mib) {
    showStatus("Peak memory cannot be lower than steady memory.");
    peakMemoryInput.focus();
    return false;
  }
  const visibleBudget = cacheStateInput.value === "cold" ? 8_000 : 3_000;
  if (platformInput.value !== "desktop" && cacheStateInput.value !== "lifecycle") {
    if (observations.first_visible_table_ms > visibleBudget) {
      showStatus(`First visible table exceeds the ${visibleBudget / 1000}-second ${cacheStateInput.value} budget.`);
      firstVisibleInput.focus();
      return false;
    }
    if (observations.first_accepted_input_ms > visibleBudget) {
      showStatus(`First accepted input exceeds the ${visibleBudget / 1000}-second ${cacheStateInput.value} budget.`);
      firstInputInput.focus();
      return false;
    }
  }
  if (observations.thermal_result === "warning") {
    showStatus("Thermal warning or severe throttling invalidates this run.");
    thermalResultInput.focus();
    return false;
  }
  if (observations.reload_or_eviction_observed === "yes") {
    showStatus("A reload or eviction invalidates this run.");
    reloadObservedInput.focus();
    return false;
  }
  return true;
}

function physicalCheckResults() {
  return Object.fromEntries(
    PHYSICAL_CHECKS.map(([id]) => [
      id,
      document.querySelector(`input[name="check-${id}"]:checked`)?.value ?? null,
    ]),
  );
}

function showStatus(message) {
  statusOutput.textContent = message;
}

function requirePhysicalChecks() {
  const results = physicalCheckResults();
  const failed = PHYSICAL_CHECKS.find(([id]) => results[id] === "fail");
  if (failed !== undefined) {
    showStatus(`Failed physical check invalidates this run: ${failed[1]}`);
    document.querySelector(`input[name="check-${failed[0]}"]:checked`)?.focus();
    return false;
  }
  const missing = PHYSICAL_CHECKS.find(([id]) => results[id] === null);
  if (missing === undefined) {
    return true;
  }
  showStatus(`Missing physical check: ${missing[1]}`);
  document.querySelector(`input[name="check-${missing[0]}"]`)?.focus();
  return false;
}

function testMetadata() {
  return {
    platform: platformInput.value,
    hardware_model: hardwareModelInput.value.trim(),
    os_version: osVersionInput.value.trim(),
    browser_family: browserFamilyInput.value,
    browser_version: browserVersionInput.value.trim(),
    cache_state: cacheStateInput.value,
    minimum_version_run: minimumVersionInput.value,
    presentation_tier: presentationTierInput.value,
    run_number: Number(runNumberInput.value),
  };
}

function requireTestMetadata() {
  const inputs = [
    platformInput,
    hardwareModelInput,
    osVersionInput,
    browserFamilyInput,
    browserVersionInput,
    cacheStateInput,
    minimumVersionInput,
    presentationTierInput,
    runNumberInput,
  ];
  for (const input of inputs) {
    if (!input.reportValidity()) {
      showStatus(`Complete the required test metadata: ${input.labels?.[0]?.textContent.trim() ?? input.id}`);
      return false;
    }
  }
  for (const input of [hardwareModelInput, osVersionInput, browserVersionInput]) {
    if (input.value !== input.value.trim()) {
      showStatus(`Remove surrounding whitespace from: ${input.labels?.[0]?.textContent.trim() ?? input.id}`);
      input.focus();
      return false;
    }
  }
  const runNumber = Number(runNumberInput.value);
  if (!Number.isInteger(runNumber) || runNumber < 1 || runNumber > 99) {
    showStatus("Run number must be an integer from 1 through 99.");
    runNumberInput.focus();
    return false;
  }
  if (platformInput.value === "desktop" && presentationTierInput.value !== "default") {
    showStatus("Desktop compatibility captures must use the default presentation tier.");
    return false;
  }
  if (platformInput.value !== detectedPlatform) {
    showStatus(`Declared platform ${platformInput.value} does not match detected platform ${detectedPlatform}.`);
    platformInput.focus();
    return false;
  }
  if (detectedBrowserFamily === "unknown") {
    showStatus("This browser could not be identified; use a supported browser before capturing.");
    browserFamilyInput.focus();
    return false;
  }
  if (browserFamilyInput.value !== detectedBrowserFamily) {
    showStatus(
      `Declared browser ${browserFamilyInput.value} does not match detected browser ${detectedBrowserFamily}.`,
    );
    browserFamilyInput.focus();
    return false;
  }
  if (!allowedBrowserFamilies(platformInput.value).includes(browserFamilyInput.value)) {
    showStatus(`Browser ${browserFamilyInput.value} is not supported for ${platformInput.value}.`);
    browserFamilyInput.focus();
    return false;
  }
  const detectedBrowserVersion = detectBrowserVersion(navigator.userAgent, detectedBrowserFamily);
  if (detectedBrowserVersion === null) {
    showStatus("This browser version could not be identified; verify the exact version before capturing.");
    browserVersionInput.focus();
    return false;
  }
  const normalizedDeclaredVersion = normalizedBrowserVersion(browserVersionInput.value);
  const normalizedDetectedVersion = normalizedBrowserVersion(detectedBrowserVersion);
  if (normalizedDeclaredVersion === null || browserVersionInput.value.trim() !== normalizedDeclaredVersion) {
    showStatus("Declared browser version must use canonical dotted numeric spelling.");
    browserVersionInput.focus();
    return false;
  }
  if (normalizedDeclaredVersion !== normalizedDetectedVersion) {
    showStatus(
      `Declared browser version ${browserVersionInput.value} does not match detected ${detectedBrowserVersion}.`,
    );
    browserVersionInput.focus();
    return false;
  }
  if (candidateWasmOptimization !== "wasm-opt-Oz") {
    showStatus("Physical capture requires the wasm-opt-Oz candidate.");
    return false;
  }
  if (
    !/^[A-Za-z0-9._-]+$/u.test(candidateBuildId) ||
    candidateBuildId.length > 128 ||
    !/^[0-9a-f]{64}$/u.test(candidateSourceHash) ||
    !candidateBuildId.includes(candidateSourceHash) ||
    !/^[0-9a-f]{64}$/u.test(candidateBundleHash) ||
    candidateBundleHashAlgorithm !== "sha256-length-prefixed-v1" ||
    candidateBevyVersion !== "0.19.1" ||
    candidateRenderer !== "WebGL2"
  ) {
    showStatus("Generated candidate identity is invalid; rebuild before capturing.");
    return false;
  }
  return true;
}

const captureLockedInputs = [
  platformInput,
  hardwareModelInput,
  osVersionInput,
  browserFamilyInput,
  browserVersionInput,
  cacheStateInput,
  minimumVersionInput,
  runNumberInput,
];

function setCaptureMetadataLocked(locked) {
  for (const input of captureLockedInputs) {
    input.disabled = locked;
  }
}

function startCapture() {
  showStatus("");
  if (telemetry.audio.transitionFailures > 0) {
    showStatus("Reset the report form after the failed audio transition before starting a capture.");
    resetButton.focus();
    return;
  }
  if (audioContext !== null && audioContext.state !== "running") {
    showStatus("Resume audio before starting the capture.");
    audioButton.focus();
    return;
  }
  if (audioContext !== null && telemetry.audio.transitionFailures === 0) {
    showStatus("Reset the report form before reusing an existing audio context for a new capture.");
    resetButton.focus();
    return;
  }
  if (rejectForAudioLifecycle("starting a capture")) {
    return;
  }
  if (document.hidden) {
    showStatus("Return this page to the foreground before starting a capture.");
    captureButton.focus();
    return;
  }
  if (telemetry.clientReadyMs === null || shell.dataset.clientState !== "ready") {
    showStatus("Wait for the client to finish loading before starting a capture.");
    captureButton.focus();
    return;
  }
  if (!requireTestMetadata()) {
    return;
  }
  telemetry.captureStartedAt = performance.now();
  telemetry.captureStoppedAt = null;
  telemetry.hiddenStartedAt = null;
  telemetry.hiddenDurationMs = 0;
  telemetry.hiddenDurationsMs = [];
  telemetry.frameCount = 0;
  telemetry.minimumFps = null;
  telemetry.maximumFps = null;
  telemetry.fpsSamples = [];
  telemetry.frameGapSamplesMs = [];
  telemetry.maximumFrameGapMs = 0;
  telemetry.currentFps = null;
  telemetry.currentJsHeapBytes = null;
  telemetry.peakJsHeapBytes = null;
  telemetry.pointerContacts = 0;
  telemetry.visibilityChanges = 0;
  telemetry.orientationChanges = 0;
  telemetry.orientationStates = [];
  telemetry.initialOrientation = window.screen.orientation?.type ?? null;
  telemetry.finalOrientation = null;
  telemetry.pageHideCount = 0;
  telemetry.pageShowCount = 0;
  telemetry.restoredFromPageCache = false;
  telemetry.audio.gestureStarts = 0;
  telemetry.audio.backgroundSuspensions = 0;
  telemetry.audio.explicitResumes = 0;
  telemetry.audio.muteChanges = 0;
  telemetry.audio.mutedPlaybackAttempts = 0;
  telemetry.audio.audiblePlaybackAttempts = 0;
  telemetry.audio.transitionFailures = 0;
  telemetry.audio.muted = false;
  audioNeedsExplicitResume = false;
  if (masterGain !== null && audioContext !== null) {
    masterGain.gain.setValueAtTime(0.12, audioContext.currentTime);
  }
  telemetry.events = [];
  resetFrameWindow();
  captureActive = true;
  setCaptureMetadataLocked(true);
  tools.classList.add("collapsed");
  toggleToolsButton.textContent = "Expand panel";
  toggleToolsButton.setAttribute("aria-expanded", "false");
  captureButton.textContent = "Stop capture";
  updateCaptureControls();
  showStatus("Capture started.");
  refreshMetrics();
}

function stopCapture() {
  if (rejectForAudioLifecycle("stopping the capture")) {
    return;
  }
  if (document.hidden) {
    showStatus("Return this page to the foreground before stopping the capture.");
    return;
  }
  telemetry.captureStoppedAt = performance.now();
  telemetry.finalOrientation = window.screen.orientation?.type ?? null;
  if (telemetry.hiddenStartedAt !== null) {
    const hiddenDuration = telemetry.captureStoppedAt - telemetry.hiddenStartedAt;
    telemetry.hiddenDurationMs += hiddenDuration;
    telemetry.hiddenDurationsMs.push(hiddenDuration);
    telemetry.hiddenStartedAt = null;
  }
  captureActive = false;
  tools.classList.remove("collapsed");
  toggleToolsButton.textContent = "Minimize panel";
  toggleToolsButton.setAttribute("aria-expanded", "true");
  captureButton.textContent = "Restart capture";
  updateAudioControls();
  showStatus(
    stoppedCaptureNeedsAudioResume()
      ? "Capture stopped. Resume audio before validating this report."
      : "Capture stopped and ready for validation.",
  );
  sampleMemory();
  refreshMetrics();
}

function updateCaptureControls() {
  const busy = audioLifecycleBusy();
  const clientReady = telemetry.clientReadyMs !== null && shell.dataset.clientState === "ready";
  captureButton.disabled = busy || (!captureActive && !clientReady);
  markEventButton.disabled = !captureActive || busy;
  resetButton.disabled = captureActive || busy;
  downloadButton.disabled =
    captureActive ||
    busy ||
    telemetry.captureStoppedAt === null ||
    telemetry.audio.transitionFailures > 0 ||
    stoppedCaptureNeedsAudioResume();
  toggleToolsButton.disabled = captureActive || busy;
}

function stoppedCaptureNeedsAudioResume() {
  return (
    !captureActive &&
    telemetry.captureStoppedAt !== null &&
    audioContext !== null &&
    (audioNeedsExplicitResume || audioContext.state !== "running")
  );
}

function updateAudioControls() {
  telemetry.audio.state =
    audioContext?.state ??
    audioContextPendingClose?.state ??
    (telemetry.audio.supported ? "not started" : "unsupported");
  audioButton.textContent = stoppedCaptureNeedsAudioResume()
    ? "Resume audio for export"
    : audioNeedsExplicitResume
      ? "Resume audio probe"
      : "Play audio probe";
  audioButton.disabled =
    !telemetry.audio.supported ||
    audioProbeInFlight ||
    audioLifecycleOperations > 0 ||
    (!captureActive && telemetry.audio.transitionFailures === 0 && !stoppedCaptureNeedsAudioResume()) ||
    (captureActive && telemetry.audio.gestureStarts >= 2);
  muteButton.disabled =
    audioContext === null || audioProbeInFlight || audioLifecycleOperations > 0 || !captureActive;
  muteButton.textContent = telemetry.audio.muted ? "Unmute" : "Mute";
  updateCaptureControls();
  refreshMetrics();
}

function createAudioGraph() {
  const AudioContext = window.AudioContext || window.webkitAudioContext;
  audioContext = new AudioContext();
  masterGain = audioContext.createGain();
  masterGain.gain.value = 0.12;
  masterGain.connect(audioContext.destination);
  audioContext.addEventListener("statechange", updateAudioControls);
}

function queueAudioLifecycleTransition(transition) {
  audioLifecycleOperations += 1;
  updateAudioControls();
  const queued = audioLifecycleTransition.then(transition, transition);
  audioLifecycleTransition = queued.catch(() => {});
  void queued
    .finally(() => {
      audioLifecycleOperations -= 1;
      updateAudioControls();
    })
    .catch(() => {});
  return queued;
}

function audioLifecycleBusy() {
  return audioProbeInFlight || audioLifecycleOperations > 0;
}

function rejectForAudioLifecycle(action) {
  if (!audioLifecycleBusy()) {
    return false;
  }
  showStatus(`Wait for audio operations to finish before ${action}.`);
  audioButton.focus();
  return true;
}

function requestBackgroundAudioSuspension() {
  if (!document.hidden || audioContext === null || audioNeedsExplicitResume) {
    return;
  }

  const contextToSuspend = audioContext;
  audioNeedsExplicitResume = true;
  void queueAudioLifecycleTransition(async () => {
    if (!document.hidden || audioContext !== contextToSuspend) {
      return false;
    }
    if (contextToSuspend.state === "running") {
      await contextToSuspend.suspend();
    }
    if (contextToSuspend.state !== "suspended" && contextToSuspend.state !== "interrupted") {
      throw new Error(`audio context did not suspend: ${contextToSuspend.state}`);
    }
    return true;
  })
    .then((suspended) => {
      if (!suspended) {
        if (audioContext === contextToSuspend && contextToSuspend.state === "running") {
          audioNeedsExplicitResume = false;
        }
        return;
      }
      if (captureActive) {
        telemetry.audio.backgroundSuspensions += 1;
      }
    })
    .catch((error) => {
      telemetry.audio.transitionFailures += 1;
      showStatus(`Audio suspend failed: ${error instanceof Error ? error.message : String(error)}`);
    })
    .finally(updateAudioControls);
}

async function playAudioProbe() {
  if (!telemetry.audio.supported || audioProbeInFlight) {
    return;
  }

  if (!captureActive && telemetry.audio.transitionFailures === 0 && !stoppedCaptureNeedsAudioResume()) {
    showStatus("Start a capture before playing the audio probe.");
    captureButton.focus();
    return;
  }
  if (captureActive && telemetry.audio.gestureStarts >= 2) {
    showStatus("This capture already contains the required two audio probe plays.");
    return;
  }

  const resumeForStoppedCapture = stoppedCaptureNeedsAudioResume();
  audioProbeInFlight = true;
  audioButton.disabled = true;
  try {
    if (audioContext === null) {
      createAudioGraph();
    }

    if (
      audioContext.state === "suspended" ||
      audioContext.state === "interrupted" ||
      (audioNeedsExplicitResume && audioContext.state !== "running")
    ) {
      await queueAudioLifecycleTransition(async () => {
        if (document.hidden) {
          throw new Error("return this page to the foreground before resuming audio");
        }
        if (audioContext.state !== "running") {
          await audioContext.resume();
        }
      });
      if (audioContext.state !== "running") {
        throw new Error(`audio context did not resume: ${audioContext.state}`);
      }
      if (audioNeedsExplicitResume && captureActive) {
        telemetry.audio.explicitResumes += 1;
      }
    }

    audioNeedsExplicitResume = false;
    if (resumeForStoppedCapture) {
      showStatus("Audio resumed. The stopped report is ready for validation.");
      return;
    }
    if (captureActive) {
      telemetry.audio.gestureStarts += 1;
      if (telemetry.audio.muted) {
        telemetry.audio.mutedPlaybackAttempts += 1;
      } else {
        telemetry.audio.audiblePlaybackAttempts += 1;
      }
    }

    const oscillator = audioContext.createOscillator();
    const envelope = audioContext.createGain();
    const startedAt = audioContext.currentTime;
    oscillator.type = "triangle";
    oscillator.frequency.setValueAtTime(523.25, startedAt);
    oscillator.frequency.exponentialRampToValueAtTime(659.25, startedAt + 0.16);
    envelope.gain.setValueAtTime(0.0001, startedAt);
    envelope.gain.exponentialRampToValueAtTime(1, startedAt + 0.015);
    envelope.gain.exponentialRampToValueAtTime(0.0001, startedAt + 0.24);
    oscillator.connect(envelope);
    envelope.connect(masterGain);
    oscillator.start(startedAt);
    oscillator.stop(startedAt + 0.25);
    oscillator.addEventListener("ended", () => {
      oscillator.disconnect();
      envelope.disconnect();
    });
    showStatus(telemetry.audio.muted ? "Muted audio probe played." : "Audio probe played.");
  } catch (error) {
    audioNeedsExplicitResume = true;
    telemetry.audio.transitionFailures += 1;
    showStatus(`Audio probe failed: ${error instanceof Error ? error.message : String(error)}`);
  } finally {
    audioProbeInFlight = false;
    updateAudioControls();
  }
}

function toggleMute() {
  if (masterGain === null || audioContext === null || !captureActive || audioLifecycleBusy()) {
    showStatus("Mute changes are available only during an active capture with idle audio.");
    return;
  }

  telemetry.audio.muted = !telemetry.audio.muted;
  if (captureActive) {
    telemetry.audio.muteChanges += 1;
  }
  masterGain.gain.setValueAtTime(telemetry.audio.muted ? 0 : 0.12, audioContext.currentTime);
  updateAudioControls();
}

function appendVisibleCaptureEvent(label) {
  const elapsedMs = captureDuration();
  const orientation = window.screen.orientation?.type ?? null;
  if (
    !captureActive ||
    elapsedMs === null ||
    elapsedMs < 0 ||
    document.visibilityState !== "visible" ||
    orientation === null
  ) {
    return false;
  }
  telemetry.events.push({
    elapsed_ms: elapsedMs,
    label: label.slice(0, 120),
    visibility: "visible",
    orientation,
  });
  return true;
}

function markEvent() {
  if (!captureActive || audioLifecycleBusy()) {
    showStatus(
      captureActive
        ? "Wait for audio operations before marking an event."
        : "Start a capture before marking an event.",
    );
    (captureActive ? audioButton : captureButton).focus();
    return;
  }
  const label = eventLabelInput.value.trim();
  if (label === "") {
    showStatus("Enter a short event label before marking the event.");
    eventLabelInput.focus();
    return;
  }
  if ([...label].some((character) => character.codePointAt(0) < 32 || character.codePointAt(0) === 127)) {
    showStatus("Event labels cannot contain control characters.");
    eventLabelInput.focus();
    return;
  }
  if (!appendVisibleCaptureEvent(label)) {
    showStatus("Event markers require visible capture timing and a known orientation.");
    return;
  }
  eventLabelInput.value = "";
  showStatus(`Marked event: ${label.slice(0, 120)}`);
  refreshMetrics();
}

function report(test = testMetadata()) {
  sampleMemory();
  return {
    schema_version: telemetry.schemaVersion,
    captured_at: new Date().toISOString(),
    candidate: {
      build_id: candidateBuildId,
      source_hash: candidateSourceHash,
      bundle_hash: candidateBundleHash,
      bundle_hash_algorithm: candidateBundleHashAlgorithm,
      wasm_optimization: candidateWasmOptimization,
      bevy: candidateBevyVersion,
      renderer: candidateRenderer,
    },
    test,
    physical_checks: physicalCheckResults(),
    external_observations: externalObservations(),
    browser: {
      declared_family: browserFamilyInput.value,
      detected_family: detectedBrowserFamily,
      detected_platform: detectedPlatform,
      declared_version: browserVersionInput.value.trim(),
      detected_version: detectBrowserVersion(navigator.userAgent, detectedBrowserFamily),
      user_agent: navigator.userAgent,
      language: navigator.language,
      hardware_concurrency: navigator.hardwareConcurrency ?? null,
      device_memory_gib: navigator.deviceMemory ?? null,
    },
    display: {
      screen_width: window.screen.width,
      screen_height: window.screen.height,
      viewport_width: window.innerWidth,
      viewport_height: window.innerHeight,
      device_pixel_ratio: window.devicePixelRatio,
      orientation: window.screen.orientation?.type ?? null,
    },
    timing_ms: {
      client_ready: telemetry.clientReadyMs,
      first_canvas_contact: telemetry.firstCanvasContactMs,
      capture_duration: captureDuration(),
      capture_started_at: telemetry.captureStartedAt,
      capture_stopped_at: telemetry.captureStoppedAt,
      hidden_duration: telemetry.hiddenDurationMs,
      hidden_durations: telemetry.hiddenDurationsMs,
    },
    performance: {
      frame_count: telemetry.frameCount,
      current_fps: telemetry.currentFps,
      minimum_one_second_fps: telemetry.minimumFps,
      median_one_second_fps: percentile(telemetry.fpsSamples, 50),
      maximum_one_second_fps: telemetry.maximumFps,
      median_frame_time_ms: percentile(telemetry.frameGapSamplesMs, 50),
      p95_frame_time_ms: percentile(telemetry.frameGapSamplesMs, 95),
      p99_frame_time_ms: percentile(telemetry.frameGapSamplesMs, 99),
      maximum_frame_gap_ms: telemetry.maximumFrameGapMs,
      current_js_heap_bytes: telemetry.currentJsHeapBytes,
      peak_js_heap_bytes: telemetry.peakJsHeapBytes,
    },
    interaction: {
      canvas_contacts: telemetry.pointerContacts,
      visibility_changes: telemetry.visibilityChanges,
      orientation_changes: telemetry.orientationChanges,
      initial_orientation: telemetry.initialOrientation,
      final_orientation: telemetry.finalOrientation,
      orientation_states: telemetry.orientationStates,
      page_hide_count: telemetry.pageHideCount,
      page_show_count: telemetry.pageShowCount,
      restored_from_page_cache: telemetry.restoredFromPageCache,
      capture_started_visible: true,
      capture_stopped_visible: !document.hidden,
      marked_events: telemetry.events,
    },
    audio: telemetry.audio,
  };
}

function requireStoppedCapture() {
  if (telemetry.captureStartedAt !== null && telemetry.captureStoppedAt !== null && !captureActive) {
    return true;
  }
  showStatus("Start and stop the capture before downloading its report.");
  captureButton.focus();
  return false;
}

function requireCaptureEvidence() {
  if (rejectForAudioLifecycle("downloading the report")) {
    return false;
  }
  const duration = captureDuration();
  if (
    telemetry.frameCount <= 0 ||
    telemetry.fpsSamples.length === 0 ||
    telemetry.frameGapSamplesMs.length === 0 ||
    telemetry.minimumFps === null ||
    telemetry.maximumFps === null ||
    !Number.isFinite(telemetry.minimumFps) ||
    !Number.isFinite(telemetry.maximumFps) ||
    telemetry.minimumFps < 0 ||
    telemetry.maximumFps < telemetry.minimumFps ||
    telemetry.frameGapSamplesMs.some((frameGap) => !Number.isFinite(frameGap) || frameGap < 0) ||
    telemetry.fpsSamples.some((fps) => !Number.isFinite(fps) || fps < 0)
  ) {
    showStatus("Capture must include internally consistent finite frame-rate samples before export.");
    captureButton.focus();
    return false;
  }
  if (telemetry.pointerContacts <= 0) {
    showStatus("Capture must include at least one canvas contact before export.");
    canvas.focus({ preventScroll: true });
    return false;
  }
  if (platformInput.value !== "desktop") {
    if (telemetry.minimumFps < 30) {
      showStatus("Minimum frame rate is below the 30 FPS mobile floor; this run fails.");
      return false;
    }
    if (telemetry.minimumFps < 55 && presentationTierInput.value !== "reduced") {
      showStatus("Minimum frame rate is below 55 FPS; repeat this run with the reduced presentation tier.");
      return false;
    }
  }
  if (duration === null || duration <= 0) {
    showStatus("Capture duration must be greater than zero.");
    return false;
  }
  if (duration > 30 * 60 * 1000) {
    showStatus("Capture duration cannot exceed 30 active foreground minutes; restart this run.");
    return false;
  }
  if (telemetry.hiddenDurationMs > 30 * 60 * 1000) {
    showStatus("Total hidden duration cannot exceed 30 minutes; restart this run.");
    return false;
  }
  if (telemetry.hiddenDurationsMs.some((hiddenDuration) => hiddenDuration > 30 * 60 * 1000)) {
    showStatus("A hidden interval cannot exceed 30 minutes; restart this run.");
    return false;
  }
  if (Math.abs(telemetry.hiddenDurationsMs.reduce((total, value) => total + value, 0) - telemetry.hiddenDurationMs) > 1) {
    showStatus("Hidden interval timing is inconsistent; restart this run.");
    return false;
  }
  const elapsedCaptureTime = telemetry.captureStoppedAt - telemetry.captureStartedAt;
  if (Math.abs(elapsedCaptureTime - telemetry.hiddenDurationMs - duration) > 1) {
    showStatus("Capture timestamps and durations are inconsistent; restart this run.");
    return false;
  }
  const minimumDuration = cacheStateInput.value === "lifecycle" ? 10 * 60 * 1000 : 60 * 1000;
  if (duration < minimumDuration) {
    showStatus(
      cacheStateInput.value === "lifecycle"
        ? "Lifecycle capture requires at least 10 active foreground minutes."
        : "Interaction capture requires at least one active foreground minute.",
    );
    return false;
  }
  if (cacheStateInput.value !== "lifecycle" && telemetry.audio.backgroundSuspensions !== 0) {
    showStatus("Interaction captures require exactly zero audio background suspensions; restart this run.");
    return false;
  }
  if (cacheStateInput.value !== "lifecycle" && telemetry.audio.explicitResumes !== 0) {
    showStatus("Interaction captures require exactly zero audio foreground resumes; restart this run.");
    return false;
  }
  if (
    cacheStateInput.value !== "lifecycle" &&
    (telemetry.restoredFromPageCache ||
      telemetry.visibilityChanges > 0 ||
      telemetry.pageHideCount > 0 ||
      telemetry.pageShowCount > 0)
  ) {
    showStatus("Interaction captures cannot include page lifecycle transitions; restart this run.");
    return false;
  }
  if (cacheStateInput.value !== "lifecycle" && telemetry.orientationChanges > 0) {
    showStatus("Interaction captures cannot include orientation changes; restart this run.");
    return false;
  }
  if (cacheStateInput.value !== "lifecycle" && telemetry.hiddenDurationsMs.length > 0) {
    showStatus("Interaction captures cannot include background intervals; restart this run.");
    return false;
  }
  if (cacheStateInput.value === "lifecycle") {
    if (
      telemetry.visibilityChanges !== 4 ||
      telemetry.orientationChanges !== 2 ||
      telemetry.pageHideCount !== 2 ||
      telemetry.pageShowCount !== 2 ||
      !hasRequiredBackgroundIntervals(telemetry.hiddenDurationsMs)
    ) {
      showStatus(
        "Lifecycle capture requires two background/foreground cycles of at least 30 seconds each, two page hide/show cycles, and two orientation changes.",
      );
      return false;
    }
    if (
      typeof telemetry.initialOrientation !== "string" ||
      !telemetry.initialOrientation.startsWith("landscape") ||
      typeof telemetry.finalOrientation !== "string" ||
      !telemetry.finalOrientation.startsWith("landscape") ||
      telemetry.orientationStates.length !== 2 ||
      !telemetry.orientationStates[0]?.startsWith("portrait") ||
      !telemetry.orientationStates[1]?.startsWith("landscape")
    ) {
      showStatus("Lifecycle capture requires an exact landscape → portrait → landscape sequence.");
      return false;
    }
    if (telemetry.audio.backgroundSuspensions !== 2 || telemetry.audio.explicitResumes !== 2) {
      showStatus("Lifecycle capture requires exactly two audio suspensions and two explicit resumes.");
      return false;
    }
    if (
      telemetry.audio.backgroundSuspensions !== telemetry.hiddenDurationsMs.length ||
      telemetry.audio.explicitResumes !== telemetry.audio.backgroundSuspensions
    ) {
      showStatus("Audio lifecycle counters do not match every recorded background interval.");
      return false;
    }
  }
  if (telemetry.audio.transitionFailures > 0) {
    showStatus("Capture contains a failed audio transition; restart this run.");
    return false;
  }
  if (telemetry.audio.state !== "running") {
    showStatus("Resume audio and confirm it is running before downloading this report.");
    audioButton.focus();
    return false;
  }
  if (
    telemetry.audio.audiblePlaybackAttempts !== 1 ||
    telemetry.audio.mutedPlaybackAttempts !== 1 ||
    telemetry.audio.gestureStarts !== 2 ||
    telemetry.audio.muteChanges !== 2 ||
    telemetry.audio.muted
  ) {
    showStatus("Capture requires exactly one audible and one muted probe plus one completed mute/unmute cycle.");
    return false;
  }
  return true;
}

function clearCaptureEvidence() {
  captureActive = false;
  telemetry.captureStartedAt = null;
  telemetry.captureStoppedAt = null;
  telemetry.hiddenStartedAt = null;
  telemetry.hiddenDurationMs = 0;
  telemetry.hiddenDurationsMs = [];
  telemetry.frameCount = 0;
  telemetry.minimumFps = null;
  telemetry.maximumFps = null;
  telemetry.fpsSamples = [];
  telemetry.frameGapSamplesMs = [];
  telemetry.maximumFrameGapMs = 0;
  telemetry.currentFps = null;
  telemetry.currentJsHeapBytes = null;
  telemetry.peakJsHeapBytes = null;
  telemetry.pointerContacts = 0;
  telemetry.visibilityChanges = 0;
  telemetry.orientationChanges = 0;
  telemetry.orientationStates = [];
  telemetry.initialOrientation = null;
  telemetry.finalOrientation = null;
  telemetry.pageHideCount = 0;
  telemetry.pageShowCount = 0;
  telemetry.restoredFromPageCache = false;
  telemetry.events = [];
  telemetry.audio.gestureStarts = 0;
  telemetry.audio.backgroundSuspensions = 0;
  telemetry.audio.explicitResumes = 0;
  telemetry.audio.muteChanges = 0;
  telemetry.audio.mutedPlaybackAttempts = 0;
  telemetry.audio.audiblePlaybackAttempts = 0;
  telemetry.audio.transitionFailures = 0;
  telemetry.audio.muted = false;
  captureButton.textContent = "Start capture";
  updateCaptureControls();
}

function feasibilityFilename(test, capturedAt) {
  const platform = test.platform || "unknown-device";
  const browser = test.browser_family || "unknown-browser";
  const browserVersion = (test.browser_version || "unknown-version").replaceAll(".", "_");
  const build = candidateBuildId.slice(0, 20);
  const bundle = candidateBundleHash.slice(0, 20);
  const run = test.run_number === null ? "unknown-run" : `run-${test.run_number}`;
  return `pwmtf-feasibility-${build}-${bundle}-${platform}-${browser}-${browserVersion}-${test.cache_state || "unknown-cache"}-${test.minimum_version_run === "yes" ? "minimum" : "current"}-${test.presentation_tier || "unknown-tier"}-${run}-${capturedAt.replaceAll(":", "-")}.json`;
}

function downloadReport() {
  showStatus("");
  if (
    !requireTestMetadata() ||
    !requirePhysicalChecks() ||
    !requireStoppedCapture() ||
    !requireExternalObservations() ||
    !requireCaptureEvidence()
  ) {
    return;
  }
  const test = testMetadata();
  const capturedAt = new Date().toISOString();
  let contents;
  try {
    contents = JSON.stringify({ ...report(test), captured_at: capturedAt }, null, 2);
  } catch (error) {
    showStatus(`Report serialization failed: ${error instanceof Error ? error.message : String(error)}`);
    return;
  }
  let url;
  try {
    url = URL.createObjectURL(new Blob([contents], { type: "application/json" }));
  } catch (error) {
    showStatus(`Report preparation failed: ${error instanceof Error ? error.message : String(error)}`);
    return;
  }
  let link;
  try {
    link = document.createElement("a");
    link.href = url;
    link.download = feasibilityFilename(test, capturedAt);
    link.click();
  } catch (error) {
    showStatus(`Report download failed: ${error instanceof Error ? error.message : String(error)}`);
    return;
  } finally {
    try {
      URL.revokeObjectURL(url);
    } catch (error) {
      console.error("PWMTF report URL cleanup failed", error);
    }
  }
  showStatus(`Downloaded ${link.download}`);
  setCaptureMetadataLocked(false);
  clearCaptureEvidence();
  refreshMetrics();
}

function resetReportForm() {
  if (captureActive) {
    showStatus("Stop the active capture before resetting the form.");
    captureButton.focus();
    return;
  }
  if (rejectForAudioLifecycle("resetting the report form")) {
    return;
  }
  setCaptureMetadataLocked(false);
  clearCaptureEvidence();
  for (const input of [
    platformInput,
    hardwareModelInput,
    osVersionInput,
    browserFamilyInput,
    browserVersionInput,
    cacheStateInput,
    minimumVersionInput,
    runNumberInput,
    firstVisibleInput,
    firstInputInput,
    steadyMemoryInput,
    peakMemoryInput,
    thermalResultInput,
    reloadObservedInput,
    eventLabelInput,
  ]) {
    input.value = "";
  }
  for (const input of physicalChecks.querySelectorAll('input[type="radio"]')) {
    input.checked = false;
  }
  telemetry.audio.transitionFailures = 0;
  audioNeedsExplicitResume = false;
  const contextToClose = audioContext;
  audioContext = null;
  audioContextPendingClose = contextToClose;
  masterGain = null;
  if (contextToClose !== null) {
    audioLifecycleOperations += 1;
    updateAudioControls();
    void contextToClose
      .close()
      .then(() => {
        if (contextToClose.state !== "closed") {
          throw new Error(`audio context did not close: ${contextToClose.state}`);
        }
      })
      .catch((error) => {
        telemetry.audio.transitionFailures += 1;
        showStatus(`Audio close failed: ${error instanceof Error ? error.message : String(error)}`);
      })
      .finally(() => {
        if (audioContextPendingClose === contextToClose) {
          audioContextPendingClose = null;
        }
        audioLifecycleOperations -= 1;
        updateAudioControls();
      });
  } else {
    updateAudioControls();
  }
  showStatus("Report form reset.");
  platformInput.focus();
}

function toggleTools() {
  if (captureActive || audioLifecycleBusy()) {
    showStatus("The capture panel stays minimized while capture or audio lifecycle work is active.");
    return;
  }
  const collapsed = tools.classList.toggle("collapsed");
  toggleToolsButton.textContent = collapsed ? "Expand panel" : "Minimize panel";
  toggleToolsButton.setAttribute("aria-expanded", String(!collapsed));
}

/**
 * Starts the capture tooling.
 *
 * `host.identity` carries the candidate identity injected into bootstrap.js
 * and `host.navigationStartedAt` anchors timings. Returns hooks bootstrap.js
 * calls as the client lifecycle advances, plus the audio port gameplay cues
 * share so the page keeps a single AudioContext.
 */
export function startFeasibility(host) {
  ({
    buildId: candidateBuildId,
    sourceHash: candidateSourceHash,
    bundleHash: candidateBundleHash,
    bundleHashAlgorithm: candidateBundleHashAlgorithm,
    wasmOptimization: candidateWasmOptimization,
    bevyVersion: candidateBevyVersion,
    renderer: candidateRenderer,
  } = host.identity);
  activePresentationTier = host.presentationTier;
  navigationStartedAt = host.navigationStartedAt;
  feasibilityEnabled = true;

  platformInput.addEventListener("change", updateBrowserFamilyOptions);
  cacheStateInput.addEventListener("change", refreshMetrics);
  captureButton.addEventListener("click", () => (captureActive ? stopCapture() : startCapture()));
  audioButton.addEventListener("click", () => void playAudioProbe());
  muteButton.addEventListener("click", toggleMute);
  markEventButton.addEventListener("click", markEvent);
  downloadButton.addEventListener("click", downloadReport);
  resetButton.addEventListener("click", resetReportForm);
  toggleToolsButton.addEventListener("click", toggleTools);
  canvas.addEventListener("pointerdown", () => {
    if (captureActive) {
      telemetry.pointerContacts += 1;
    }
    telemetry.firstCanvasContactMs ??= performance.now() - navigationStartedAt;
    refreshMetrics();
  });
  window.addEventListener("orientationchange", () => {
    if (captureActive) {
      telemetry.orientationChanges += 1;
      telemetry.orientationStates.push(window.screen.orientation?.type ?? null);
    }
    refreshMetrics();
  });
  window.addEventListener("pagehide", () => {
    if (captureActive) {
      telemetry.pageHideCount += 1;
    }
    refreshMetrics();
  });
  window.addEventListener("pageshow", (event) => {
    if (captureActive) {
      telemetry.pageShowCount += 1;
    }
    if (captureActive && event.persisted) {
      telemetry.restoredFromPageCache = true;
      appendVisibleCaptureEvent("restored from page cache");
    }
    refreshMetrics();
  });
  window.addEventListener("resize", refreshMetrics);
  document.addEventListener("visibilitychange", () => {
    if (captureActive) {
      telemetry.visibilityChanges += 1;
    }
    resetFrameWindow();
    if (captureActive) {
      const now = performance.now();
      if (document.hidden && telemetry.hiddenStartedAt === null) {
        telemetry.hiddenStartedAt = now;
      } else if (!document.hidden && telemetry.hiddenStartedAt !== null) {
        const hiddenDuration = now - telemetry.hiddenStartedAt;
        telemetry.hiddenDurationMs += hiddenDuration;
        telemetry.hiddenDurationsMs.push(hiddenDuration);
        telemetry.hiddenStartedAt = null;
      }
    }
    if (document.hidden) {
      requestBackgroundAudioSuspension();
    }
    refreshMetrics();
  });

  candidateOutput.textContent = `Build ${candidateBuildId}\nSource ${candidateSourceHash}\nBundle ${candidateBundleHash}\nBundle algorithm ${candidateBundleHashAlgorithm}\nWASM ${candidateWasmOptimization}\nDetected browser ${detectedBrowserFamily}`;
  presentationTierInput.value = activePresentationTier;
  presentationTierInput.disabled = true;
  updateBrowserFamilyOptions();
  for (const [id, label] of PHYSICAL_CHECKS) {
    const row = document.createElement("div");
    row.className = "physical-check";
    const description = document.createElement("span");
    description.textContent = label;
    row.append(description);
    for (const value of ["pass", "fail"]) {
      const option = document.createElement("label");
      const input = document.createElement("input");
      input.type = "radio";
      input.name = `check-${id}`;
      input.value = value;
      option.append(input, value === "pass" ? "Pass" : "Fail");
      row.append(option);
    }
    physicalChecks.append(row);
  }
  tools.hidden = false;
  window.requestAnimationFrame(frame);
  updateAudioControls();
  refreshMetrics();

  return {
    /** Records client readiness once the table is visible and interactive. */
    markClientReady() {
      telemetry.clientReadyMs = performance.now() - navigationStartedAt;
      updateCaptureControls();
      refreshMetrics();
    },
    /** Re-renders metrics after a bootstrap-side lifecycle change. */
    refresh: refreshMetrics,
    /** Gameplay cues play through the capture's audio graph. */
    audio: {
      get context() {
        return audioContext;
      },
      get masterGain() {
        return masterGain;
      },
      ensureGraph() {
        if (audioContext === null) {
          createAudioGraph();
        }
      },
      markResumed() {
        audioNeedsExplicitResume = false;
      },
      // Captures count explicit resumes; table contact never resumes here.
      resumeOnContact: false,
    },
  };
}
