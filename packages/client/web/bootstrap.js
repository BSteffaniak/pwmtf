const shell = document.querySelector("#game-shell");
const canvas = document.querySelector("#pwmtf-canvas");
const loading = document.querySelector("#loading");
const loadError = document.querySelector("#load-error");
const accountPanel = document.querySelector("#account-panel");
const accountLabel = document.querySelector("#account-label");
const googleSignIn = document.querySelector("#google-sign-in");
const developmentSignIn = document.querySelector("#development-sign-in");
const developmentUsername = document.querySelector("#development-username");
const signOut = document.querySelector("#sign-out");
const socialPanel = document.querySelector("#social-panel");
socialPanel.open = new URLSearchParams(window.location.search).has("match");
const handleForm = document.querySelector("#handle-form");
const handleInput = document.querySelector("#handle-input");
const challengeForm = document.querySelector("#challenge-form");
const challengeHandle = document.querySelector("#challenge-handle");
const challengeList = document.querySelector("#challenge-list");
const rematchList = document.querySelector("#rematch-list");
const lobbyList = document.querySelector("#lobby-list");
const offerRematchButton = document.querySelector("#offer-rematch");
const concedeMatchButton = document.querySelector("#concede-match");
const matchStatus = document.querySelector("#match-status");
const matchResult = document.querySelector("#match-result");
const matchResultTitle = document.querySelector("#match-result-title");
const matchResultDetail = document.querySelector("#match-result-detail");
const turnTimer = document.querySelector("#turn-timer");
const createInvitationButton = document.querySelector("#create-invitation");
const lobbyPanel = document.querySelector("#lobby-panel");
const lobbyLabel = document.querySelector("#lobby-label");
const lobbyState = document.querySelector("#lobby-state");
const readyLobbyButton = document.querySelector("#ready-lobby");
const cancelLobbyButton = document.querySelector("#cancel-lobby");
const socialStatus = document.querySelector("#social-status");
const reload = loadError.querySelector("button");
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
const query = new URLSearchParams(window.location.search);
const feasibilityEnabled = query.has("feasibility");
const activePresentationTier = query.get("tier") === "reduced" ? "reduced" : "default";
const candidateBuildId = "__PWMTF_BUILD_ID__";
const candidateSourceHash = "__PWMTF_SOURCE_HASH__";
const candidateBundleHash = "__PWMTF_BUNDLE_HASH__";
const candidateBundleHashAlgorithm = "sha256-length-prefixed-v1";
const candidateWasmOptimization = "__PWMTF_WASM_OPTIMIZATION__";
const candidateBevyVersion = "0.19.1";
const candidateRenderer = "WebGL2";
const navigationStartedAt = performance.now();
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
let gameplayAudioEnabled = false;
let gameplayAudioLastChecksum = null;
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

function playTone(frequency, duration, volume = 0.45) {
  if (!gameplayAudioEnabled || audioContext === null || audioContext.state !== "running") {
    return;
  }
  const oscillator = audioContext.createOscillator();
  const envelope = audioContext.createGain();
  const startedAt = audioContext.currentTime;
  oscillator.type = "triangle";
  oscillator.frequency.setValueAtTime(frequency, startedAt);
  envelope.gain.setValueAtTime(0.0001, startedAt);
  envelope.gain.exponentialRampToValueAtTime(volume, startedAt + 0.01);
  envelope.gain.exponentialRampToValueAtTime(0.0001, startedAt + duration);
  oscillator.connect(envelope);
  envelope.connect(masterGain);
  oscillator.start(startedAt);
  oscillator.stop(startedAt + duration);
  oscillator.addEventListener("ended", () => {
    oscillator.disconnect();
    envelope.disconnect();
  });
}

async function enableGameplayAudio() {
  if (!telemetry.audio.supported || gameplayAudioEnabled) {
    return;
  }
  if (audioContext === null) {
    createAudioGraph();
  }
  if (audioContext.state !== "running") {
    await audioContext.resume();
  }
  if (audioContext.state === "running") {
    gameplayAudioEnabled = true;
    audioNeedsExplicitResume = false;
    playTone(440, 0.08, 0.25);
  }
}

function updateGameplayAudio(module) {
  if (!gameplayAudioEnabled || !module.match_socket_ready()) {
    return;
  }
  const checksum = module.match_checksum();
  if (checksum === undefined || checksum === gameplayAudioLastChecksum) {
    return;
  }
  if (gameplayAudioLastChecksum !== null) {
    playTone(196, 0.12);
    window.setTimeout(() => playTone(294, 0.09, 0.3), 70);
  }
  gameplayAudioLastChecksum = checksum;
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

reload.addEventListener("click", () => window.location.reload());
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
  void enableGameplayAudio().catch((error) => {
    console.error("PWMTF gameplay audio failed", error);
  });
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
    stopLobbyPolling();
  } else if (activeLobbyId !== null && lobbyPollTimer === null) {
    void pollLobby();
  }
  refreshMetrics();
});

if (feasibilityEnabled) {
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
}

let matchSocketActive = false;
let matchReconnectTimer = null;
let matchSubscriptionUrl = null;
let matchConnectStartedAt = null;
let matchPlayerSeat = null;
let matchDeadlineAtMs = null;
let matchDeadlineRevision = null;
let matchServerTimeOffsetMs = 0;
let matchServerTimeRequestStartedAt = 0;
let matchServerTimeUncertaintyMs = 0;
let matchAccessRevision = null;
let matchAccessRefreshInFlight = false;
let matchAccessRefreshPendingRevision = null;
let activeLobbyId = null;
let activeLobbyConnectionId = null;
let lobbyPollTimer = null;
let socialPollTimer = null;
let socialRefreshInFlight = false;
let wasmModule = null;
const aimMode = document.querySelector("#aim-mode");
const shotControlMode = document.querySelector("#shot-control-mode");
const shotControlInstructions = document.querySelector("#shot-control-instructions");
const gameHelp = document.querySelector("#game-help");
const combinedHelp = "Drag back anywhere on the table to aim and power · Release to shoot";
const separateHelp = "Touch a direction on the table to aim · Drag POWER down and release to shoot";
function applyShotControlMode() {
  const separate = shotControlMode.value === "1";
  wasmModule?.set_shot_control_mode(separate ? 1 : 0);
  gameHelp.textContent = separate ? separateHelp : combinedHelp;
  shotControlInstructions.textContent = separate
    ? "Touch anywhere on the table to aim from the cue ball toward that point; drag to refine direction without shooting. Drag the POWER bar down to adjust strength; release a charged pull to shoot. The amber line shows power, not the ball's predicted travel."
    : "Start anywhere on the table and drag back to aim and set power together. Short pulls cancel; release a charged pull to shoot. The POWER bar shows your current strength.";
  canvas.setAttribute("aria-label", separate
    ? "Pool table. Touch a point to aim from the cue ball toward it; drag to refine. Drag the POWER bar down and release to shoot."
    : "Pool table. Drag anywhere on the table to aim and power the shot; release to shoot.");
}
shotControlMode.addEventListener("change", applyShotControlMode);
applyShotControlMode();
const spinSide = document.querySelector("#spin-side");
const spinVertical = document.querySelector("#spin-vertical");
const updateSpin = () => wasmModule?.set_cue_spin(Number(spinSide.value), Number(spinVertical.value));
spinSide.addEventListener("input", updateSpin);
spinVertical.addEventListener("input", updateSpin);
document.querySelector("#reset-spin").addEventListener("click", () => {
  spinSide.value = "0";
  spinVertical.value = "0";
  updateSpin();
});
const fullscreenButton = document.querySelector("#fullscreen-table");
fullscreenButton.hidden = !document.documentElement.requestFullscreen;
fullscreenButton.addEventListener("click", async () => {
  try {
    await document.documentElement.requestFullscreen();
    document.querySelector("#game-menu").open = false;
  } catch {
    fullscreenButton.textContent = "Full screen unavailable";
  }
});
const unlimitedAimRange = document.querySelector("#unlimited-aim-range");
unlimitedAimRange.addEventListener("change", () => {
  wasmModule?.set_unlimited_aim_range(unlimitedAimRange.checked);
});
aimMode.addEventListener("change", () => {
  wasmModule?.set_aim_mode(Number(aimMode.value));
});

function applyMatchAccess(access, requestedAtMs = Date.now(), receivedAtMs = Date.now()) {
  if (
    !Number.isSafeInteger(requestedAtMs) ||
    !Number.isSafeInteger(receivedAtMs) ||
    requestedAtMs < 0 ||
    receivedAtMs < requestedAtMs
  ) {
    throw new Error("match access timing sample is invalid");
  }
  const revision = Number(access.revision);
  if (!Number.isSafeInteger(revision) || revision < 0) {
    throw new Error("match access revision is invalid");
  }
  if (matchAccessRevision !== null && revision < matchAccessRevision) {
    return false;
  }
  const player = Number(access.player);
  if (player !== 1 && player !== 2) {
    throw new Error("match access participant seat is invalid");
  }
  const activePlayer = Number(access.active_player);
  if (activePlayer !== 0 && activePlayer !== 1 && activePlayer !== 2) {
    throw new Error("match access active player is invalid");
  }
  const completed = access.completed === true;
  if (completed !== (activePlayer === 0)) {
    throw new Error("match access lifecycle is inconsistent");
  }
  const serverTimeMs = Number(access.server_time_ms);
  if (!Number.isSafeInteger(serverTimeMs) || serverTimeMs < 0) {
    throw new Error("match access server time is invalid");
  }
  const deadlineAtMs =
    access.deadline_at_ms === null || access.deadline_at_ms === undefined
      ? null
      : Number(access.deadline_at_ms);
  if (deadlineAtMs !== null && (!Number.isSafeInteger(deadlineAtMs) || deadlineAtMs < 0)) {
    throw new Error("match access deadline is invalid");
  }
  if (completed !== (deadlineAtMs === null)) {
    throw new Error("match access deadline lifecycle is inconsistent");
  }
  matchPlayerSeat = player;
  if (wasmModule !== null) {
    wasmModule.set_match_player(player);
  }
  matchDeadlineAtMs = deadlineAtMs;
  matchDeadlineRevision = revision;
  if (requestedAtMs >= matchServerTimeRequestStartedAt) {
    const roundTripMs = receivedAtMs - requestedAtMs;
    const requestMidpointMs = requestedAtMs + Math.floor(roundTripMs / 2);
    matchServerTimeOffsetMs = serverTimeMs - requestMidpointMs;
    matchServerTimeRequestStartedAt = requestedAtMs;
    matchServerTimeUncertaintyMs = Math.ceil(roundTripMs / 2);
  }
  matchAccessRevision = revision;
  updateTurnTimer();
  return true;
}

async function refreshMatchAccess(matchId, minimumRevision) {
  if (matchAccessRefreshInFlight) {
    matchAccessRefreshPendingRevision = Math.max(
      matchAccessRefreshPendingRevision ?? 0,
      minimumRevision,
    );
    return;
  }
  matchAccessRefreshInFlight = true;
  try {
    const requestedAtMs = Date.now();
    const access = await apiRequest(`/api/matches/${matchId}`);
    const receivedAtMs = Date.now();
    if (Number(access.revision) < minimumRevision) {
      return;
    }
    applyMatchAccess(access, requestedAtMs, receivedAtMs);
  } finally {
    matchAccessRefreshInFlight = false;
    const pendingRevision = matchAccessRefreshPendingRevision;
    matchAccessRefreshPendingRevision = null;
    if (pendingRevision !== null && pendingRevision !== matchAccessRevision) {
      void refreshMatchAccess(matchId, pendingRevision).catch((error) => {
        console.error("PWMTF queued match deadline refresh failed", error);
      });
    }
  }
}

function updateTurnTimer() {
  if (matchDeadlineAtMs === null || !Number.isFinite(matchDeadlineAtMs)) {
    turnTimer.hidden = true;
    turnTimer.textContent = "";
    return;
  }
  const estimatedServerNowMs = Date.now() + matchServerTimeOffsetMs;
  const optimisticRemainingMs = matchDeadlineAtMs - estimatedServerNowMs + matchServerTimeUncertaintyMs;
  const remainingSeconds = Math.max(0, Math.ceil(optimisticRemainingMs / 1_000));
  turnTimer.hidden = false;
  turnTimer.textContent = `${remainingSeconds}s`;
  if (remainingSeconds === 0) {
    turnTimer.dataset.expired = "true";
  } else {
    turnTimer.removeAttribute("data-expired");
  }
}

function completionReasonText(reason, localPlayerWon) {
  if (reason === "concession") {
    return localPlayerWon ? "Your opponent conceded." : "The match ended by concession.";
  }
  if (reason === "legal-eight-ball") {
    return "The 8-ball was legally pocketed.";
  }
  if (reason === "illegal-eight-ball") {
    return "The 8-ball was pocketed illegally.";
  }
  return "The authoritative match is complete.";
}

function presentMatchCompletion(module) {
  const winner = Number(module.match_winner());
  if (winner === 0) {
    return false;
  }
  const localPlayerWon = matchPlayerSeat === winner;
  matchDeadlineAtMs = null;
  matchDeadlineRevision = null;
  matchServerTimeOffsetMs = 0;
  matchServerTimeRequestStartedAt = 0;
  matchServerTimeUncertaintyMs = 0;
  matchAccessRevision = null;
  matchAccessRefreshPendingRevision = null;
  updateTurnTimer();
  concedeMatchButton.hidden = true;
  concedeMatchButton.disabled = true;
  offerRematchButton.hidden = false;
  matchResult.hidden = false;
  matchResult.dataset.outcome = localPlayerWon ? "win" : "loss";
  matchResultTitle.textContent = localPlayerWon ? "You win" : `Player ${winner} wins`;
  matchResultDetail.textContent = completionReasonText(
    String(module.match_completion_reason()),
    localPlayerWon,
  );
  matchStatus.textContent = "Match complete · result saved";
  return true;
}

function startMatchSocket(module) {
  const parameters = new URLSearchParams(window.location.search);
  const matchId = parameters.get("match");
  if (!/^\d{1,39}$/.test(matchId ?? "")) {
    return;
  }
  const scheme = window.location.protocol === "https:" ? "wss:" : "ws:";
  matchSubscriptionUrl = `${scheme}//${window.location.host}/ws?match_id=${matchId}`;
  const connect = () => {
    if (!matchSubscriptionUrl || document.visibilityState === "hidden" || !navigator.onLine) {
      return;
    }
    if (matchReconnectTimer !== null) {
      window.clearTimeout(matchReconnectTimer);
      matchReconnectTimer = null;
    }
    try {
      module.connect_match_socket(matchSubscriptionUrl);
      matchSocketActive = true;
      matchConnectStartedAt = performance.now();
    } catch (error) {
      console.error("PWMTF match socket failed", error);
      matchSocketActive = false;
      scheduleReconnect();
    }
  };
  const scheduleReconnect = () => {
    if (
      matchReconnectTimer !== null ||
      !matchSubscriptionUrl ||
      document.visibilityState === "hidden" ||
      !navigator.onLine
    ) {
      return;
    }
    const delay = Math.min(30_000, Math.max(500, Number(module.match_socket_retry_delay_ms())));
    matchReconnectTimer = window.setTimeout(() => {
      matchReconnectTimer = null;
      connect();
    }, delay);
  };
  const monitor = window.setInterval(() => {
    updateTurnTimer();
    if (module.match_socket_ready()) {
      matchSocketActive = true;
      matchConnectStartedAt = null;
      const commandRejected = module.match_command_rejected();
      if (commandRejected) {
        concedeMatchButton.disabled = false;
      }
      const commandPending = module.match_has_pending_prediction();
      if (presentMatchCompletion(module)) {
        updateGameplayAudio(module);
        return;
      }
      matchResult.hidden = true;
      matchResult.removeAttribute("data-outcome");
      concedeMatchButton.hidden = false;
      concedeMatchButton.disabled = commandPending;
      const revision = module.match_revision();
      if (
        revision !== undefined &&
        revision !== matchAccessRevision &&
        !commandPending
      ) {
        void refreshMatchAccess(matchId, revision).catch((error) => {
          console.error("PWMTF match deadline refresh failed", error);
        });
      }
      const activePlayer = Number(module.match_active_player());
      const turn =
        activePlayer === 0
          ? ""
          : matchPlayerSeat === activePlayer
            ? " · your turn"
            : ` · player ${activePlayer}'s turn`;
      matchStatus.textContent = commandRejected
        ? "Command rejected · synchronized to authority"
        : commandPending
          ? "Waiting for authority…"
          : `${revision === undefined ? "Connected" : `Connected · revision ${revision}`}${turn}`;
      if (
        revision !== undefined &&
        matchDeadlineRevision !== null &&
        revision > matchDeadlineRevision
      ) {
        matchDeadlineAtMs = null;
        matchDeadlineRevision = null;
      }
      updateGameplayAudio(module);
      return;
    }
    if (
      matchSocketActive &&
      matchConnectStartedAt !== null &&
      performance.now() - matchConnectStartedAt >= 10_000
    ) {
      matchSocketActive = false;
      matchConnectStartedAt = null;
      module.disconnect_match_socket();
      scheduleReconnect();
      return;
    }
    if (matchSocketActive && module.match_socket_needs_reconnect()) {
      matchSocketActive = false;
      matchConnectStartedAt = null;
      concedeMatchButton.hidden = true;
      matchStatus.textContent = "Connection lost · reconnecting";
      scheduleReconnect();
    }
  }, 500);
  window.addEventListener("online", connect);
  concedeMatchButton.addEventListener("click", () => {
    if (!window.confirm("Concede this match? Your opponent will win.")) {
      return;
    }
    if (matchPlayerSeat === null) {
      matchStatus.textContent = "Participant seat is unavailable.";
      return;
    }
    try {
      module.send_concession(matchPlayerSeat);
      concedeMatchButton.disabled = true;
      matchStatus.textContent = "Concession submitted…";
    } catch (error) {
      console.error("PWMTF concession failed", error);
      matchStatus.textContent = "Concession failed. Reconnect and try again.";
    }
  });
  window.addEventListener("offline", () => {
    module.disconnect_match_socket();
    matchSocketActive = false;
    matchConnectStartedAt = null;
    concedeMatchButton.hidden = true;
    matchStatus.textContent = "Offline · reconnecting when network returns";
  });
  window.addEventListener("blur", () => {
    module.interrupt_match_gesture();
  });
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden") {
      module.interrupt_match_gesture();
      module.disconnect_match_socket();
      matchSocketActive = false;
      matchConnectStartedAt = null;
    } else if (!matchSocketActive) {
      connect();
    }
  });
  window.addEventListener("pagehide", () => {
    window.clearInterval(monitor);
    stopLobbyPolling();
    stopSocialPolling();
    void disconnectLobbyPresence().catch(() => {});
    if (matchReconnectTimer !== null) {
      window.clearTimeout(matchReconnectTimer);
    }
    module.disconnect_match_socket();
  });
  connect();
}

let runtimeAuth = { development_login: false, google_login: true };

function renderLobbyList(lobbies) {
  lobbyList.replaceChildren();
  for (const lobby of lobbies) {
    const row = document.createElement("div");
    row.className = "social-row";
    const label = document.createElement("span");
    label.textContent = lobby.status === "started" ? "Started match" : "Waiting lobby";
    const open = document.createElement("button");
    open.type = "button";
    open.textContent = "Open";
    open.addEventListener("click", () => {
      if (lobby.status === "started" && lobby.match_id !== null) {
        const url = new URL(window.location.href);
        setMatchLocation(url, lobby.match_id);
        window.location.assign(url);
      } else {
        enterLobby(lobby);
      }
    });
    row.append(label, open);
    lobbyList.append(row);
  }
}

async function refreshSocialState() {
  if (socialRefreshInFlight || document.visibilityState === "hidden") {
    return;
  }
  socialRefreshInFlight = true;
  try {
    const [challenges, rematches, lobbies] = await Promise.all([
      loadPendingChallenges(),
      loadPendingRematches(),
      apiRequest("/api/lobbies"),
    ]);
    renderChallenges(challenges);
    renderRematches(rematches);
    renderLobbyList(lobbies);
    const current = activeLobbyId === null ? null : String(activeLobbyId);
    const candidate = lobbies.find((lobby) => lobby.status === "waiting") ?? null;
    if (candidate !== null && candidate.lobby_id !== current) {
      enterLobby(candidate);
    }
  } catch (error) {
    socialBackgroundFailure(error);
  } finally {
    socialRefreshInFlight = false;
  }
}

function startSocialPolling() {
  if (socialPollTimer !== null || new URLSearchParams(window.location.search).has("match")) {
    return;
  }
  const poll = async () => {
    await refreshSocialState();
    socialPollTimer = window.setTimeout(poll, 1_000);
  };
  socialPollTimer = window.setTimeout(poll, 1_000);
}

function stopSocialPolling() {
  if (socialPollTimer !== null) {
    window.clearTimeout(socialPollTimer);
    socialPollTimer = null;
  }
}

async function refreshSession() {
  const response = await fetch("/api/session", {
    credentials: "same-origin",
    headers: { Accept: "application/json" },
  });
  accountPanel.hidden = false;
  if (response.status === 401) {
    accountLabel.textContent = "Play online with friends";
    googleSignIn.hidden = !runtimeAuth.google_login;
    developmentSignIn.hidden = !runtimeAuth.development_login;
    signOut.hidden = true;
    socialPanel.hidden = true;
    stopSocialPolling();
    return;
  }
  if (!response.ok) {
    throw new Error(`session request failed with ${response.status}`);
  }
  const session = await response.json();
  accountLabel.textContent = session.handle ? `Signed in as @${session.handle}` : "Signed in";
  handleInput.value = session.handle ?? "";
  googleSignIn.hidden = true;
  developmentSignIn.hidden = true;
  signOut.hidden = false;
  socialPanel.hidden = false;
  await refreshSocialState();
  startSocialPolling();
  return session;
}

function apiRequest(path, options = {}) {
  const requestHeaders = {
    Accept: "application/json",
    "X-PWMTF-Origin": window.location.origin,
  };
  if (options.body !== undefined) {
    requestHeaders["Content-Type"] = "application/json";
  }
  return fetch(path, {
    credentials: "same-origin",
    ...options,
    headers: requestHeaders,
  }).then(async (response) => {
    if (!response.ok) {
      const message = await response.text();
      throw new Error(message || `request failed with ${response.status}`);
    }
    return response.status === 204 ? null : response.json();
  });
}

function socialFailure(error) {
  console.error("PWMTF social operation failed", error);
  socialStatus.textContent = error instanceof Error ? error.message : "Operation failed";
}

function socialBackgroundFailure(error) {
  console.error("PWMTF social refresh failed", error);
}

function stopLobbyPolling() {
  if (lobbyPollTimer !== null) {
    window.clearTimeout(lobbyPollTimer);
    lobbyPollTimer = null;
  }
}

async function disconnectLobbyPresence() {
  if (activeLobbyId === null || activeLobbyConnectionId === null) {
    return;
  }
  const lobbyId = activeLobbyId;
  const connectionId = activeLobbyConnectionId;
  activeLobbyConnectionId = null;
  await apiRequest(`/api/lobbies/${lobbyId}/connections/${connectionId}`, { method: "DELETE" });
}

function setMatchLocation(url, matchId) {
  url.searchParams.set("match", matchId);
  url.searchParams.delete("player_one");
  url.searchParams.delete("player_two");
}

async function pollLobby() {
  if (activeLobbyId === null || document.visibilityState === "hidden") {
    return;
  }
  try {
    const lobby = await apiRequest(`/api/lobbies/${activeLobbyId}`);
    lobbyState.textContent =
      lobby.status === "waiting"
        ? lobby.both_ready
          ? "Both players ready. Starting match…"
          : "Waiting for both players"
        : lobby.status;
    if (lobby.status === "started" && lobby.match_id !== null) {
      stopLobbyPolling();
      activeLobbyConnectionId = null;
      const url = new URL(window.location.href);
      setMatchLocation(url, lobby.match_id);
      window.location.assign(url);
      return;
    }
    if (lobby.status === "cancelled") {
      stopLobbyPolling();
      activeLobbyId = null;
      activeLobbyConnectionId = null;
      cancelLobbyButton.hidden = true;
      readyLobbyButton.hidden = true;
      return;
    }
    if (activeLobbyConnectionId !== null) {
      await apiRequest(`/api/lobbies/${activeLobbyId}/connections/${activeLobbyConnectionId}`, {
        method: "POST",
      });
    }
  } catch (error) {
    socialFailure(error);
  }
  if (activeLobbyId !== null) {
    lobbyPollTimer = window.setTimeout(() => void pollLobby(), 1_000);
  }
}

function enterLobby(lobby) {
  socialPanel.open = true;
  const nextLobbyId = String(lobby.lobby_id);
  if (activeLobbyId === nextLobbyId && activeLobbyConnectionId !== null) {
    return;
  }
  stopLobbyPolling();
  void disconnectLobbyPresence().catch(socialFailure);
  activeLobbyId = nextLobbyId;
  lobbyPanel.hidden = false;
  lobbyLabel.textContent = `Lobby ${lobby.lobby_id}`;
  lobbyState.textContent = "Waiting for both players";
  cancelLobbyButton.hidden = false;
  readyLobbyButton.hidden = false;
  readyLobbyButton.disabled = true;
  socialStatus.textContent = `Joined waiting lobby ${lobby.lobby_id}.`;
  if (activeLobbyId !== null && activeLobbyConnectionId === null) {
    socialStatus.textContent = "Connecting to lobby…";
  }
  void apiRequest(`/api/lobbies/${lobby.lobby_id}`, { method: "POST" })
    .then((connection) => {
      if (activeLobbyId !== nextLobbyId) {
        return apiRequest(`/api/lobbies/${nextLobbyId}/connections/${connection.connection_id}`, { method: "DELETE" });
      }
      activeLobbyConnectionId = connection.connection_id;
      readyLobbyButton.disabled = false;
      return pollLobby();
    })
    .catch(socialFailure);
}

readyLobbyButton.addEventListener("click", async () => {
  if (activeLobbyId === null || activeLobbyConnectionId === null) {
    return;
  }
  readyLobbyButton.disabled = true;
  try {
    const lobby = await apiRequest(`/api/lobbies/${activeLobbyId}/ready`, { method: "POST" });
    lobbyState.textContent = lobby.both_ready
      ? "Both players ready. Starting match…"
      : "Ready. Waiting for your friend.";
  } catch (error) {
    readyLobbyButton.disabled = false;
    socialFailure(error);
  }
});

cancelLobbyButton.addEventListener("click", async () => {
  if (activeLobbyId === null) {
    return;
  }
  try {
    const lobby = await apiRequest(`/api/lobbies/${activeLobbyId}`, { method: "DELETE" });
    activeLobbyConnectionId = null;
    lobbyState.textContent = lobby.status;
    activeLobbyId = null;
    stopLobbyPolling();
    cancelLobbyButton.hidden = true;
    readyLobbyButton.hidden = true;
  } catch (error) {
    socialFailure(error);
  }
});

async function acceptRematch(matchId) {
  socialStatus.textContent = "Accepting rematch…";
  try {
    const match = await apiRequest(`/api/matches/${matchId}/rematch/accept`, { method: "POST" });
    const url = new URL(window.location.href);
    setMatchLocation(url, match.match_id);
    window.location.assign(url);
  } catch (error) {
    socialFailure(error);
  }
}

async function loadPendingRematches() {
  return apiRequest("/api/rematches");
}

function renderRematches(rematches) {
  rematchList.replaceChildren();
  if (rematches.length === 0) {
    rematchList.textContent = "No rematch offers.";
    return;
  }
  for (const rematch of rematches) {
    const row = document.createElement("div");
    const label = document.createElement("span");
    label.textContent = `Match ${rematch.previous_match_id}`;
    const accept = document.createElement("button");
    accept.type = "button";
    accept.textContent = "Accept";
    accept.addEventListener("click", () => void acceptRematch(rematch.previous_match_id));
    row.append(label, accept);
    rematchList.append(row);
  }
}

async function refreshRematches() {
  renderRematches(await loadPendingRematches());
}

async function offerRematch(matchId) {
  offerRematchButton.disabled = true;
  try {
    await apiRequest(`/api/matches/${matchId}/rematch`, { method: "POST" });
    offerRematchButton.textContent = "Rematch offered";
    socialStatus.textContent = "Rematch offered.";
  } catch (error) {
    offerRematchButton.disabled = false;
    throw error;
  }
}

offerRematchButton.addEventListener("click", () => {
  const matchId = new URLSearchParams(window.location.search).get("match");
  if (matchId !== null) {
    void offerRematch(matchId).catch(socialFailure);
  }
});

const currentMatchId = new URLSearchParams(window.location.search).get("match");
if (/^\d{1,39}$/.test(currentMatchId ?? "")) {
  offerRematchButton.hidden = true;
}

async function acceptChallenge(challengeId, button) {
  if (button.disabled) {
    return;
  }
  button.disabled = true;
  socialStatus.textContent = "Accepting challenge…";
  try {
    const lobby = await apiRequest(`/api/challenges/${challengeId}/accept`, { method: "POST" });
    enterLobby(lobby);
    await refreshSocialState();
  } catch (error) {
    socialFailure(error);
  } finally {
    button.disabled = false;
  }
}

async function loadPendingChallenges() {
  return apiRequest("/api/challenges");
}

function renderChallenges(challenges) {
  challengeList.replaceChildren();
  if (challenges.length === 0) {
    challengeList.textContent = "No incoming challenges.";
    return;
  }
  for (const challenge of challenges) {
    const row = document.createElement("div");
    const label = document.createElement("span");
    label.textContent = `@${challenge.from_handle}`;
    const accept = document.createElement("button");
    accept.type = "button";
    accept.textContent = "Accept";
    accept.addEventListener("click", () => void acceptChallenge(challenge.challenge_id, accept));
    row.append(label, accept);
    challengeList.append(row);
  }
}

handleForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  socialStatus.textContent = "Saving handle…";
  try {
    const profile = await apiRequest("/api/profile/handle", {
      method: "PUT",
      body: JSON.stringify({ handle: handleInput.value.trim() }),
    });
    accountLabel.textContent = `Signed in as @${profile.handle}`;
    socialStatus.textContent = `Handle @${profile.handle} saved.`;
  } catch (error) {
    socialFailure(error);
  }
});

async function refreshChallenges() {
  renderChallenges(await loadPendingChallenges());
}

challengeForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  socialStatus.textContent = "Creating challenge…";
  try {
    const challenge = await apiRequest("/api/challenges", {
      method: "POST",
      body: JSON.stringify({ handle: challengeHandle.value.trim() }),
    });
    socialStatus.textContent = `Challenge ${challenge.challenge_id} is waiting for acceptance.`;
  } catch (error) {
    socialFailure(error);
  }
});

async function copyInvitation(value) {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(value);
    return "Private invitation copied to the clipboard.";
  }
  return `Private invitation: ${value}`;
}

createInvitationButton.addEventListener("click", async () => {
  socialStatus.textContent = "Creating invitation…";
  try {
    const invitation = await apiRequest("/api/invitations", { method: "POST" });
    socialStatus.textContent = await copyInvitation(invitation.invitation_url);
  } catch (error) {
    socialFailure(error);
  }
});

async function redeemInvitationFromUrl() {
  const token = new URLSearchParams(window.location.search).get("invite");
  if (token === null) {
    return;
  }
  try {
    const lobby = await apiRequest("/api/invitations/redeem", {
      method: "POST",
      body: JSON.stringify({ token }),
    });
    const url = new URL(window.location.href);
    url.searchParams.delete("invite");
    window.history.replaceState(null, "", url);
    enterLobby(lobby);
  } catch (error) {
    socialFailure(error);
  }
}

googleSignIn.addEventListener("submit", () => {
  googleSignIn.querySelector("button").disabled = true;
});

developmentSignIn.addEventListener("submit", async (event) => {
  event.preventDefault();
  const button = developmentSignIn.querySelector("button");
  button.disabled = true;
  try {
    const response = await fetch("/auth/development", {
      method: "POST",
      credentials: "same-origin",
      headers: {
        Accept: "application/json",
        "Content-Type": "application/json",
        "X-PWMTF-Origin": window.location.origin,
      },
      body: JSON.stringify({ username: developmentUsername.value.trim() }),
    });
    if (!response.ok) {
      const message = await response.text();
      throw new Error(message || `local sign in failed with ${response.status}`);
    }
    developmentUsername.value = "";
    await refreshSession();
  } catch (error) {
    console.error("PWMTF local development sign in failed", error);
    accountLabel.textContent = error instanceof Error ? error.message : "Local sign in failed";
  } finally {
    button.disabled = false;
  }
});

signOut.addEventListener("click", async () => {
  signOut.disabled = true;
  try {
    const response = await fetch("/api/session", {
      method: "DELETE",
      credentials: "same-origin",
      headers: { Accept: "application/json" },
    });
    if (!response.ok) {
      throw new Error(`sign out failed with ${response.status}`);
    }
    await refreshSession();
  } catch (error) {
    console.error("PWMTF sign out failed", error);
    accountLabel.textContent = "Sign out failed. Try again.";
  } finally {
    signOut.disabled = false;
  }
});

const runtimeAuthReady = fetch("/api/auth/runtime", {
  credentials: "same-origin",
  headers: { Accept: "application/json" },
}).then(async (response) => {
  if (!response.ok) {
    throw new Error(`authentication runtime request failed with ${response.status}`);
  }
  runtimeAuth = await response.json();
});

const sessionReady = runtimeAuthReady.then(() => refreshSession())
  .then(async (session) => {
    const matchId = new URLSearchParams(window.location.search).get("match");
    if (session !== undefined && /^\d{1,39}$/.test(matchId ?? "")) {
      const requestedAtMs = Date.now();
      const access = await apiRequest(`/api/matches/${matchId}`);
      applyMatchAccess(access, requestedAtMs, Date.now());
      const url = new URL(window.location.href);
      url.searchParams.delete("player_one");
      url.searchParams.delete("player_two");
      window.history.replaceState(null, "", url);
    }
    await redeemInvitationFromUrl();
    return session;
  })
  .catch((error) => {
    console.error("PWMTF session lookup failed", error);
    accountPanel.hidden = false;
    accountLabel.textContent = "Account status unavailable";
    googleSignIn.hidden = !runtimeAuth.google_login;
    developmentSignIn.hidden = !runtimeAuth.development_login;
    return undefined;
  });

try {
  if (!document.createElement("canvas").getContext("webgl2")) {
    throw new Error("WebGL 2 is unavailable");
  }
  wasmModule = await import("./pwmtf_client.js");
  await wasmModule.default();
  wasmModule.set_aim_mode(Number(aimMode.value));
  applyShotControlMode();
  updateSpin();
  wasmModule.set_unlimited_aim_range(unlimitedAimRange.checked);
  await sessionReady;
  if (matchPlayerSeat !== null) {
    wasmModule.set_match_player(matchPlayerSeat);
  }
  startMatchSocket(wasmModule);
  await new Promise((resolve) => window.requestAnimationFrame(resolve));
  await new Promise((resolve) => window.requestAnimationFrame(resolve));
  telemetry.clientReadyMs = performance.now() - navigationStartedAt;
  shell.dataset.clientState = "ready";
  loading.remove();
  canvas.focus({ preventScroll: true });
  refreshMetrics();
} catch (error) {
  console.error("PWMTF client startup failed", error);
  shell.dataset.clientState = "failed";
  loading.remove();
  loadError.hidden = false;
  refreshMetrics();
}
