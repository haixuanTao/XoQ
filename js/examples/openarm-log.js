// openarm-log.js — Logging, toasts, status display

let _logEl, _toastsEl, _statusEl;
const TOAST_DURATION = 4000;
const TOAST_MAX = 5;

export function initLog(logEl, toastsEl, statusEl) {
  _logEl = logEl;
  _toastsEl = toastsEl;
  _statusEl = statusEl;
}

export function log(msg, type = "info", { toast: showToast = true } = {}) {
  const entry = document.createElement("div");
  entry.className = `log-entry log-${type}`;
  entry.textContent = `[${new Date().toLocaleTimeString()}] ${msg}`;
  _logEl.appendChild(entry);
  if (_logEl.children.length > 200) _logEl.removeChild(_logEl.firstChild);
  _logEl.scrollTop = _logEl.scrollHeight;

  if (!showToast) return;
  const toast = document.createElement("div");
  toast.className = `toast toast-${type}`;
  toast.style.setProperty('--toast-duration', `${TOAST_DURATION}ms`);
  toast.textContent = msg;
  _toastsEl.prepend(toast);
  while (_toastsEl.children.length > TOAST_MAX) _toastsEl.lastChild.remove();
  setTimeout(() => toast.remove(), TOAST_DURATION + 400);
}

export function formatBytes(b) {
  if (b < 1024) return `${b} B`;
  if (b < 1024 * 1024) return `${(b / 1024).toFixed(1)} K`;
  return `${(b / (1024 * 1024)).toFixed(1)} M`;
}

export function setStatus(s) { _statusEl.textContent = s; }
