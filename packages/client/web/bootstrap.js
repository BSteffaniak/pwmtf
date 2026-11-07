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
const query = new URLSearchParams(window.location.search);
const feasibilityEnabled = query.has("feasibility");
const activePresentationTier = query.get("tier") === "reduced" ? "reduced" : "default";
// Build tooling injects and verifies these exact lines; keep each on one line.
const candidateBuildId = "__PWMTF_BUILD_ID__";
const candidateSourceHash = "__PWMTF_SOURCE_HASH__";
const candidateBundleHash = "__PWMTF_BUNDLE_HASH__";
const candidateBundleHashAlgorithm = "sha256-length-prefixed-v1";
const candidateWasmOptimization = "__PWMTF_WASM_OPTIMIZATION__";
const candidateBevyVersion = "0.19.1";
const candidateRenderer = "WebGL2";
const navigationStartedAt = performance.now();
const MATCH_ID_PATTERN = /^\d{1,39}$/u;

// One AudioContext per page. Normal play owns it here; a feasibility capture
// supplies its own port so probes and gameplay cues share one graph.
const localAudio = {
  context: null,
  masterGain: null,
  ensureGraph() {
    if (this.context !== null) {
      return;
    }
    const AudioContext = window.AudioContext || window.webkitAudioContext;
    this.context = new AudioContext();
    this.masterGain = this.context.createGain();
    this.masterGain.gain.value = 0.12;
    this.masterGain.connect(this.context.destination);
  },
  markResumed() {},
  resumeOnContact: true,
};
const audioSupported = Boolean(window.AudioContext || window.webkitAudioContext);
let gameAudio = localAudio;
let gameplayAudioEnabled = false;
let feasibility = null;

function playTone(frequency, duration, volume = 0.45) {
  const context = gameAudio.context;
  if (!gameplayAudioEnabled || context === null || context.state !== "running") {
    return;
  }
  const oscillator = context.createOscillator();
  const envelope = context.createGain();
  const startedAt = context.currentTime;
  oscillator.type = "triangle";
  oscillator.frequency.setValueAtTime(frequency, startedAt);
  envelope.gain.setValueAtTime(0.0001, startedAt);
  envelope.gain.exponentialRampToValueAtTime(volume, startedAt + 0.01);
  envelope.gain.exponentialRampToValueAtTime(0.0001, startedAt + duration);
  oscillator.connect(envelope);
  envelope.connect(gameAudio.masterGain);
  oscillator.start(startedAt);
  oscillator.stop(startedAt + duration);
  oscillator.addEventListener("ended", () => {
    oscillator.disconnect();
    envelope.disconnect();
  });
}

async function enableGameplayAudio() {
  if (!audioSupported) {
    return;
  }
  if (gameplayAudioEnabled) {
    if (gameAudio.resumeOnContact && gameAudio.context?.state === "suspended") {
      await gameAudio.context.resume();
    }
    return;
  }
  gameAudio.ensureGraph();
  if (gameAudio.context.state !== "running") {
    await gameAudio.context.resume();
  }
  if (gameAudio.context.state === "running") {
    gameplayAudioEnabled = true;
    gameAudio.markResumed();
    playTone(440, 0.08, 0.25);
  }
}

function playRevisionCue() {
  playTone(196, 0.12);
  window.setTimeout(() => playTone(294, 0.09, 0.3), 70);
}

if (feasibilityEnabled) {
  try {
    const { startFeasibility } = await import("./feasibility.js");
    feasibility = startFeasibility({
      identity: {
        buildId: candidateBuildId,
        sourceHash: candidateSourceHash,
        bundleHash: candidateBundleHash,
        bundleHashAlgorithm: candidateBundleHashAlgorithm,
        wasmOptimization: candidateWasmOptimization,
        bevyVersion: candidateBevyVersion,
        renderer: candidateRenderer,
      },
      presentationTier: activePresentationTier,
      navigationStartedAt,
    });
    gameAudio = feasibility.audio;
  } catch (error) {
    console.error("PWMTF feasibility tooling failed to load", error);
  }
}

reload.addEventListener("click", () => window.location.reload());
canvas.addEventListener("pointerdown", () => {
  void enableGameplayAudio().catch((error) => {
    console.error("PWMTF gameplay audio failed", error);
  });
});

// Initialized WASM module; null until its exports are safe to call.
let wasmModule = null;
const aimMode = document.querySelector("#aim-mode");
const shotControlMode = document.querySelector("#shot-control-mode");
const unlimitedAimRange = document.querySelector("#unlimited-aim-range");
// Device-local presentation preferences only; never persist shot inputs or identity.
const preferencesKey = "pwmtf.presentation.v1";
function loadPresentationPreferences() {
  try {
    const raw = window.localStorage.getItem(preferencesKey);
    if (raw === null || raw.length > 256) return;
    const value = JSON.parse(raw);
    if (value === null || typeof value !== "object" || Array.isArray(value) || value.version !== 1) return;
    if (value.shotControl === 0 || value.shotControl === 1) shotControlMode.value = String(value.shotControl);
    if (value.aimGuide === 0 || value.aimGuide === 1 || value.aimGuide === 2) aimMode.value = String(value.aimGuide);
    if (typeof value.unlimitedRange === "boolean") unlimitedAimRange.checked = value.unlimitedRange;
  } catch {
    // Browsers can deny storage; the visible defaults must still work.
  }
}
function savePresentationPreferences() {
  try {
    window.localStorage.setItem(preferencesKey, JSON.stringify({
      version: 1,
      shotControl: Number(shotControlMode.value),
      aimGuide: Number(aimMode.value),
      unlimitedRange: unlimitedAimRange.checked,
    }));
  } catch {
    // A private or storage-blocked browser still supports in-page changes.
  }
}
loadPresentationPreferences();
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
shotControlMode.addEventListener("change", () => {
  applyShotControlMode();
  savePresentationPreferences();
});
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
unlimitedAimRange.addEventListener("change", () => {
  wasmModule?.set_unlimited_aim_range(unlimitedAimRange.checked);
  savePresentationPreferences();
});
aimMode.addEventListener("change", () => {
  wasmModule?.set_aim_mode(Number(aimMode.value));
  savePresentationPreferences();
});

// Session state. The WASM transport owns the single `/ws` socket, retries,
// pings, clock sync, subscriptions, and pending commands; this page only
// renders its events and tells it when the tab may hold a connection.
let signedIn = false;
let sessionStarted = false;
let sessionActive = false;
let connectionStatus = "inactive";
let currentMatchId = null;
let currentMatch = null;
let lastRejection = null;
let activeLobbyId = null;
let turnTimerInterval = null;
let socialRefreshInFlight = false;
let socialRefreshQueued = false;
let runtimeAuth = { development_login: false, google_login: true };

const REJECTION_MESSAGES = {
  malformed: "The server could not read that move. The table was resynchronized.",
  "stale-revision": "The table changed before your move arrived. Synchronized to the latest state.",
  "wrong-turn": "It is not your turn.",
  "match-completed": "This match is already over.",
  "illegal-move": "That move is not allowed. Synchronized to the server.",
  "idempotency-conflict": "That move conflicted with an earlier submission. Synchronized to the server.",
  unavailable: "The server could not save that move. Try again.",
  "not-subscribed": "Not subscribed to this match yet. Reconnecting…",
  superseded: "The table moved on before your move was confirmed.",
};

function isDecimalId(value) {
  return typeof value === "string" && MATCH_ID_PATTERN.test(value);
}

function isUnsignedSafeInteger(value) {
  return Number.isSafeInteger(value) && value >= 0;
}

function parseMatchEvent(detail) {
  const seat = detail?.seat;
  const activePlayer = detail?.active_player;
  const winner = detail?.winner;
  const deadline = detail?.deadline_at_ms;
  if (
    !isDecimalId(detail?.match_id) ||
    !isUnsignedSafeInteger(detail.revision) ||
    (seat !== 1 && seat !== 2) ||
    ![0, 1, 2].includes(activePlayer) ||
    ![0, 1, 2].includes(winner) ||
    typeof detail.completion_reason !== "string" ||
    typeof detail.ball_in_hand !== "boolean" ||
    typeof detail.pending !== "boolean" ||
    typeof detail.playing !== "boolean" ||
    (deadline !== null && !isUnsignedSafeInteger(deadline)) ||
    !Number.isSafeInteger(detail.server_offset_ms) ||
    !isUnsignedSafeInteger(detail.clock_uncertainty_ms)
  ) {
    return null;
  }
  return detail;
}

function matchLocation(matchId) {
  const url = new URL(window.location.href);
  if (matchId === null) {
    url.searchParams.delete("match");
  } else {
    url.searchParams.set("match", matchId);
  }
  url.searchParams.delete("player_one");
  url.searchParams.delete("player_two");
  return url;
}

function updateSessionActivity() {
  if (!sessionStarted) {
    return;
  }
  const active = signedIn && document.visibilityState !== "hidden" && navigator.onLine;
  if (active === sessionActive) {
    return;
  }
  sessionActive = active;
  wasmModule.session_set_active(active);
  renderMatchChrome();
}

function ensureSession() {
  if (wasmModule === null || !signedIn) {
    return;
  }
  if (!sessionStarted) {
    wasmModule.session_start();
    sessionStarted = true;
    sessionActive = true;
  }
  updateSessionActivity();
}

function handleSessionEvent(name, detailJson) {
  let detail;
  try {
    detail = JSON.parse(detailJson);
  } catch (error) {
    console.error("PWMTF session event was not JSON", name, error);
    return;
  }
  switch (name) {
    case "connection":
      onConnection(detail);
      break;
    case "match":
      onMatch(detail);
      break;
    case "rejected":
      onRejected(detail);
      break;
    case "lobby-changed":
      if (isDecimalId(detail?.lobby_id) && detail.lobby_id === activeLobbyId) {
        void refreshLobby();
      }
      break;
    case "match-started":
      if (isDecimalId(detail?.lobby_id) && isDecimalId(detail?.match_id)) {
        onMatchStarted(detail.lobby_id, detail.match_id);
      }
      break;
    case "social-changed":
      void refreshSocialState();
      break;
    case "subscription-denied":
      onSubscriptionDenied(detail);
      break;
    default:
      break;
  }
}

function onConnection(detail) {
  const status = detail?.status;
  if (!["connecting", "ready", "reconnecting", "inactive"].includes(status)) {
    return;
  }
  connectionStatus = status;
  if (status === "ready") {
    // Pushes sent while disconnected are not replayed; catch up once.
    void refreshSocialState();
    if (activeLobbyId !== null) {
      void refreshLobby();
    }
  }
  renderMatchChrome();
}

function onMatch(detail) {
  const match = parseMatchEvent(detail);
  if (match === null) {
    console.error("PWMTF ignored an invalid match event");
    return;
  }
  if (match.match_id !== currentMatchId) {
    return;
  }
  if (currentMatch !== null && match.revision > currentMatch.revision) {
    lastRejection = null;
    playRevisionCue();
  }
  if (match.pending && currentMatch !== null && !currentMatch.pending) {
    lastRejection = null;
  }
  currentMatch = match;
  renderMatchChrome();
}

function onRejected(detail) {
  if (!isDecimalId(detail?.match_id) || detail.match_id !== currentMatchId) {
    return;
  }
  lastRejection = Object.hasOwn(REJECTION_MESSAGES, detail.reason) ? detail.reason : "unavailable";
  renderMatchChrome();
}

function onSubscriptionDenied(detail) {
  if (!isDecimalId(detail?.id)) {
    return;
  }
  if (detail.kind === "match" && detail.id === currentMatchId) {
    currentMatch = null;
    matchStatus.textContent = "You are not a participant in this match.";
    matchStatus.dataset.denied = "true";
    concedeMatchButton.hidden = true;
  } else if (detail.kind === "lobby" && detail.id === activeLobbyId) {
    clearLobby();
    socialStatus.textContent = "That lobby is no longer available.";
  }
}

// `match-started` reaches every tab of both accounts (rematches use lobby
// "0"). Only the tab waiting in that lobby, an idle tab, or a tab showing a
// finished match navigates; a tab mid-match or waiting elsewhere stays put.
function onMatchStarted(lobbyId, matchId) {
  if (currentMatchId === matchId) {
    return;
  }
  const ownLobby = activeLobbyId !== null && activeLobbyId === lobbyId;
  const idle = activeLobbyId === null && currentMatchId === null;
  const finishedMatch =
    activeLobbyId === null && currentMatch !== null && currentMatch.active_player === 0;
  if (!ownLobby && !idle && !finishedMatch) {
    return;
  }
  if (ownLobby) {
    clearLobby();
  }
  enterMatch(matchId, { push: true });
}

function enterMatch(matchId, { push }) {
  if (push) {
    window.history.pushState({ match: matchId }, "", matchLocation(matchId));
  }
  currentMatchId = matchId;
  currentMatch = null;
  lastRejection = null;
  socialPanel.open = true;
  offerRematchButton.hidden = true;
  offerRematchButton.disabled = false;
  offerRematchButton.textContent = "Offer rematch";
  delete matchStatus.dataset.denied;
  wasmModule?.subscribe_match(matchId);
  renderMatchChrome();
}

function leaveMatch() {
  currentMatchId = null;
  currentMatch = null;
  lastRejection = null;
  wasmModule?.unsubscribe_match();
  renderMatchChrome();
}

window.addEventListener("popstate", () => {
  const matchId = new URLSearchParams(window.location.search).get("match");
  if (isDecimalId(matchId)) {
    if (matchId !== currentMatchId) {
      enterMatch(matchId, { push: false });
    }
  } else if (currentMatchId !== null) {
    leaveMatch();
  }
});

function remainingTurnMs(match) {
  // Server times are Unix ms; the offset is server minus local Date.now().
  return match.deadline_at_ms - (Date.now() + match.server_offset_ms) + match.clock_uncertainty_ms;
}

function updateTurnTimer() {
  const match = currentMatch;
  if (match === null || match.deadline_at_ms === null || match.active_player === 0) {
    turnTimer.hidden = true;
    turnTimer.textContent = "";
    turnTimer.removeAttribute("data-expired");
    if (turnTimerInterval !== null) {
      window.clearInterval(turnTimerInterval);
      turnTimerInterval = null;
    }
    return;
  }
  const remainingSeconds = Math.max(0, Math.ceil(remainingTurnMs(match) / 1_000));
  turnTimer.hidden = false;
  turnTimer.textContent = `${remainingSeconds}s`;
  if (remainingSeconds === 0) {
    turnTimer.dataset.expired = "true";
  } else {
    turnTimer.removeAttribute("data-expired");
  }
  // Pure local arithmetic from the last event; never a network request.
  turnTimerInterval ??= window.setInterval(updateTurnTimer, 250);
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

function connectionText() {
  if (!signedIn) {
    return "Sign in to join this match.";
  }
  if (connectionStatus === "inactive" || !sessionActive) {
    return navigator.onLine
      ? "Paused while this tab is in the background"
      : "Offline · reconnecting when network returns";
  }
  if (connectionStatus === "reconnecting") {
    return "Connection lost · reconnecting";
  }
  if (connectionStatus === "connecting") {
    return "Connecting…";
  }
  return "Loading match…";
}

function writeMatchDataset(match) {
  const data = matchStatus.dataset;
  data.connection = connectionStatus;
  if (match === null) {
    for (const key of ["revision", "seat", "activePlayer", "winner", "deadlineAtMs", "pending", "playing"]) {
      delete data[key];
    }
    return;
  }
  data.revision = String(match.revision);
  data.seat = String(match.seat);
  data.activePlayer = String(match.active_player);
  data.winner = String(match.winner);
  data.deadlineAtMs = match.deadline_at_ms === null ? "" : String(match.deadline_at_ms);
  data.pending = String(match.pending);
  data.playing = String(match.playing);
}

function renderMatchChrome() {
  if (currentMatchId === null) {
    matchStatus.textContent = "";
    writeMatchDataset(null);
    concedeMatchButton.hidden = true;
    matchResult.hidden = true;
    matchResult.removeAttribute("data-outcome");
    offerRematchButton.hidden = true;
    updateTurnTimer();
    return;
  }
  if (matchStatus.dataset.denied === "true") {
    return;
  }
  const match = currentMatch;
  writeMatchDataset(match);
  updateTurnTimer();
  if (match !== null && match.winner !== 0) {
    const localPlayerWon = match.seat === match.winner;
    concedeMatchButton.hidden = true;
    concedeMatchButton.disabled = true;
    offerRematchButton.hidden = false;
    matchResult.hidden = false;
    matchResult.dataset.outcome = localPlayerWon ? "win" : "loss";
    matchResultTitle.textContent = localPlayerWon ? "You win" : `Player ${match.winner} wins`;
    matchResultDetail.textContent = completionReasonText(match.completion_reason, localPlayerWon);
    matchStatus.textContent = "Match complete · result saved";
    return;
  }
  matchResult.hidden = true;
  matchResult.removeAttribute("data-outcome");
  offerRematchButton.hidden = true;
  if (match === null || connectionStatus !== "ready" || !sessionActive) {
    concedeMatchButton.hidden = true;
    matchStatus.textContent = connectionText();
    return;
  }
  concedeMatchButton.hidden = false;
  concedeMatchButton.disabled = match.pending || match.playing;
  const turn =
    match.active_player === match.seat
      ? match.ball_in_hand
        ? " · your turn · ball in hand"
        : " · your turn"
      : ` · player ${match.active_player}'s turn`;
  const progress = match.pending
    ? " · waiting for authority…"
    : match.playing
      ? " · shot in progress"
      : turn;
  const rejection = lastRejection === null ? "" : `${REJECTION_MESSAGES[lastRejection]} · `;
  matchStatus.textContent = `${rejection}Connected · revision ${match.revision}${progress}`;
}

concedeMatchButton.addEventListener("click", () => {
  if (currentMatch === null || wasmModule === null) {
    return;
  }
  if (!window.confirm("Concede this match? Your opponent will win.")) {
    return;
  }
  try {
    wasmModule.send_concession(currentMatch.seat);
    concedeMatchButton.disabled = true;
  } catch (error) {
    console.error("PWMTF concession failed", error);
    matchStatus.textContent = "Concession failed. Try again once connected.";
  }
});

window.addEventListener("online", updateSessionActivity);
window.addEventListener("offline", updateSessionActivity);
window.addEventListener("blur", () => {
  wasmModule?.interrupt_match_gesture();
});
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "hidden") {
    wasmModule?.interrupt_match_gesture();
    // A feasibility capture records its own background suspensions.
    if (feasibility === null && localAudio.context?.state === "running") {
      void localAudio.context.suspend().catch(() => {});
    }
  }
  updateSessionActivity();
});
window.addEventListener("pagehide", () => {
  if (sessionStarted && sessionActive) {
    sessionActive = false;
    wasmModule.session_set_active(false);
  }
});
window.addEventListener("pageshow", updateSessionActivity);

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
      if (lobby.status === "started" && isDecimalId(lobby.match_id)) {
        enterMatch(lobby.match_id, { push: true });
      } else {
        enterLobby(lobby);
      }
    });
    row.append(label, open);
    lobbyList.append(row);
  }
}

async function refreshSocialState() {
  if (!signedIn) {
    return;
  }
  if (socialRefreshInFlight) {
    // Never drop a push that arrives mid-refresh; run once more afterwards.
    socialRefreshQueued = true;
    return;
  }
  socialRefreshInFlight = true;
  try {
    const [challenges, rematches, lobbies] = await Promise.all([
      apiRequest("/api/challenges"),
      apiRequest("/api/rematches"),
      apiRequest("/api/lobbies"),
    ]);
    renderChallenges(challenges);
    renderRematches(rematches);
    renderLobbyList(lobbies);
    const candidate = lobbies.find((lobby) => lobby.status === "waiting") ?? null;
    if (currentMatchId === null && candidate !== null && String(candidate.lobby_id) !== activeLobbyId) {
      enterLobby(candidate);
    }
  } catch (error) {
    console.error("PWMTF social refresh failed", error);
  } finally {
    socialRefreshInFlight = false;
    if (socialRefreshQueued) {
      socialRefreshQueued = false;
      void refreshSocialState();
    }
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
    signedIn = false;
    updateSessionActivity();
    renderMatchChrome();
    return undefined;
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
  signedIn = true;
  ensureSession();
  await refreshSocialState();
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

function renderLobbyState(lobby) {
  if (lobby.status !== "waiting") {
    lobbyState.textContent = lobby.status;
    return;
  }
  // Readiness belongs to a live presence, so a reconnect clears it; the Ready
  // button follows the server's view rather than the last click.
  readyLobbyButton.disabled = lobby.self_ready === true || lobby.self_present !== true;
  if (lobby.both_ready) {
    lobbyState.textContent = "Both players ready. Starting match…";
  } else if (lobby.self_ready) {
    lobbyState.textContent = lobby.opponent_present
      ? "Ready. Waiting for your friend to ready up."
      : "Ready. Waiting for your friend to join.";
  } else if (!lobby.self_present) {
    lobbyState.textContent = "Connecting to the lobby…";
  } else {
    lobbyState.textContent = lobby.opponent_present
      ? `Your friend is here${lobby.opponent_ready ? " and ready" : ""}. Press Ready.`
      : "Waiting for your friend. Press Ready when you are set.";
  }
}

function clearLobby() {
  if (activeLobbyId !== null) {
    wasmModule?.unsubscribe_lobby();
  }
  activeLobbyId = null;
  lobbyPanel.hidden = true;
  cancelLobbyButton.hidden = true;
  readyLobbyButton.hidden = true;
}

function handleLobbyRecord(lobby) {
  if (String(lobby.lobby_id) !== activeLobbyId) {
    return;
  }
  renderLobbyState(lobby);
  if (lobby.status === "started" && isDecimalId(lobby.match_id)) {
    onMatchStarted(activeLobbyId, lobby.match_id);
  } else if (lobby.status === "cancelled") {
    clearLobby();
  }
}

async function refreshLobby() {
  const lobbyId = activeLobbyId;
  if (lobbyId === null) {
    return;
  }
  try {
    handleLobbyRecord(await apiRequest(`/api/lobbies/${lobbyId}`));
  } catch (error) {
    socialFailure(error);
  }
}

function enterLobby(lobby) {
  socialPanel.open = true;
  const nextLobbyId = String(lobby.lobby_id);
  if (!isDecimalId(nextLobbyId) || activeLobbyId === nextLobbyId) {
    return;
  }
  if (activeLobbyId !== null) {
    wasmModule?.unsubscribe_lobby();
  }
  activeLobbyId = nextLobbyId;
  lobbyPanel.hidden = false;
  lobbyLabel.textContent = `Lobby ${nextLobbyId}`;
  lobbyState.textContent = "Waiting for both players";
  cancelLobbyButton.hidden = false;
  readyLobbyButton.hidden = false;
  readyLobbyButton.disabled = false;
  socialStatus.textContent = `Joined waiting lobby ${nextLobbyId}.`;
  // The live lobby subscription is this tab's presence.
  wasmModule?.subscribe_lobby(nextLobbyId);
  void refreshLobby();
}

readyLobbyButton.addEventListener("click", async () => {
  if (activeLobbyId === null) {
    return;
  }
  readyLobbyButton.disabled = true;
  try {
    const lobby = await apiRequest(`/api/lobbies/${activeLobbyId}/ready`, { method: "POST" });
    handleLobbyRecord(lobby);
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
    lobbyState.textContent = lobby.status;
    clearLobby();
  } catch (error) {
    socialFailure(error);
  }
});

async function acceptRematch(matchId) {
  socialStatus.textContent = "Accepting rematch…";
  try {
    const match = await apiRequest(`/api/matches/${matchId}/rematch/accept`, { method: "POST" });
    socialStatus.textContent = "";
    const nextMatchId = String(match.match_id);
    // The `match-started` push may already have entered this match.
    if (nextMatchId !== currentMatchId) {
      enterMatch(nextMatchId, { push: true });
    }
  } catch (error) {
    socialFailure(error);
  }
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
  if (currentMatchId !== null) {
    void offerRematch(currentMatchId).catch(socialFailure);
  }
});

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
  const button = googleSignIn.querySelector("button");
  button.disabled = true;
  // A blocked or abandoned navigation leaves this document in place; never
  // strand the only sign-in control in a disabled state.
  window.setTimeout(() => {
    button.disabled = false;
  }, 5_000);
});

window.addEventListener("pageshow", () => {
  googleSignIn.querySelector("button").disabled = false;
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
    window.history.replaceState(window.history.state, "", matchLocation(currentMatchId));
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

const initialMatchId = query.get("match");
if (isDecimalId(initialMatchId)) {
  enterMatch(initialMatchId, { push: false });
}

try {
  if (!document.createElement("canvas").getContext("webgl2")) {
    throw new Error("WebGL 2 is unavailable");
  }
  const module = await import("./pwmtf_client.js");
  await module.default();
  module.set_session_listener(handleSessionEvent);
  wasmModule = module;
  wasmModule.set_aim_mode(Number(aimMode.value));
  applyShotControlMode();
  updateSpin();
  wasmModule.set_unlimited_aim_range(unlimitedAimRange.checked);
  // Subscriptions requested before the module loaded are applied now; the
  // transport re-sends them itself after every reconnect.
  if (currentMatchId !== null) {
    wasmModule.subscribe_match(currentMatchId);
  }
  if (activeLobbyId !== null) {
    wasmModule.subscribe_lobby(activeLobbyId);
  }
  await sessionReady;
  ensureSession();
  renderMatchChrome();
  await new Promise((resolve) => window.requestAnimationFrame(resolve));
  await new Promise((resolve) => window.requestAnimationFrame(resolve));
  shell.dataset.clientState = "ready";
  feasibility?.markClientReady();
  loading.remove();
  canvas.focus({ preventScroll: true });
} catch (error) {
  console.error("PWMTF client startup failed", error);
  shell.dataset.clientState = "failed";
  loading.remove();
  loadError.hidden = false;
  feasibility?.refresh();
}
