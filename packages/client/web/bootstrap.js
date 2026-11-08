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
const accountSettings = document.querySelector("#account-settings");
const accountSettingsLabel = document.querySelector("#account-settings-label");
const handleSummary = document.querySelector("#handle-summary");
const handleDisplay = document.querySelector("#handle-display");
const incomingCard = document.querySelector("#incoming");
const accountLobbies = document.querySelector("#account-lobbies");
const invitationOutput = document.querySelector("#invitation-output");
const invitationUrl = document.querySelector("#invitation-url");
const copyInvitationButton = document.querySelector("#copy-invitation");
const lobbySelf = document.querySelector("#lobby-self");
const lobbySelfState = document.querySelector("#lobby-self-state");
const lobbyOpponent = document.querySelector("#lobby-opponent");
const lobbyOpponentName = document.querySelector("#lobby-opponent-name");
const lobbyOpponentState = document.querySelector("#lobby-opponent-state");
const playSheet = document.querySelector("#play-sheet");
const settingsSheet = document.querySelector("#settings-sheet");
const sheetBackdrop = document.querySelector("#sheet-backdrop");
const openPlayButton = document.querySelector("#open-play");
const openPlayLabel = document.querySelector("#open-play-label");
const playBadge = document.querySelector("#play-badge");
const openSettingsButton = document.querySelector("#open-settings");
const matchHud = document.querySelector("#match-hud");
const hudPlayers = document.querySelector("#hud-players");
const hudTurn = document.querySelector("#hud-turn");
const rackSelf = document.querySelector("#rack-self");
const rackOpponent = document.querySelector("#rack-opponent");
const currentGameCard = document.querySelector("#current-game");
const currentGameLabel = document.querySelector("#current-game-label");
const rotateHintText = document.querySelector("#rotate-hint-text");
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
const unlimitedAimRange = document.querySelector("#unlimited-aim-range");
// Device-local presentation preferences only; never persist shot inputs or identity.
const preferencesKey = "pwmtf.presentation.v1";
function loadPresentationPreferences() {
  try {
    const raw = window.localStorage.getItem(preferencesKey);
    if (raw === null || raw.length > 256) return;
    const value = JSON.parse(raw);
    if (value === null || typeof value !== "object" || Array.isArray(value) || value.version !== 1) return;
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
      aimGuide: Number(aimMode.value),
      unlimitedRange: unlimitedAimRange.checked,
    }));
  } catch {
    // A private or storage-blocked browser still supports in-page changes.
  }
}
loadPresentationPreferences();
const spinSide = document.querySelector("#spin-side");
const spinVertical = document.querySelector("#spin-vertical");
const spinButton = document.querySelector("#spin-button");
const spinButtonDot = document.querySelector("#spin-button-dot");
const spinPopover = document.querySelector("#spin-popover");
const spinPad = document.querySelector("#spin-pad");
const spinPadDot = document.querySelector("#spin-pad-dot");
const spinLabel = document.querySelector("#spin-label");

function spinDescription(side, vertical) {
  if (side === 0 && vertical === 0) {
    return "Center hit";
  }
  const parts = [];
  if (vertical !== 0) {
    parts.push(`${vertical > 0 ? "Follow" : "Draw"} ${Math.abs(vertical)}%`);
  }
  if (side !== 0) {
    parts.push(`${side > 0 ? "Right" : "Left"} ${Math.abs(side)}%`);
  }
  return parts.join(" · ");
}

// Side is positive to the right; vertical is positive for follow (top of ball).
const updateSpin = () => {
  const side = Number(spinSide.value);
  const vertical = Number(spinVertical.value);
  wasmModule?.set_cue_spin(side, vertical);
  const left = `${50 + side * 0.38}%`;
  const top = `${50 - vertical * 0.38}%`;
  for (const dot of [spinPadDot, spinButtonDot]) {
    dot.style.left = left;
    dot.style.top = top;
  }
  const description = spinDescription(side, vertical);
  spinLabel.textContent = description;
  spinPad.setAttribute("aria-valuetext", description);
};

function setSpinFromPointer(event) {
  const rect = spinPad.getBoundingClientRect();
  let x = ((event.clientX - rect.left) / rect.width) * 2 - 1;
  let y = ((event.clientY - rect.top) / rect.height) * 2 - 1;
  const length = Math.hypot(x, y);
  if (length > 1) {
    x /= length;
    y /= length;
  }
  // Snap near the middle so a center hit is easy to select by touch.
  const snap = (value) => (Math.abs(value) < 0.12 ? 0 : Math.round(value * 100));
  spinSide.value = String(snap(x));
  spinVertical.value = String(snap(-y));
  updateSpin();
}

spinPad.addEventListener("pointerdown", (event) => {
  spinPad.setPointerCapture(event.pointerId);
  setSpinFromPointer(event);
});
spinPad.addEventListener("pointermove", (event) => {
  if (spinPad.hasPointerCapture(event.pointerId)) {
    setSpinFromPointer(event);
  }
});
spinPad.addEventListener("keydown", (event) => {
  const step = { ArrowLeft: [-10, 0], ArrowRight: [10, 0], ArrowUp: [0, 10], ArrowDown: [0, -10] }[event.key];
  if (step === undefined) {
    return;
  }
  event.preventDefault();
  const clamp = (value) => Math.max(-100, Math.min(100, value));
  spinSide.value = String(clamp(Number(spinSide.value) + step[0]));
  spinVertical.value = String(clamp(Number(spinVertical.value) + step[1]));
  updateSpin();
});
document.querySelector("#reset-spin").addEventListener("click", () => {
  spinSide.value = "0";
  spinVertical.value = "0";
  updateSpin();
});
function setSpinPopover(open) {
  spinPopover.hidden = !open;
  spinButton.setAttribute("aria-expanded", String(open));
}
spinButton.addEventListener("click", () => setSpinPopover(spinPopover.hidden));
document.addEventListener("pointerdown", (event) => {
  if (!spinPopover.hidden && !spinPopover.contains(event.target) && !spinButton.contains(event.target)) {
    setSpinPopover(false);
  }
});
updateSpin();
const fullscreenButton = document.querySelector("#fullscreen-table");
fullscreenButton.hidden = !document.documentElement.requestFullscreen;
fullscreenButton.addEventListener("click", async () => {
  try {
    await document.documentElement.requestFullscreen();
    closeSheets();
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

// App update state (see "App updates" below).
let updateAvailable = false;
let reloadWhenSafe = false;
let lastUpdateCheckAt = 0;
let pointerHeld = false;

// Opponent handles keyed by match id, learned from the lobby list.
const matchOpponents = new Map();
let pendingIncoming = { challenges: 0, rematches: 0 };
let lastToastedRejection = null;
let opponentLookupFor = null;
// The table as of the last settled shot, so racks and summaries update when
// the balls stop rather than the moment the shot is accepted.
let settledMatch = null;
let lastSummarizedShot = null;

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
    case "hint":
      showHint(detail);
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
  closeSheets();
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
  turnTimer.dataset.low = String(remainingSeconds <= 10);
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
  renderMatchStatus();
  renderMatchHud();
}

function renderMatchStatus() {
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
    if (lobby.opponent_handle && isDecimalId(lobby.match_id ?? "")) {
      matchOpponents.set(lobby.match_id, lobby.opponent_handle);
    }
  }
  const visible = lobbies.filter(
    (lobby) =>
      lobby.status === "waiting" ||
      (lobby.status === "started" && lobby.match_in_progress !== false),
  );
  for (const lobby of visible) {
    const started = lobby.status === "started" && isDecimalId(lobby.match_id ?? "");
    const current = started && lobby.match_id === currentMatchId;
    const open = listButton(current ? "Playing" : started ? "Resume" : "Open", "secondary", () => {
      if (started) {
        enterMatch(lobby.match_id, { push: true });
      } else {
        enterLobby(lobby);
      }
    });
    open.disabled = current || (!started && String(lobby.lobby_id) === activeLobbyId);
    lobbyList.append(
      listItem(
        `vs ${opponentName(lobby.opponent_handle)}`,
        started ? "Game in progress" : "Waiting in lobby",
        open,
      ),
    );
  }
  accountLobbies.hidden = visible.length === 0;
  renderMatchHud();
}

function opponentName(handle) {
  return handle ? `@${handle}` : "your friend";
}

function listItem(title, subtitle, ...actions) {
  const row = document.createElement("div");
  row.className = "list-item";
  const text = document.createElement("span");
  const strong = document.createElement("strong");
  strong.textContent = title;
  const small = document.createElement("small");
  small.textContent = subtitle;
  text.append(strong, small);
  row.append(text, ...actions);
  return row;
}

function listButton(label, className, onClick) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = className;
  button.textContent = label;
  button.addEventListener("click", () => onClick(button));
  return button;
}

function renderPlayBadge() {
  const count = pendingIncoming.challenges + pendingIncoming.rematches;
  playBadge.hidden = count === 0;
  playBadge.textContent = String(count);
  incomingCard.hidden = count === 0;
}

const BALL_COLORS = {
  1: "#f2c230", 2: "#2457c5", 3: "#d43a2f", 4: "#6b3fa0",
  5: "#f07f22", 6: "#1f8a4c", 7: "#7c2230", 8: "#151515",
};
const GROUP_BALLS = {
  solids: [1, 2, 3, 4, 5, 6, 7],
  stripes: [9, 10, 11, 12, 13, 14, 15],
};

function pocketName(pocket) {
  return String(pocket).replace("-", " ");
}

function ballList(numbers) {
  const items = numbers.map(String);
  return items.length <= 1 ? items.join("") : `${items.slice(0, -1).join(", ")} and ${items.at(-1)}`;
}

/// What the local player should do now, appended to "Your turn".
function objectiveHint(match) {
  if (match.on_break) {
    return " · break";
  }
  if (match.ball_in_hand) {
    return " · ball in hand: drag the cue ball";
  }
  if (match.my_target === "eight") {
    return match.called_pocket
      ? ` · 8-ball → ${pocketName(match.called_pocket)}`
      : " · call a pocket for the 8";
  }
  if (match.my_target === "solids" || match.my_target === "stripes") {
    return ` · ${match.my_target}`;
  }
  return " · open table";
}

function miniBall(number, pocketed) {
  const ball = document.createElement("i");
  ball.className = `mini-ball${number > 8 ? " stripe" : ""}${number === 8 ? " eight" : ""}${pocketed ? " pocketed" : ""}`;
  ball.style.setProperty("--ball", BALL_COLORS[number > 8 ? number - 8 : number]);
  ball.title = pocketed ? `${number} (pocketed)` : String(number);
  return ball;
}

function renderRack(rack, target, pocketed) {
  rack.replaceChildren();
  if (target === "eight") {
    rack.append(miniBall(8, false));
  } else if (Object.hasOwn(GROUP_BALLS, target)) {
    rack.append(...GROUP_BALLS[target].map((number) => miniBall(number, pocketed.has(number))));
  }
  rack.hidden = rack.childElementCount === 0;
}

function renderRacks(match) {
  const live = match !== null && currentMatchId !== null && match.winner === 0;
  const pocketed = new Set(live && Array.isArray(match.pocketed) ? match.pocketed : []);
  renderRack(rackSelf, live ? match.my_target : null, pocketed);
  renderRack(rackOpponent, live ? match.opponent_target : null, pocketed);
}

const FOUL_TEXT = {
  scratch: "Scratch",
  "no-contact": "Missed every ball",
  "wrong-first-contact": "Hit the wrong ball first",
  "no-rail": "No ball reached a rail",
};

/// One line describing a finished shot, shown once when the balls settle.
function summarizeShot(match, name) {
  const shot = match.last_shot;
  if (shot === null || typeof shot !== "object" || shot.revision === lastSummarizedShot) {
    return;
  }
  lastSummarizedShot = shot.revision;
  if (match.winner !== 0) {
    return;
  }
  const mine = shot.shooter === match.seat;
  const who = mine ? "You" : name;
  const parts = [];
  if (shot.reracked) {
    parts.push("8-ball on the break: re-rack");
  } else if (shot.pocketed.length > 0) {
    parts.push(`${who} pocketed ${ballList(shot.pocketed)}`);
  }
  if (shot.assigned_group === "solids" || shot.assigned_group === "stripes") {
    const other = shot.assigned_group === "solids" ? "stripes" : "solids";
    parts.push(mine ? `You're ${shot.assigned_group}!` : `${name} is ${shot.assigned_group}, you're ${other}`);
  }
  const foul = shot.fouls.find((value) => Object.hasOwn(FOUL_TEXT, value));
  if (foul) {
    parts.push(`${FOUL_TEXT[foul]}!`);
  }
  if (shot.ball_in_hand) {
    parts.push(mine ? `${name} has ball in hand` : "You have ball in hand");
  } else if (shot.kept_turn) {
    parts.push(mine ? "Still your turn" : `${name} shoots again`);
  } else if (parts.length === 0) {
    parts.push(mine ? `No ball in · ${name}'s turn` : `${name} missed · your turn`);
  }
  socialStatus.textContent = parts.join(" · ");
}

function showHint(detail) {
  if (detail?.reason === "call-pocket") {
    socialStatus.textContent = "Tap the pocket you're aiming the 8-ball at, then shoot.";
  }
}

function shortRejection(reason) {
  return reason === "wrong-turn" ? "Not your turn" : "Move not accepted";
}

function renderMatchHud() {
  applyUpdateIfSafe();
  const inMatch = currentMatchId !== null;
  currentGameCard.hidden = !inMatch;
  openPlayLabel.textContent = inMatch ? "Friends" : "Play friends";
  if (!inMatch) {
    rotateHintText.textContent = "Turn your phone sideways to practice.";
    hudPlayers.textContent = "";
    hudTurn.textContent = "Practice table";
    matchHud.dataset.tone = "";
    settledMatch = null;
    renderRacks(null);
    return;
  }
  const opponent = matchOpponents.get(currentMatchId);
  if (opponent === undefined && signedIn && opponentLookupFor !== currentMatchId) {
    // A just-started match may not be in the last lobby list yet.
    opponentLookupFor = currentMatchId;
    void refreshSocialState();
  }
  const name = opponent ? `@${opponent}` : "Opponent";
  hudPlayers.textContent = `You vs ${name}`;
  currentGameLabel.textContent = `Playing ${opponentName(opponent)}.`;
  rotateHintText.textContent = `Your game vs ${opponentName(opponent)} is on. Turn your phone sideways to play.`;
  const match = currentMatch;
  if (settledMatch !== null && settledMatch.match_id !== currentMatchId) {
    settledMatch = null;
  }
  let text;
  let tone = "";
  if (matchStatus.dataset.denied === "true") {
    text = "Not your match";
    tone = "warn";
  } else if (!signedIn) {
    text = "Sign in to play";
  } else if (match === null || connectionStatus !== "ready" || !sessionActive) {
    text = connectionStatus === "reconnecting" ? "Reconnecting…" : "Connecting…";
    tone = connectionStatus === "reconnecting" ? "warn" : "";
  } else if (match.winner !== 0) {
    text = match.winner === match.seat ? "You won" : "You lost";
  } else if (lastRejection !== null) {
    text = shortRejection(lastRejection);
    tone = "warn";
  } else if (match.pending) {
    text = "Sending shot…";
  } else if (match.playing) {
    text = "Shot in progress";
  } else if (match.active_player === match.seat) {
    text = `Your turn${objectiveHint(match)}`;
    tone = "you";
  } else {
    text = `${name}'s turn${match.opponent_target === "eight" ? " · on the 8" : ""}`;
  }
  hudTurn.textContent = text;
  matchHud.dataset.tone = tone;
  if (match !== null && !match.playing) {
    settledMatch = match;
    summarizeShot(match, name);
  }
  renderRacks(settledMatch);
  if (lastRejection !== null && lastRejection !== lastToastedRejection) {
    socialStatus.textContent = REJECTION_MESSAGES[lastRejection];
  }
  lastToastedRejection = lastRejection;
}

// ---------- Sheets ----------

function closeSheets() {
  playSheet.hidden = true;
  settingsSheet.hidden = true;
  sheetBackdrop.hidden = true;
  openPlayButton.setAttribute("aria-expanded", "false");
  openSettingsButton.setAttribute("aria-expanded", "false");
}

function openSheet(name) {
  const play = name === "play";
  // Never leave a half-drawn shot behind a sheet.
  wasmModule?.interrupt_match_gesture();
  setSpinPopover(false);
  playSheet.hidden = !play;
  settingsSheet.hidden = play;
  sheetBackdrop.hidden = false;
  openPlayButton.setAttribute("aria-expanded", String(play));
  openSettingsButton.setAttribute("aria-expanded", String(!play));
}

openPlayButton.addEventListener("click", () => openSheet("play"));
openSettingsButton.addEventListener("click", () => openSheet("settings"));
document.querySelector("#open-settings-inline").addEventListener("click", () => openSheet("settings"));
sheetBackdrop.addEventListener("click", closeSheets);
for (const button of document.querySelectorAll("[data-close-sheet]")) {
  button.addEventListener("click", closeSheets);
}
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape") {
    closeSheets();
    setSpinPopover(false);
  }
});

// Toasts clear themselves however their text was set.
let toastTimer = null;
new MutationObserver(() => {
  window.clearTimeout(toastTimer);
  if (socialStatus.textContent !== "") {
    toastTimer = window.setTimeout(() => {
      socialStatus.textContent = "";
    }, 3_500);
  }
}).observe(socialStatus, { childList: true, characterData: true, subtree: true });
socialStatus.addEventListener("click", () => {
  socialStatus.textContent = "";
});

// ---------- Social state ----------

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

function renderSignedOut() {
  signedIn = false;
  accountPanel.hidden = false;
  googleSignIn.hidden = !runtimeAuth.google_login;
  developmentSignIn.hidden = !runtimeAuth.development_login;
  socialPanel.hidden = true;
  accountSettings.hidden = true;
  pendingIncoming = { challenges: 0, rematches: 0 };
  renderPlayBadge();
}

function renderHandle(handle) {
  const hasHandle = typeof handle === "string" && handle !== "";
  handleSummary.hidden = !hasHandle;
  handleForm.hidden = hasHandle;
  handleDisplay.textContent = hasHandle ? `@${handle}` : "";
  accountSettingsLabel.textContent = hasHandle ? `Signed in as @${handle}` : "Signed in · no player name yet";
  // Challenges and invitations need a public name first.
  document.querySelector("#start-game").hidden = !hasHandle;
}

async function refreshSession() {
  const response = await fetch("/api/session", {
    credentials: "same-origin",
    headers: { Accept: "application/json" },
  });
  if (response.status === 401) {
    accountLabel.textContent =
      "Sign in to challenge friends online. Practice works without an account.";
    renderSignedOut();
    updateSessionActivity();
    renderMatchChrome();
    return undefined;
  }
  if (!response.ok) {
    throw new Error(`session request failed with ${response.status}`);
  }
  const session = await response.json();
  accountPanel.hidden = true;
  socialPanel.hidden = false;
  accountSettings.hidden = false;
  renderHandle(session.handle);
  signedIn = true;
  ensureSession();
  await refreshSocialState();
  renderMatchChrome();
  return session;
}

class ApiError extends Error {
  constructor(status, message) {
    super(message);
    this.status = status;
  }
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
      throw new ApiError(response.status, message || `request failed with ${response.status}`);
    }
    return response.status === 204 ? null : response.json();
  });
}

const HANDLE_PATTERN = /^[a-z][a-z0-9_]{2,23}$/u;
const HANDLE_RULES = "Use 3–24 lowercase letters, numbers or _, starting with a letter.";

// Turns server status codes into something a player can act on.
function friendlyError(error, specific = {}) {
  const status = error instanceof ApiError ? error.status : 0;
  if (Object.hasOwn(specific, status)) {
    return specific[status];
  }
  switch (status) {
    case 0:
      return navigator.onLine ? "Could not reach the server. Try again." : "You are offline.";
    case 401:
      return "Your sign-in expired. Sign in again.";
    case 403:
      return "That is not allowed.";
    case 404:
      return "That no longer exists. Refresh and try again.";
    case 409:
      return "That conflicts with the current state. Refresh and try again.";
    default:
      return status >= 500 ? "The server had a problem. Try again in a moment." : "Something went wrong. Try again.";
  }
}

function socialFailure(error, specific) {
  console.error("PWMTF social operation failed", error);
  socialStatus.textContent = friendlyError(error, specific);
}

function normalizeHandleInput(input) {
  input.addEventListener("input", () => {
    const normalized = input.value.toLowerCase().replace(/[^a-z0-9_]/gu, "");
    if (normalized !== input.value) {
      input.value = normalized;
    }
  });
}
normalizeHandleInput(handleInput);
normalizeHandleInput(challengeHandle);
normalizeHandleInput(developmentUsername);

// ---------- Lobby ----------

function describePresence(present, ready) {
  if (ready) {
    return ["ready", "Ready"];
  }
  return present ? ["present", "Here"] : ["away", "Not here yet"];
}

function renderLobbyState(lobby) {
  const opponent = opponentName(lobby.opponent_handle);
  lobbyLabel.textContent = lobby.opponent_handle ? `Game with @${lobby.opponent_handle}` : "Game lobby";
  lobbyOpponentName.textContent = lobby.opponent_handle ? `@${lobby.opponent_handle}` : "Friend";
  if (lobby.status !== "waiting") {
    lobbyState.textContent = lobby.status === "cancelled" ? "This lobby was closed." : "Starting…";
    return;
  }
  const [selfState, selfText] = describePresence(lobby.self_present, lobby.self_ready);
  const [opponentState, opponentText] = describePresence(lobby.opponent_present, lobby.opponent_ready);
  lobbySelf.dataset.state = selfState;
  lobbySelfState.textContent = lobby.self_present ? selfText : "Connecting…";
  lobbyOpponent.dataset.state = opponentState;
  lobbyOpponentState.textContent = opponentText;
  // Readiness belongs to a live presence, so a reconnect clears it; the Ready
  // button follows the server's view rather than the last click.
  readyLobbyButton.disabled = lobby.self_ready === true || lobby.self_present !== true;
  readyLobbyButton.textContent = lobby.self_ready ? "Ready ✓" : "I'm ready";
  if (lobby.both_ready) {
    lobbyState.textContent = "Both ready. Starting the game…";
  } else if (lobby.self_ready) {
    lobbyState.textContent = lobby.opponent_present
      ? `Waiting for ${opponent} to press ready.`
      : `Waiting for ${opponent} to open the game.`;
  } else if (!lobby.self_present) {
    lobbyState.textContent = "Connecting to the lobby…";
  } else {
    lobbyState.textContent = lobby.opponent_present
      ? `${opponent} is here${lobby.opponent_ready ? " and ready" : ""}. Press ready to start.`
      : `Press ready. The game starts when ${opponent} is here and ready too.`;
  }
}

function clearLobby() {
  if (activeLobbyId !== null) {
    wasmModule?.unsubscribe_lobby();
  }
  activeLobbyId = null;
  lobbyPanel.hidden = true;
}

function handleLobbyRecord(lobby) {
  if (String(lobby.lobby_id) !== activeLobbyId) {
    return;
  }
  renderLobbyState(lobby);
  if (lobby.status === "started" && isDecimalId(lobby.match_id)) {
    if (lobby.opponent_handle) {
      matchOpponents.set(lobby.match_id, lobby.opponent_handle);
    }
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
  const nextLobbyId = String(lobby.lobby_id);
  if (!isDecimalId(nextLobbyId) || activeLobbyId === nextLobbyId) {
    return;
  }
  if (activeLobbyId !== null) {
    wasmModule?.unsubscribe_lobby();
  }
  activeLobbyId = nextLobbyId;
  lobbyPanel.hidden = false;
  lobbySelf.dataset.state = "away";
  lobbyOpponent.dataset.state = "away";
  lobbySelfState.textContent = "Connecting…";
  lobbyOpponentState.textContent = "";
  lobbyState.textContent = "Connecting to the lobby…";
  readyLobbyButton.disabled = true;
  readyLobbyButton.textContent = "I'm ready";
  if (lobby.opponent_handle !== undefined) {
    renderLobbyState({ ...lobby, status: "waiting" });
  }
  openSheet("play");
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
    socialFailure(error, { 409: "Still connecting to the lobby. Try again in a moment." });
  }
});

cancelLobbyButton.addEventListener("click", async () => {
  if (activeLobbyId === null) {
    return;
  }
  if (!window.confirm("Leave this lobby? Your friend will need a new invitation.")) {
    return;
  }
  try {
    await apiRequest(`/api/lobbies/${activeLobbyId}`, { method: "DELETE" });
    clearLobby();
    socialStatus.textContent = "Lobby closed.";
    void refreshSocialState();
  } catch (error) {
    socialFailure(error);
  }
});

// ---------- Rematches ----------

async function acceptRematch(matchId, fromHandle, button) {
  button.disabled = true;
  try {
    const match = await apiRequest(`/api/matches/${matchId}/rematch/accept`, { method: "POST" });
    const nextMatchId = String(match.match_id);
    if (fromHandle) {
      matchOpponents.set(nextMatchId, fromHandle);
    }
    // The `match-started` push may already have entered this match.
    if (nextMatchId !== currentMatchId) {
      enterMatch(nextMatchId, { push: true });
    }
  } catch (error) {
    button.disabled = false;
    socialFailure(error, { 409: "That rematch is no longer available." });
  }
}

function renderRematches(rematches) {
  rematchList.replaceChildren();
  pendingIncoming.rematches = rematches.length;
  for (const rematch of rematches) {
    rematchList.append(
      listItem(
        rematch.from_handle ? `@${rematch.from_handle}` : "Your opponent",
        "Wants a rematch",
        listButton("Accept", "primary", (button) => void acceptRematch(rematch.previous_match_id, rematch.from_handle, button)),
      ),
    );
  }
  renderPlayBadge();
}

async function offerRematch(matchId) {
  offerRematchButton.disabled = true;
  try {
    await apiRequest(`/api/matches/${matchId}/rematch`, { method: "POST" });
    offerRematchButton.textContent = "Rematch offered";
    socialStatus.textContent = "Rematch offered. It starts when your opponent accepts.";
  } catch (error) {
    offerRematchButton.disabled = false;
    throw error;
  }
}

offerRematchButton.addEventListener("click", () => {
  if (currentMatchId !== null) {
    void offerRematch(currentMatchId).catch((error) =>
      socialFailure(error, { 409: "A rematch was already offered for this game." }),
    );
  }
});

// ---------- Challenges ----------

async function acceptChallenge(challengeId, button) {
  if (button.disabled) {
    return;
  }
  button.disabled = true;
  try {
    const lobby = await apiRequest(`/api/challenges/${challengeId}/accept`, { method: "POST" });
    enterLobby(lobby);
    await refreshSocialState();
  } catch (error) {
    socialFailure(error, { 404: "That challenge is no longer available." });
  } finally {
    button.disabled = false;
  }
}

function renderChallenges(challenges) {
  challengeList.replaceChildren();
  pendingIncoming.challenges = challenges.length;
  for (const challenge of challenges) {
    challengeList.append(
      listItem(
        `@${challenge.from_handle}`,
        "Challenged you",
        listButton("Accept", "primary", (button) => void acceptChallenge(challenge.challenge_id, button)),
      ),
    );
  }
  renderPlayBadge();
}

handleForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const handle = handleInput.value.trim().toLowerCase();
  if (!HANDLE_PATTERN.test(handle)) {
    socialStatus.textContent = HANDLE_RULES;
    handleInput.focus();
    return;
  }
  const button = handleForm.querySelector("button");
  button.disabled = true;
  try {
    const profile = await apiRequest("/api/profile/handle", {
      method: "PUT",
      body: JSON.stringify({ handle }),
    });
    renderHandle(profile.handle);
    socialStatus.textContent = `You're @${profile.handle}. Invite a friend to play!`;
  } catch (error) {
    socialFailure(error, {
      400: HANDLE_RULES,
      409: `@${handle} is taken, or your account already has a name.`,
    });
  } finally {
    button.disabled = false;
  }
});

challengeForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const handle = challengeHandle.value.trim().toLowerCase();
  if (!HANDLE_PATTERN.test(handle)) {
    socialStatus.textContent = `Player names: ${HANDLE_RULES.toLowerCase()}`;
    challengeHandle.focus();
    return;
  }
  if (handleDisplay.textContent === `@${handle}`) {
    socialStatus.textContent = "You can't challenge yourself. Invite a friend with a link instead.";
    return;
  }
  const button = challengeForm.querySelector("button");
  button.disabled = true;
  try {
    await apiRequest("/api/challenges", {
      method: "POST",
      body: JSON.stringify({ handle }),
    });
    challengeHandle.value = "";
    socialStatus.textContent = `Challenge sent to @${handle}. The lobby opens when they accept.`;
  } catch (error) {
    socialFailure(error, {
      400: "You can't challenge yourself.",
      404: `No player named @${handle}. Check the spelling.`,
      409: `You already challenged @${handle}.`,
    });
  } finally {
    button.disabled = false;
  }
});

// ---------- Invitations ----------

async function copyText(value) {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(value);
    return true;
  }
  invitationUrl.select();
  return false;
}

createInvitationButton.addEventListener("click", async () => {
  createInvitationButton.disabled = true;
  try {
    const invitation = await apiRequest("/api/invitations", { method: "POST" });
    const url = invitation.invitation_url;
    invitationUrl.value = url;
    invitationOutput.hidden = false;
    if (navigator.share) {
      try {
        await navigator.share({ title: "Pool with More Than Friends", text: "Play 8-ball with me!", url });
        socialStatus.textContent = "Invitation sent. The lobby opens when your friend joins.";
        return;
      } catch (error) {
        if (error?.name === "AbortError") {
          return;
        }
      }
    }
    socialStatus.textContent = (await copyText(url).catch(() => false))
      ? "Link copied. Send it to a friend; it works once."
      : "Copy the link below and send it to a friend.";
  } catch (error) {
    socialFailure(error);
  } finally {
    createInvitationButton.disabled = false;
  }
});

copyInvitationButton.addEventListener("click", async () => {
  socialStatus.textContent = (await copyText(invitationUrl.value).catch(() => false))
    ? "Link copied."
    : "Select the link and copy it.";
});

const PENDING_INVITE_KEY = "pwmtf.pending-invite";

async function redeemInvitationFromUrl() {
  const url = new URL(window.location.href);
  let token = url.searchParams.get("invite");
  if (token !== null) {
    url.searchParams.delete("invite");
    window.history.replaceState(window.history.state, "", url);
  } else {
    try {
      token = window.sessionStorage.getItem(PENDING_INVITE_KEY);
    } catch {
      token = null;
    }
  }
  if (token === null || token.length > 256) {
    return;
  }
  if (!signedIn) {
    // Keep the invitation across the Google sign-in round trip.
    try {
      window.sessionStorage.setItem(PENDING_INVITE_KEY, token);
    } catch {
      // Without storage the friend can open the link again after signing in.
    }
    accountLabel.textContent = "Your friend invited you to a game. Sign in to join.";
    openSheet("play");
    return;
  }
  try {
    window.sessionStorage.removeItem(PENDING_INVITE_KEY);
  } catch {
    // Ignore unavailable storage.
  }
  try {
    const lobby = await apiRequest("/api/invitations/redeem", {
      method: "POST",
      body: JSON.stringify({ token }),
    });
    enterLobby(lobby);
  } catch (error) {
    socialFailure(error, {
      400: "That invitation link is not valid, or it is your own link. Send it to a friend.",
      409: "That invitation link has expired or was already used. Ask for a new one.",
      404: "That invitation link is not valid. Ask for a new one.",
    });
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
    closeSheets();
    await refreshSession();
  } catch (error) {
    console.error("PWMTF sign out failed", error);
    socialStatus.textContent = "Sign out failed. Try again.";
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
    renderSignedOut();
    accountLabel.textContent = "Could not check your account. Practice still works; reload to try again.";
    return undefined;
  });

// ---------- App updates ----------
// An installed app is often resumed from memory rather than reloaded, so it
// can keep running an old build indefinitely. Compare the running build with
// the one the page now references, and reload when it is safe: immediately
// on returning to the app, otherwise after the user taps Refresh or the
// next time the app comes back.
const UPDATE_CHECK_INTERVAL_MS = 10 * 60 * 1000;
const UPDATE_RESUME_MIN_GAP_MS = 15 * 1000;
const updateBanner = document.querySelector("#update-banner");

canvas.addEventListener("pointerdown", () => {
  pointerHeld = true;
});
for (const name of ["pointerup", "pointercancel"]) {
  window.addEventListener(name, () => {
    pointerHeld = false;
    applyUpdateIfSafe();
  });
}

async function latestBuildId() {
  const response = await fetch(`/?update-check=${Date.now()}`, {
    cache: "no-store",
    credentials: "same-origin",
    headers: { Accept: "text/html" },
  });
  if (!response.ok) {
    return null;
  }
  return /bootstrap\.js\?v=([A-Za-z0-9._-]{1,200})"/u.exec(await response.text())?.[1] ?? null;
}

/// Reloading now would not lose a shot in flight or interrupt a gesture.
function updateIsSafe() {
  return !pointerHeld && (currentMatch === null || (!currentMatch.pending && !currentMatch.playing));
}

const UPDATE_ATTEMPT_KEY = "pwmtf.update-attempt";
let latestKnownBuild = null;

function applyUpdateIfSafe() {
  if (!updateAvailable || !reloadWhenSafe || !updateIsSafe()) {
    return;
  }
  // Reload automatically at most once per new build, so a deploy whose page
  // and scripts disagree can never trap the app in a reload loop.
  try {
    if (window.sessionStorage.getItem(UPDATE_ATTEMPT_KEY) === latestKnownBuild) {
      reloadWhenSafe = false;
      return;
    }
    window.sessionStorage.setItem(UPDATE_ATTEMPT_KEY, latestKnownBuild);
  } catch {
    // Without storage the reload still happens; the banner covers a repeat.
  }
  window.location.reload();
}

async function checkForUpdate(resumed) {
  // Builds without an injected identity (local tooling) cannot compare.
  if (candidateBuildId.startsWith("__")) {
    return;
  }
  const now = Date.now();
  if (now - lastUpdateCheckAt < (resumed ? UPDATE_RESUME_MIN_GAP_MS : UPDATE_CHECK_INTERVAL_MS / 2)) {
    return;
  }
  lastUpdateCheckAt = now;
  const latest = await latestBuildId().catch(() => null);
  if (latest === null || latest === candidateBuildId) {
    return;
  }
  latestKnownBuild = latest;
  updateAvailable = true;
  updateBanner.hidden = false;
  if (resumed) {
    // The user just came back: update before they start playing again.
    reloadWhenSafe = true;
    applyUpdateIfSafe();
  }
}

document.querySelector("#update-now").addEventListener("click", () => {
  // An explicit tap always reloads, whatever happened on earlier attempts.
  try {
    window.sessionStorage.removeItem(UPDATE_ATTEMPT_KEY);
  } catch {
    // Storage may be unavailable; reloading still works.
  }
  reloadWhenSafe = true;
  applyUpdateIfSafe();
});
document.addEventListener("visibilitychange", () => {
  if (document.visibilityState === "visible") {
    void checkForUpdate(true);
  } else if (updateAvailable) {
    // Apply the update the next time the app comes back.
    reloadWhenSafe = true;
  }
});
window.addEventListener("pageshow", (event) => {
  if (event.persisted) {
    void checkForUpdate(true);
  }
});
window.setInterval(() => {
  if (document.visibilityState === "visible") {
    void checkForUpdate(false);
  }
}, UPDATE_CHECK_INTERVAL_MS);

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
