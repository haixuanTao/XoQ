// openarm-chat.js — Chat MoQ pub/sub, user discovery, viewer count

import * as Moq from "@moq/lite";
import { log } from "./openarm-log.js";
import { buildConnectOpts } from "./openarm-moq.js";

const TOAST_MAX = 5;
const USER_COLORS = [
  "#a855f7", "#f97316", "#22d3ee", "#f43f5e", "#84cc16",
  "#eab308", "#ec4899", "#14b8a6", "#6366f1", "#fb923c",
];

function userColor(name) {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) | 0;
  return USER_COLORS[Math.abs(h) % USER_COLORS.length];
}

export function createChatState() {
  return {
    conn: null,
    broadcast: null,
    track: null,
    announced: null,
    subscribers: new Map(),
    running: false,
    sessionId: Math.random().toString(36).slice(2, 8),
  };
}

function showChatMessage(msg, toastsEl) {
  const toast = document.createElement("div");
  toast.className = "toast toast-chat toast-sticky";
  const time = new Date(msg.ts);
  const timeSpan = document.createElement("span");
  timeSpan.style.color = "#999";
  timeSpan.textContent = time.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", second: "2-digit" }) + " ";
  const nameSpan = document.createElement("span");
  nameSpan.style.color = userColor(msg.name);
  nameSpan.style.fontWeight = "bold";
  nameSpan.textContent = msg.name;
  if (msg.html) {
    const body = document.createElement("span");
    body.innerHTML = `: ${msg.html}`;
    toast.append(timeSpan, nameSpan, body);
  } else {
    toast.append(timeSpan, nameSpan, `: ${msg.text}`);
  }
  toastsEl.prepend(toast);
  while (toastsEl.children.length > TOAST_MAX) toastsEl.lastChild.remove();
}

export function sendChatMessage(text, chatState, getUsername, toastsEl) {
  if (!text.trim()) return;
  const msg = { name: getUsername(), text: text.trim(), ts: Date.now() };
  if (chatState.track) {
    try { chatState.track.writeJson(msg); } catch (e) {
      log(`[chat] Send error: ${e.message}`, 'error');
    }
  }
  showChatMessage(msg, toastsEl);
}

function getPublishPath(chatState, getUsername) {
  return `${getUsername()}_${chatState.sessionId}`;
}

function updateViewerCount(chatState) {
  const count = chatState.subscribers.size + (chatState.running ? 1 : 0);
  document.getElementById("viewerCount").textContent = count === 1 ? "1 viewer" : `${count} viewers`;
}

async function handleChatPublish(chatState) {
  try {
    while (chatState.running && chatState.broadcast) {
      const request = await chatState.broadcast.requested();
      if (!request) break;
      if (request.track.name === "messages") {
        chatState.track = request.track;
        log(`[chat] Publish track active`, 'success', { toast: false });
      } else {
        request.track.close(new Error("unknown track"));
      }
    }
  } catch (e) {
    if (chatState.running) log(`[chat] Publish error: ${e.message}`, 'error');
  }
}

async function subscribeToChatUser(chatState, username, toastsEl) {
  try {
    const broadcast = chatState.conn.consume(Moq.Path.from(username));
    const track = broadcast.subscribe("messages", 0);
    chatState.subscribers.set(username, { broadcast, track });
    log(`[chat] Subscribed to ${username}`, 'data', { toast: false });
    while (chatState.running) {
      const msg = await track.readJson();
      if (!msg) break;
      showChatMessage(msg, toastsEl);
    }
  } catch (e) {
    if (chatState.running) log(`[chat] ${username}: ${e.message}`, 'error');
  } finally {
    chatState.subscribers.delete(username);
  }
}

async function discoverChatUsers(chatState, getUsername, toastsEl) {
  try {
    chatState.announced = chatState.conn.announced();
    while (chatState.running) {
      const entry = await chatState.announced.next();
      if (!entry) break;
      const path = entry.path;
      if (path === Moq.Path.from(getPublishPath(chatState, getUsername))) continue;
      if (entry.active && !chatState.subscribers.has(path)) {
        log(`[chat] User joined: ${String(path).replace(/_[a-z0-9]+$/, '')}`, 'data');
        subscribeToChatUser(chatState, path, toastsEl);
        updateViewerCount(chatState);
      } else if (!entry.active) {
        chatState.subscribers.delete(path);
        log(`[chat] User left: ${String(path).replace(/_[a-z0-9]+$/, '')}`, 'info');
        updateViewerCount(chatState);
      }
    }
  } catch (e) {
    if (chatState.running) log(`[chat] Discovery error: ${e.message}`, 'error');
  }
}

export async function connectChat(config, chatState, getUsername, toastsEl) {
  const relay = config.general.relay;
  const chatPath = (config.chat.path || "").trim();
  if (!chatPath) return;

  chatState.running = true;
  const fullUrl = `${relay}/${chatPath}`;
  const connectOpts = buildConnectOpts(config);
  try {
    chatState.conn = await Moq.Connection.connect(new URL(fullUrl), connectOpts);
    log(`[chat] Connected`, 'success');

    chatState.broadcast = new Moq.Broadcast();
    chatState.conn.publish(Moq.Path.from(getPublishPath(chatState, getUsername)), chatState.broadcast);
    log(`[chat] Publishing as "${getUsername()}"`, 'data', { toast: false });
    updateViewerCount(chatState);

    // Show intro message after connection noise settles (local only)
    setTimeout(() => {
      showChatMessage({ name: "openarm", html: "Hello! This is our <b>XoQ OpenArm</b> demo running <em>in real time</em> with a <b>real robot</b>. You can make it do something by typing: <b><code>@robot pick up the yellow cube and put it in the green box</code></b>", ts: Date.now() }, toastsEl);
    }, 2000);

    handleChatPublish(chatState);
    discoverChatUsers(chatState, getUsername, toastsEl);
  } catch (e) {
    log(`[chat] ${e.message}`, 'error');
  }
}

export function disconnectChat(chatState) {
  chatState.running = false;
  for (const [, sub] of chatState.subscribers) {
    try { sub.broadcast.close(); } catch {}
  }
  chatState.subscribers.clear();
  if (chatState.announced) { try { chatState.announced.close(); } catch {} chatState.announced = null; }
  if (chatState.track) { try { chatState.track.close(); } catch {} chatState.track = null; }
  if (chatState.broadcast) { try { chatState.broadcast.close(); } catch {} chatState.broadcast = null; }
  if (chatState.conn) { try { chatState.conn.close(); } catch {} chatState.conn = null; }
  updateViewerCount(chatState);
}
