// openarm-moq.js — MoQ connection logic for arms, cameras, and RealSense

import * as Moq from "@moq/lite";
import { JOINTS, parseAllCanFrames, parseDamiaoState, canIdToJointIdx } from "./openarm-can.js";
import { log } from "./openarm-log.js";
import { MsePlayer, DepthDecoder, HAS_WEBCODECS, stripTimestamp,
  rsConcat, rsEncodeVarInt, rsEncodeClientSetup, rsEncodeSubscribe,
  RsBufReader, RsStreamReader } from "./openarm-depth.js";

// ─── Helpers ─────────────────────────────────────────
export function buildConnectOpts(config) {
  const certHash = (config.general.certHash || "").trim();
  const wsDelay = /firefox/i.test(navigator.userAgent) ? 0 : 2000;
  const opts = { websocket: { delay: wsDelay } };
  if (certHash) {
    const hex = certHash.replace(/[^0-9a-fA-F]/g, '');
    const hashBytes = new Uint8Array(hex.length / 2);
    for (let i = 0; i < hashBytes.length; i++) hashBytes[i] = parseInt(hex.substr(i * 2, 2), 16);
    opts.webtransport = {
      serverCertificateHashes: [{ algorithm: "sha-256", value: hashBytes.buffer }],
    };
    log(`Using cert hash: ${hex.slice(0, 16)}...`, "data", { toast: false });
  }
  return opts;
}

export function withTimeout(promise, ms) {
  return Promise.race([
    promise,
    new Promise((_, rej) => setTimeout(() => rej(new Error('stale (no data for ' + (ms/1000) + 's)')), ms)),
  ]);
}
export const STALE_MS = 10000;
export const RECONNECT_DELAY = 300;

// Strip /state or /commands suffix to get base path
function basePath(path) {
  return path.replace(/\/(state|commands)$/, "");
}

// ─── Arm subscription (raw WebTransport) ─────────────
async function subscribeArmOnce(config, appState, label, path, jointState) {
  const relay = config.general.relay;
  const fullUrl = `${relay}/${basePath(path)}/state`;
  log(`[${label}] Connecting to ${fullUrl}...`, 'info', { toast: false });

  // 1. Connect raw WebTransport
  const wt = new WebTransport(fullUrl);
  await Promise.race([
    wt.ready,
    new Promise((_, rej) => setTimeout(() => rej(new Error(`[${label}] WT timeout`)), 8000)),
  ]);
  log(`[${label}] Connected`, "success");

  // 2. Setup handshake (bidi stream type 0)
  const setup = await wt.createBidirectionalStream();
  const sw = setup.writable.getWriter();
  const sr = new RsStreamReader(setup.readable.getReader());
  await sw.write(rsConcat(rsEncodeVarInt(0), rsEncodeClientSetup()));
  await sr.readMessage(); // ServerSetup (ignore contents)

  // 3. Subscribe to "can" track (bidi stream type 2)
  const sub = await wt.createBidirectionalStream();
  const subW = sub.writable.getWriter();
  const subR = new RsStreamReader(sub.readable.getReader());
  await subW.write(rsConcat(rsEncodeVarInt(2), rsEncodeSubscribe(0, "", "can", 0)));
  await subR.readMessage(); // SubscribeOk

  log(`[${label}] Subscribed to 'can' track`, "success", { toast: false });

  // 4. Read incoming uni streams (groups with frames)
  const uniReader = wt.incomingUnidirectionalStreams.getReader();
  try {
    while (appState.running) {
      const { value: stream, done } = await Promise.race([
        uniReader.read(),
        new Promise((_, rej) => setTimeout(() => rej(new Error(`stale (no data for ${STALE_MS/1000}s)`)), STALE_MS)),
      ]);
      if (done) break;

      // Parse group: DataType(0) + header(subId, seq) + frame(size-prefixed data)
      const r = new RsStreamReader(stream.getReader());
      if (await r.readVarInt() !== 0) continue; // skip non-object types
      const hdr = await r.readMessage();
      const br = new RsBufReader(hdr);
      br.readVarInt(); // subscribe_id
      br.readVarInt(); // sequence

      // Read all frames in this group
      try {
        while (true) {
          const frame = await r.readMessage();
          const bytes = new Uint8Array(frame);
          appState.bytesTotal += bytes.length;
          const canFrames = parseAllCanFrames(bytes);
          appState.frameCount += canFrames.length;
          appState.fpsCounter += canFrames.length;
          for (const parsed of canFrames) {
            const jointIdx = canIdToJointIdx(parsed.canId);
            if (jointIdx < 0) continue;
            const state = parseDamiaoState(parsed.data);
            if (!state) continue;
            jointState[jointIdx].targetAngle = state.qRad;
            jointState[jointIdx].velocity = state.vel;
            jointState[jointIdx].torque = state.tau;
            jointState[jointIdx].tempMos = state.tempMos;
            jointState[jointIdx].tempRotor = state.tempRotor;
            jointState[jointIdx].updated = true;
          }
        }
      } catch { /* stream ended (FIN) — next group */ }
    }
  } finally {
    try { wt.close(); } catch {}
  }
}

export async function subscribeArm(config, appState, label, path, jointState) {
  let lastError = null;
  while (appState.running) {
    try {
      await subscribeArmOnce(config, appState, label, path, jointState);
      if (!appState.running) break;
      lastError = null;
      log(`[${label}] Stream ended`, 'info');
    } catch (e) {
      if (!appState.running) break;
      if (lastError) log(`[${label}] ${e.message}`, 'error');
      lastError = e;
    }
    if (!appState.running) break;
    await new Promise(r => setTimeout(r, RECONNECT_DELAY));
  }
}

// ─── Plain camera connection (video only, no depth) ──
async function connectCameraOnce(config, cam, path, videoEl, label) {
  const relay = config.general.relay;
  const fullUrl = `${relay}/${path}`;

  cam.colorPlayer = new MsePlayer(videoEl, label);

  const connectOpts = buildConnectOpts(config);
  log(`[${label}] Connecting to ${fullUrl}...`, 'info', { toast: false });
  cam.conn = await Moq.Connection.connect(new URL(fullUrl), connectOpts);
  log(`[${label}] Connected`, 'success');

  const broadcast = cam.conn.consume(Moq.Path.from(""));
  const videoTrack = broadcast.subscribe("video", 0);
  log(`[${label}] Subscribed to video track`, 'success', { toast: false });

  while (cam.running) {
    const group = await withTimeout(videoTrack.nextGroup(), STALE_MS);
    if (!group) { log(`[${label}] video track ended`); break; }
    while (cam.running) {
      const frame = await withTimeout(group.readFrame(), STALE_MS);
      if (!frame) break;
      cam.colorPlayer.onData(new Uint8Array(frame));
    }
  }
}

function cleanupCamera(cam) {
  if (cam.conn) { try { cam.conn.close(); } catch {} cam.conn = null; }
  if (cam.colorPlayer) { cam.colorPlayer.destroy(); cam.colorPlayer = null; }
}

async function connectSingleCamera(config, cam, path, videoEl, label) {
  if (!path) return;
  cam.running = true;
  let lastError = null;
  while (cam.running) {
    try {
      await connectCameraOnce(config, cam, path, videoEl, label);
      if (!cam.running) break;
      lastError = null;
      log(`[${label}] Stream ended`, 'info');
    } catch (e) {
      if (!cam.running) break;
      if (lastError) log(`[${label}] ${e.message}`, 'error');
      lastError = e;
    }
    cleanupCamera(cam);
    if (!cam.running) break;
    await new Promise(r => setTimeout(r, RECONNECT_DELAY));
  }
}

export async function connectCameras(config, camState, camVideoEls) {
  const promises = [];
  config.cameras.forEach((camCfg, i) => {
    const path = (camCfg.path || "").trim();
    if (camCfg.enabled !== false && path && camState[i]) {
      promises.push(connectSingleCamera(config, camState[i], path, camVideoEls[i], camCfg.label || ("Cam " + (i+1))));
    }
  });
  if (promises.length) await Promise.all(promises);
}

export function disconnectCameras(camState) {
  for (const cam of camState) {
    cam.running = false;
    cleanupCamera(cam);
  }
}

// ─── RealSense connection (video + depth) ────────────
async function connectRealSenseOnce(config, appState, cam, path, videoEl, label) {
  const relay = config.general.relay;
  const fullUrl = `${relay}/${path}`;

  cam.colorPlayer = new MsePlayer(videoEl, label + " Color");
  cam.depthDecoder = new DepthDecoder();

  const connectOpts = buildConnectOpts(config);
  log(`[${label}] Connecting to ${fullUrl}...`, 'info', { toast: false });
  cam.conn = await Moq.Connection.connect(new URL(fullUrl), connectOpts);
  log(`[${label}] Connected`, 'success');

  const broadcast = cam.conn.consume(Moq.Path.from(""));
  const videoTrack = broadcast.subscribe("video", 0);
  const trackNames = ['video'];

  async function readTrack(track, handler, name, hasTimestamp = true) {
    while (cam.running) {
      const group = await withTimeout(track.nextGroup(), STALE_MS);
      if (!group) { log(`[${label}] ${name} track ended`); break; }
      while (cam.running) {
        const frame = await withTimeout(group.readFrame(), STALE_MS);
        if (!frame) break;
        const bytes = new Uint8Array(frame);
        handler(hasTimestamp ? stripTimestamp(bytes, appState.latency) : bytes);
      }
    }
  }

  const promises = [readTrack(videoTrack, d => { appState.videoFps.count++; cam.colorPlayer.onData(d); }, 'video')];

  if (HAS_WEBCODECS) {
    const depthTrack = broadcast.subscribe("depth", 0);
    promises.push(readTrack(depthTrack, d => cam.depthDecoder.onData(d), 'depth'));
    trackNames.push('depth');

    // Subscribe to metadata track (intrinsics JSON, sent on keyframes)
    const metadataTrack = broadcast.subscribe("metadata", 0);
    promises.push(readTrack(metadataTrack, d => {
      try {
        const json = new TextDecoder().decode(d);
        const meta = JSON.parse(json);
        if (meta.fx && meta.fy) {
          cam.intrinsics = meta;
          if (!cam._intrinsicsLogged) {
            log(`[${label}] Intrinsics: ${meta.width}x${meta.height} fx=${meta.fx} fy=${meta.fy} ppx=${meta.ppx} ppy=${meta.ppy}`, 'data', { toast: false });
            cam._intrinsicsLogged = true;
          }
        }
      } catch (e) { console.warn('metadata parse error:', e); }
    }, 'metadata', false));
    trackNames.push('metadata');
  }

  log(`[${label}] Subscribed to ${trackNames.join(' + ')} tracks`, 'success', { toast: false });
  await Promise.all(promises);
}

function cleanupRealSense(cam) {
  if (cam.conn) { try { cam.conn.close(); } catch {} cam.conn = null; }
  if (cam.colorPlayer) { cam.colorPlayer.destroy(); cam.colorPlayer = null; }
  if (cam.depthDecoder) { cam.depthDecoder.destroy(); cam.depthDecoder = null; }
  cam.intrinsics = null;
  cam._intrinsicsLogged = false;
  cam._frustumUpdated = false;
}

async function connectSingleRealSense(config, appState, cam, path, videoEl, label) {
  if (!path) return;
  cam.running = true;
  let lastError = null;
  while (cam.running) {
    try {
      await connectRealSenseOnce(config, appState, cam, path, videoEl, label);
      if (!cam.running) break;
      lastError = null;
      log(`[${label}] Stream ended`, 'info');
    } catch (e) {
      if (!cam.running) break;
      if (lastError) log(`[${label}] ${e.message}`, 'error');
      lastError = e;
    }
    cleanupRealSense(cam);
    if (!cam.running) break;
    await new Promise(r => setTimeout(r, RECONNECT_DELAY));
  }
}

export async function connectRealSense(config, appState, rsCams, rsVideoEls) {
  const promises = [];
  config.realsense.forEach((rsCfg, i) => {
    const path = (rsCfg.path || "").trim();
    if (rsCfg.enabled !== false && path && rsCams[i]) {
      promises.push(connectSingleRealSense(config, appState, rsCams[i], path, rsVideoEls[i], rsCfg.label || ("RS " + (i+1))));
    }
  });
  if (promises.length) await Promise.all(promises);
}

export function disconnectRealSense(rsCams, pointClouds) {
  for (const cam of rsCams) {
    cam.running = false;
    cleanupRealSense(cam);
  }
  pointClouds.forEach(pc => pc.geometry.setDrawRange(0, 0));
}

// ─── Motor Commands ─────────────────────────────────

const MIT_ENABLE  = new Uint8Array([0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFC]);
const MIT_DISABLE = new Uint8Array([0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFD]);

export function encodeMitZeroTorque() {
  const p = 0x8000, v = 0x800, kp = 0, kd = 0, t = 0x800;
  return new Uint8Array([
    p >> 8, p & 0xFF,
    v >> 4,
    ((v & 0xF) << 4) | (kp >> 8),
    kp & 0xFF,
    kd >> 4,
    ((kd & 0xF) << 4) | (t >> 8),
    t & 0xFF,
  ]);
}

export function encodeCanFrame(canId, data) {
  const buf = new Uint8Array(6 + data.length);
  buf[0] = 0x00;
  buf[1] = canId & 0xFF;
  buf[2] = (canId >> 8) & 0xFF;
  buf[3] = (canId >> 16) & 0xFF;
  buf[4] = (canId >> 24) & 0xFF;
  buf[5] = data.length;
  buf.set(data, 6);
  return buf;
}

// Connect command channel for a single arm (@moq/lite publisher)
export async function ensureCmdConnection(config, armId, armPath, cmdState) {
  const cs = cmdState[armId];
  if (cs.conn && cs.track) return; // already connected

  const relay = config.general.relay;
  const cmdUrl = `${relay}/${basePath(armPath)}/commands`;
  const connectOpts = buildConnectOpts(config);

  log(`[${armId}] Connecting commands to ${cmdUrl}...`, 'info', { toast: false });
  cs.conn = await Promise.race([
    Moq.Connection.connect(new URL(cmdUrl), connectOpts),
    new Promise((_, rej) => setTimeout(() => rej(new Error(`[${armId}] Command connection timeout`)), 8000)),
  ]);
  cs.broadcast = new Moq.Broadcast();
  cs.conn.publish(Moq.Path.from(""), cs.broadcast);

  log(`[${armId}] Waiting for CAN server to subscribe (30s)...`);
  const request = await Promise.race([
    cs.broadcast.requested(),
    new Promise((_, rej) => setTimeout(() => rej(new Error(`[${armId}] No subscriber after 30s — is moq-can-server running?`)), 30000)),
  ]);
  if (!request) { log(`[${armId}] Command broadcast closed`, "error"); return; }
  cs.track = request.track;
  cs.group = null;

  // Monitor for connection close and clear state
  cs.conn.closed.then(() => {
    log(`[${armId}] Command connection closed`, "info", { toast: false });
    cs.conn = null;
    cs.track = null;
    cs.broadcast = null;
    cs.group = null;
  }).catch(() => {
    cs.conn = null;
    cs.track = null;
    cs.broadcast = null;
    cs.group = null;
  });

  log(`[${armId}] Command track active`, "success");
}

function disconnectCmdArm(armId, cmdState) {
  const cs = cmdState[armId];
  if (!cs) return;
  cs.group = null;
  if (cs.track) { try { cs.track.close(); } catch {} cs.track = null; }
  if (cs.broadcast) { try { cs.broadcast.close(); } catch {} cs.broadcast = null; }
  if (cs.conn) { try { cs.conn.close(); } catch {} cs.conn = null; }
}

function sendCanFrameToAll(cs, data) {
  if (!cs.track) return;
  for (let motorId = 1; motorId <= 8; motorId++) {
    const frame = encodeCanFrame(motorId, data);
    const group = cs.track.appendGroup();
    group.writeFrame(frame);
    group.close();
  }
}

// Enable MIT mode (torque on) for a single arm
export async function enableArmTorque(config, armId, armPath, cmdState) {
  await ensureCmdConnection(config, armId, armPath, cmdState);
  const cs = cmdState[armId];
  if (!cs.track) throw new Error(`[${armId}] No command track`);
  sendCanFrameToAll(cs, MIT_ENABLE);
  log(`[${armId}] Torque enabled (MIT mode)`, "success");
}

// Disable MIT mode (torque off) for a single arm
export async function disableArmTorque(config, armId, armPath, cmdState) {
  const cs = cmdState[armId];
  if (!cs || !cs.track) { log(`[${armId}] No command connection to disable torque`, "info"); return; }
  sendCanFrameToAll(cs, MIT_DISABLE);
  log(`[${armId}] Torque disabled`, "success");
  disconnectCmdArm(armId, cmdState);
}

// Start query loop for a single arm pair
export async function startQueryLoopForPair(config, pairIdx, armConfigs, cmdState) {
  const pair = config.armPairs[pairIdx];
  if (!pair) return null;

  const pairArms = armConfigs.filter(a => a.pairIdx === pairIdx);

  const promises = [];
  pairArms.forEach(arm => {
    const path = (arm.path || "").trim();
    if (path) promises.push(ensureCmdConnection(config, arm.id, path, cmdState));
  });
  await Promise.all(promises);

  const mitCmd = encodeMitZeroTorque();
  let motorIdx = 0;
  let armIdx = 0;

  const activeTracks = [];
  pairArms.forEach(arm => {
    if (cmdState[arm.id] && cmdState[arm.id].conn && cmdState[arm.id].track) activeTracks.push(cmdState[arm.id]);
  });
  if (activeTracks.length === 0) {
    log(`No command tracks connected for ${pair.label}`, "error");
    return null;
  }

  const rateHz = pair.queryRate || 200;
  const intervalMs = Math.max(1, Math.round(1000 / rateHz));

  const interval = setInterval(() => {
    const cs = activeTracks[armIdx % activeTracks.length];
    if (!cs.track) return;
    const canId = motorIdx + 1;
    const frame = encodeCanFrame(canId, mitCmd);
    try {
      const group = cs.track.appendGroup();
      group.writeFrame(frame);
      group.close();
    } catch { /* connection closed, skip */ }
    motorIdx = (motorIdx + 1) % 8;
    if (motorIdx === 0) armIdx++;
  }, intervalMs);

  log(`Query started for ${pair.label} at ${rateHz}Hz (${activeTracks.length} arm${activeTracks.length > 1 ? "s" : ""})`, "success");
  return interval;
}

// Stop query loop for a single arm pair (does NOT close cmd connections — torque may still be active)
export function stopQueryLoopForPair(queryInterval) {
  if (queryInterval) clearInterval(queryInterval);
}

// Stop all query loops
export function stopAllQueryLoops(queryIntervals) {
  for (const interval of Object.values(queryIntervals)) {
    if (interval) clearInterval(interval);
  }
}

// Disconnect all command connections for all arms
export function disconnectAllCmdArms(armConfigs, cmdState) {
  for (const arm of armConfigs) {
    disconnectCmdArm(arm.id, cmdState);
  }
}
