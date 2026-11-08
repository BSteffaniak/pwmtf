// Installation only: no service worker, offline caching, or auth interception.
const panel = document.querySelector('#install-app');
const button = document.querySelector('#install-app-button');
const hint = document.querySelector('#install-app-hint');
const standalone = window.matchMedia('(display-mode: standalone)');
let pendingPrompt = null;
let prompting = false;
const ios = /iPad|iPhone|iPod/u.test(navigator.userAgent)
  || (navigator.platform === 'MacIntel' && navigator.maxTouchPoints > 1);
function refresh() {
  const installed = standalone.matches || navigator.standalone === true;
  panel.hidden = installed;
  button.hidden = !pendingPrompt || prompting;
  hint.textContent = ios
    ? 'For a borderless game, open in Safari, tap Share → Add to Home Screen (enable Open as Web App if offered), then launch the home-screen icon.'
    : 'Install from your browser’s menu, then launch the app icon to play without the address bar. An internet connection is still required.';
}
window.addEventListener('beforeinstallprompt', event => {
  event.preventDefault();
  pendingPrompt = event;
  refresh();
});
button.addEventListener('click', async () => {
  const prompt = pendingPrompt;
  if (!prompt || prompting) return;
  pendingPrompt = null;
  prompting = true;
  refresh();
  try {
    await prompt.prompt();
    await prompt.userChoice;
  } catch {
    // Browser menu instructions remain available if prompting is unsupported.
  } finally {
    prompting = false;
    refresh();
  }
});
window.addEventListener('appinstalled', () => {
  pendingPrompt = null;
  panel.hidden = true;
});
standalone.addEventListener('change', refresh);
refresh();
// The table is landscape-only. The manifest asks installed apps to launch in
// landscape; where the browser also allows a runtime lock, hold it there.
if (standalone.matches || navigator.standalone === true) {
  window.screen?.orientation?.lock?.('landscape')?.catch(() => {});
}
