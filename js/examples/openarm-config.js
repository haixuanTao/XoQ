// openarm-config.js — Config loading/saving/migration (shared by openarm.html + settings.html)

export const LS_KEY = "openarm.config";
export const LS_PREFIX = "openarm.";

export function defaultConfig() {
  return {
    version: 3,
    general: { relay: "https://cdn.1ms.ai", certHash: "" },
    armPairs: [
      { id: "pair2", enabled: true, label: "Baguette Arm",
        leftPath: "anon/7e58263812ba/xoq-can-can0", rightPath: "anon/7e58263812ba/xoq-can-can1",
        position: {x:1,y:0,z:1}, rotation: {roll:0,pitch:0,yaw:0},
        queryRate: 1, autoQuery: true },
      { id: "pair3", enabled: true, label: "champagne-arm",
        leftPath: "anon/a13af1d39199/xoq-can-can0", rightPath: "anon/a13af1d39199/xoq-can-can1",
        position: {x:0,y:0,z:0}, rotation: {roll:0,pitch:0,yaw:0},
        queryRate: 1, autoQuery: true },
    ],
    cameras: [],
    realsense: [
      { id: "rs3", enabled: true, label: "baguette-realsense",
        path: "anon/7e58263812ba/realsense-243222073892",
        position: {x:0.79,y:0.81,z:1.23}, rotation: {roll:90,pitch:-45,yaw:0},
        showColor: true, pointSize: 2 },
      { id: "rs2", enabled: true, label: "champagne-realsense",
        path: "anon/a13af1d39199/realsense-233522074606",
        position: {x:-0.26,y:0.71,z:0.3}, rotation: {roll:90,pitch:-45,yaw:0},
        showColor: true, pointSize: 2 },
    ],
    audio: { enabled: false, path: "anon/openarm-audio" },
    chat: { enabled: false, path: "anon/openarm-chat", username: "anon" },
  };
}

export function migrateLegacy() {
  const g = (k, fb) => localStorage.getItem(LS_PREFIX + k) || fb;
  const cfg = defaultConfig();
  cfg.general.relay = g("relay", cfg.general.relay);
  cfg.general.certHash = g("certHash", "");
  cfg.armPairs[0].queryRate = parseInt(g("queryRate", "200")) || 200;
  cfg.armPairs[0].leftPath = g("left", "");
  cfg.armPairs[0].rightPath = g("right", "");
  cfg.realsense[0].path = g("depth", "anon/realsense");
  cfg.realsense[0].pointSize = parseFloat(g("ptSize", "2")) || 2;
  cfg.realsense[0].position = { x: parseFloat(g("camX","-0.33")), y: parseFloat(g("camY","0.84")), z: parseFloat(g("camZ","0.29")) };
  cfg.realsense[0].rotation = { roll: parseFloat(g("camRoll","90")), pitch: parseFloat(g("camPitch","-222")), yaw: parseFloat(g("camYaw","0")) };
  cfg.realsense[1].path = g("depth2", "");
  cfg.realsense[1].position = { x: parseFloat(g("cam2X","-0.33")), y: parseFloat(g("cam2Y","0.84")), z: parseFloat(g("cam2Z","0.29")) };
  cfg.realsense[1].rotation = { roll: parseFloat(g("cam2Roll","90")), pitch: parseFloat(g("cam2Pitch","-225")), yaw: parseFloat(g("cam2Yaw","0")) };
  cfg.audio.path = g("audioPath", "anon/openarm-audio");
  cfg.chat.path = g("chatPath", "anon/openarm-chat");
  cfg.chat.username = g("chatUsername", "anon");
  return cfg;
}

// Migrate old arms[] format to armPairs[]
export function migrateArmsToArmPairs(cfg) {
  if (cfg.arms && !cfg.armPairs) {
    cfg.armPairs = [];
    for (let i = 0; i < cfg.arms.length; i += 2) {
      const left = cfg.arms[i];
      const right = cfg.arms[i + 1];
      cfg.armPairs.push({
        id: "pair" + (Math.floor(i / 2) + 1),
        label: "Arm Pair " + (Math.floor(i / 2) + 1),
        leftPath: left ? left.path || "" : "",
        rightPath: right ? right.path || "" : "",
        position: {x:0, y:0, z:0},
        rotation: {roll:0, pitch:0, yaw:0},
        queryRate: 200,
      });
    }
    delete cfg.arms;
  }
  return cfg;
}

// Migrate v2 config (cameras[]) to v3 (cameras[] + realsense[])
export function migrateV2ToV3(cfg) {
  if (cfg.version >= 3) return cfg;
  // All v2 "cameras" were realsense-capable — move them to realsense[]
  const oldPointSize = (cfg.general && cfg.general.pointSize) || 2;
  const oldQueryRate = (cfg.general && cfg.general.queryRate) || 200;
  cfg.realsense = (cfg.cameras || []).map((cam, i) => ({
    id: cam.id || ("rs" + (i + 1)),
    label: cam.label || ("RealSense " + (i + 1)),
    path: cam.path || "",
    position: cam.position || {x:-0.33, y:0.84, z:0.29},
    rotation: cam.rotation || {roll:90, pitch:-222, yaw:0},
    showColor: cam.showColor !== false,
    pointSize: oldPointSize,
  }));
  cfg.cameras = [];
  // Move queryRate to arm pairs
  if (cfg.armPairs) {
    cfg.armPairs.forEach(p => { if (!p.queryRate) p.queryRate = oldQueryRate; });
  }
  // Clean up general
  if (cfg.general) {
    delete cfg.general.queryRate;
    delete cfg.general.pointSize;
  }
  // Clean up audio.enabled (no longer in config)
  if (cfg.audio) delete cfg.audio.enabled;
  cfg.version = 3;
  return cfg;
}

export function loadConfig(opts = {}) {
  let cfg;
  const raw = localStorage.getItem(LS_KEY);
  if (raw) {
    try { cfg = migrateArmsToArmPairs(JSON.parse(raw)); } catch { cfg = defaultConfig(); }
  } else if (localStorage.getItem(LS_PREFIX + "relay")) {
    cfg = migrateLegacy();
    localStorage.setItem(LS_KEY, JSON.stringify(cfg));
  } else {
    cfg = defaultConfig();
  }
  // Apply v2 → v3 migration
  cfg = migrateV2ToV3(cfg);
  // Ensure armPairs have position/rotation/queryRate
  if (cfg.armPairs) {
    cfg.armPairs.forEach(p => {
      if (p.enabled === undefined) p.enabled = true;
      if (!p.position) p.position = {x:0, y:0, z:0};
      if (!p.rotation) p.rotation = {roll:0, pitch:0, yaw:0};
      if (!p.queryRate) p.queryRate = 100;
      if (p.autoQuery === undefined) p.autoQuery = false;
      if (p.leftPath) p.leftPath = p.leftPath.replace(/\/state$/, "");
      if (p.rightPath) p.rightPath = p.rightPath.replace(/\/state$/, "");
    });
  }
  if (cfg.realsense) {
    cfg.realsense.forEach(r => {
      if (r.enabled === undefined) r.enabled = true;
      if (!r.pointSize) r.pointSize = 2;
    });
  }
  if (!cfg.cameras) cfg.cameras = [];
  cfg.cameras.forEach(c => { if (c.enabled === undefined) c.enabled = true; });
  // Ensure tags arrays exist
  if (cfg.armPairs) cfg.armPairs.forEach(p => { if (!p.tags) p.tags = []; });
  if (cfg.realsense) cfg.realsense.forEach(r => { if (!r.tags) r.tags = []; });
  cfg.cameras.forEach(c => { if (!c.tags) c.tags = []; });
  if (cfg.audio && cfg.audio.enabled === undefined) cfg.audio.enabled = true;
  if (cfg.chat && cfg.chat.enabled === undefined) cfg.chat.enabled = true;
  // URL param overrides (openarm.html only)
  if (opts.urlOverrides) {
    const params = new URLSearchParams(location.search);
    if (params.has("relay")) cfg.general.relay = params.get("relay") || "";
    if (params.has("certHash")) cfg.general.certHash = params.get("certHash") || "";
    if (params.has("left") && cfg.armPairs[0]) cfg.armPairs[0].leftPath = params.get("left") || "";
    if (params.has("path") && cfg.armPairs[0] && !params.has("left")) cfg.armPairs[0].leftPath = params.get("path") || "";
    if (params.has("right") && cfg.armPairs[0]) cfg.armPairs[0].rightPath = params.get("right") || "";
    if (params.has("depth") && cfg.realsense[0]) cfg.realsense[0].path = params.get("depth") || "";
    if (params.has("depth2") && cfg.realsense[1]) cfg.realsense[1].path = params.get("depth2") || "";
  }
  return cfg;
}

export function saveConfig(cfg) {
  localStorage.setItem(LS_KEY, JSON.stringify(cfg));
}

export function flattenArmPairs(config) {
  const armConfigs = [];
  config.armPairs.forEach((pair, pairIdx) => {
    const lbl = pair.label || ("Arm Pair " + (pairIdx + 1));
    const pos = pair.position || {x:0, y:0, z:0};
    const rot = pair.rotation || {roll:0, pitch:0, yaw:0};
    const enabled = pair.enabled !== false;
    armConfigs.push({ id: pair.id + "_left",  label: lbl + " Left",  path: pair.leftPath  || "", pairIdx, position: pos, rotation: rot, enabled });
    armConfigs.push({ id: pair.id + "_right", label: lbl + " Right", path: pair.rightPath || "", pairIdx, position: pos, rotation: rot, enabled });
  });
  return armConfigs;
}
