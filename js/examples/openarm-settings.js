// openarm-settings.js — Reusable settings UI (shared by settings.html + openarm.html)

import { defaultConfig, saveConfig } from "./openarm-config.js";

function esc(s) { return (s || "").replace(/"/g, "&quot;").replace(/</g, "&lt;"); }

const TAG_COLORS = ["#ff6b6b","#ffa502","#2ed573","#1e90ff","#a855f7","#ff6348","#00d4ff","#eccc68"];
const TAG_PRESETS = [];

function tagColor(tag) {
  let h = 0;
  for (let i = 0; i < tag.length; i++) h = ((h << 5) - h + tag.charCodeAt(i)) | 0;
  return TAG_COLORS[Math.abs(h) % TAG_COLORS.length];
}

function renderTagsField(card, tags, onChange) {
  let wrap = card.querySelector(".tags-field");
  if (!wrap) {
    wrap = document.createElement("div");
    wrap.className = "tags-field";
    wrap.innerHTML = `<label>Tags</label><div class="tags-wrap"></div>`;
    // Insert after the Label field
    const labelField = card.querySelector(".field");
    if (labelField && labelField.nextSibling) {
      labelField.parentNode.insertBefore(wrap, labelField.nextSibling);
    } else {
      card.appendChild(wrap);
    }
  }
  card.dataset.tags = JSON.stringify(tags);
  const inner = wrap.querySelector(".tags-wrap");
  inner.innerHTML = "";
  tags.forEach((t, i) => {
    const pill = document.createElement("span");
    pill.className = "tag-pill";
    pill.style.background = tagColor(t);
    pill.textContent = t;
    const x = document.createElement("span");
    x.className = "tag-x";
    x.textContent = "\u00d7";
    x.addEventListener("click", () => { tags.splice(i, 1); renderTagsField(card, tags, onChange); onChange(); });
    pill.appendChild(x);
    inner.appendChild(pill);
  });
  // Preset pills (only those not already added)
  TAG_PRESETS.filter(p => !tags.includes(p)).forEach(p => {
    const pill = document.createElement("span");
    pill.className = "tag-pill tag-preset";
    pill.style.color = tagColor(p);
    pill.textContent = p;
    pill.addEventListener("click", () => { tags.push(p); renderTagsField(card, tags, onChange); onChange(); });
    inner.appendChild(pill);
  });
  // Custom input
  const inp = document.createElement("input");
  inp.type = "text"; inp.className = "tag-input"; inp.placeholder = "add...";
  inp.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && inp.value.trim()) {
      const v = inp.value.trim().toLowerCase();
      if (!tags.includes(v)) { tags.push(v); renderTagsField(card, tags, onChange); onChange(); }
      else inp.value = "";
    }
  });
  inner.appendChild(inp);
}

/**
 * Initialize the settings UI inside a container element.
 * @param {HTMLElement} containerEl — DOM element to render into
 * @param {object} config — mutable config object (loaded from localStorage)
 * @param {object} callbacks
 *   .onSave(config)          — called on every change
 *   .onLiveUpdate(type, idx) — called for position/rotation/label/pointSize
 *   .onStructuralChange()    — called for path/add/remove/enabled changes
 *   .primaryButtonLabel      — text for the primary button (e.g. "Save & Open Visualizer")
 *   .onPrimaryAction()       — called when the primary button is clicked
 * @returns {{ renderAll, destroy }}
 */
export function initSettings(containerEl, config, callbacks = {}) {
  // Build skeleton HTML
  containerEl.innerHTML = `
    <div class="section">
      <h2>General</h2>
      <div class="field">
        <label>Relay URL</label>
        <input type="text" data-s="relay" />
      </div>
      <div class="field">
        <label>Cert Hash</label>
        <input type="text" data-s="certHash" placeholder="sha256 hex (optional)" />
      </div>
    </div>
    <div class="section">
      <h2>Arm Pairs</h2>
      <div data-s="armPairsList"></div>
      <button class="btn-add" data-s="addArmPair">+ Add Arm Pair</button>
    </div>
    <div class="section">
      <h2>RealSense</h2>
      <div data-s="realsenseList"></div>
      <button class="btn-add" data-s="addRealSense">+ Add RealSense</button>
    </div>
    <div class="section">
      <h2>Cameras</h2>
      <div data-s="camerasList"></div>
      <button class="btn-add" data-s="addCamera">+ Add Camera</button>
    </div>
    <div class="section">
      <h2><input type="checkbox" data-s="audioEnabled" title="Enable/disable audio" /> Audio</h2>
      <div class="field">
        <label>Path</label>
        <input type="text" data-s="audioPath" />
      </div>
    </div>
    <div class="section">
      <h2><input type="checkbox" data-s="chatEnabled" title="Enable/disable chat" /> Chat</h2>
      <div class="field">
        <label>Path</label>
        <input type="text" data-s="chatPath" />
      </div>
      <div class="field">
        <label>Username</label>
        <input type="text" data-s="chatUsername" />
      </div>
    </div>
    <div class="footer">
      <button class="btn-primary" data-s="primaryBtn">${esc(callbacks.primaryButtonLabel || "Save")}</button>
      <button class="btn-secondary" data-s="resetDefaults">Reset to Defaults</button>
      <button class="btn-secondary" data-s="exportJson">Export JSON</button>
      <button class="btn-secondary" data-s="importJson">Import JSON</button>
      <button class="btn-secondary" data-s="importMachine">Import Machine</button>
      <button class="btn-secondary" data-s="importClipboard">Paste Machine</button>
    </div>
  `;

  // Hidden file inputs
  const importFile = document.createElement("input");
  importFile.type = "file"; importFile.accept = ".json"; importFile.style.display = "none";
  containerEl.appendChild(importFile);

  const importMachineFile = document.createElement("input");
  importMachineFile.type = "file"; importMachineFile.accept = ".json"; importMachineFile.style.display = "none";
  containerEl.appendChild(importMachineFile);

  // Helper to query within our container only
  const q = (sel) => containerEl.querySelector(`[data-s="${sel}"]`);

  // ─── Read from UI ─────────────────────────────────
  function readFromUI() {
    config.general.relay = q("relay").value;
    config.general.certHash = q("certHash").value;

    config.armPairs = [];
    q("armPairsList").querySelectorAll(".item-card").forEach((card) => {
      config.armPairs.push({
        id: card.dataset.id,
        enabled: card.querySelector(".pair-enabled").checked,
        label: card.querySelector(".pair-label").value,
        tags: JSON.parse(card.dataset.tags || "[]"),
        leftPath: card.querySelector(".pair-left").value,
        rightPath: card.querySelector(".pair-right").value,
        position: {
          x: parseFloat(card.querySelector(".pair-px").value) || 0,
          y: parseFloat(card.querySelector(".pair-py").value) || 0,
          z: parseFloat(card.querySelector(".pair-pz").value) || 0,
        },
        rotation: {
          roll: parseFloat(card.querySelector(".pair-rr").value) || 0,
          pitch: parseFloat(card.querySelector(".pair-rp").value) || 0,
          yaw: parseFloat(card.querySelector(".pair-ry").value) || 0,
        },
        queryRate: parseInt(card.querySelector(".pair-qrate").value) || 100,
        autoQuery: card.querySelector(".pair-autoquery").checked,
      });
    });

    config.realsense = [];
    q("realsenseList").querySelectorAll(".item-card").forEach((card) => {
      config.realsense.push({
        id: card.dataset.id,
        enabled: card.querySelector(".rs-enabled").checked,
        label: card.querySelector(".rs-label").value,
        tags: JSON.parse(card.dataset.tags || "[]"),
        path: card.querySelector(".rs-path").value,
        position: {
          x: parseFloat(card.querySelector(".rs-px").value) || 0,
          y: parseFloat(card.querySelector(".rs-py").value) || 0,
          z: parseFloat(card.querySelector(".rs-pz").value) || 0,
        },
        rotation: {
          roll: parseFloat(card.querySelector(".rs-rr").value) || 0,
          pitch: parseFloat(card.querySelector(".rs-rp").value) || 0,
          yaw: parseFloat(card.querySelector(".rs-ry").value) || 0,
        },
        showColor: card.querySelector(".rs-showColor").checked,
        pointSize: parseFloat(card.querySelector(".rs-ptsize").value) || 2,
      });
    });

    config.cameras = [];
    q("camerasList").querySelectorAll(".item-card").forEach((card) => {
      config.cameras.push({
        id: card.dataset.id,
        enabled: card.querySelector(".cam-enabled").checked,
        label: card.querySelector(".cam-label").value,
        tags: JSON.parse(card.dataset.tags || "[]"),
        path: card.querySelector(".cam-path").value,
      });
    });

    config.audio.enabled = q("audioEnabled").checked;
    config.audio.path = q("audioPath").value;
    config.chat.enabled = q("chatEnabled").checked;
    config.chat.path = q("chatPath").value;
    config.chat.username = q("chatUsername").value;
  }

  function autoSave(liveField) {
    readFromUI();
    saveConfig(config);
    if (callbacks.onSave) callbacks.onSave(config);
    if (liveField && callbacks.onLiveUpdate) callbacks.onLiveUpdate(liveField.type, liveField.index);
  }

  function structuralChange() {
    readFromUI();
    saveConfig(config);
    if (callbacks.onSave) callbacks.onSave(config);
    if (callbacks.onStructuralChange) callbacks.onStructuralChange();
  }

  // ─── Card renderers ──────────────────────────────
  function renderArmPairCard(pair, idx) {
    const p = pair.position || {x:0,y:0,z:0};
    const r = pair.rotation || {roll:0,pitch:0,yaw:0};
    const card = document.createElement("div");
    card.className = "item-card";
    card.dataset.id = pair.id;
    card.innerHTML = `
      <div class="item-header">
        <input type="checkbox" class="pair-enabled" ${pair.enabled !== false ? "checked" : ""} title="Enable/disable this arm pair" />
        <span>Arm Pair ${idx + 1}</span>
        <button class="btn-remove" data-action="remove">Remove</button>
      </div>
      <div class="field">
        <label>Label</label>
        <input type="text" class="pair-label" value="${esc(pair.label)}" />
      </div>
      <div class="pair-row">
        <label>Left Path</label>
        <input type="text" class="pair-left" value="${esc(pair.leftPath)}" placeholder="e.g. anon/xoq-can-can0" />
      </div>
      <div class="pair-row">
        <label>Right Path</label>
        <input type="text" class="pair-right" value="${esc(pair.rightPath)}" placeholder="e.g. anon/xoq-can-can1" />
      </div>
      <div class="field">
        <label>Position</label>
        <div class="field-group">
          <label>X</label><input type="number" class="pair-px" value="${p.x}" step="0.01" />
          <label>Y</label><input type="number" class="pair-py" value="${p.y}" step="0.01" />
          <label>Z</label><input type="number" class="pair-pz" value="${p.z}" step="0.01" />
        </div>
      </div>
      <div class="field">
        <label>Rotation</label>
        <div class="field-group">
          <label>Roll</label><input type="number" class="pair-rr" value="${r.roll}" step="1" />
          <label>Pitch</label><input type="number" class="pair-rp" value="${r.pitch}" step="1" />
          <label>Yaw</label><input type="number" class="pair-ry" value="${r.yaw}" step="1" />
        </div>
      </div>
      <div class="field">
        <label>Query Rate</label>
        <input type="number" class="pair-qrate" value="${pair.queryRate || 100}" min="10" max="1000" step="10" />
        <span style="font-size:0.75rem; color:#666;">Hz</span>
      </div>
      <div class="field">
        <label>Auto Query</label>
        <input type="checkbox" class="pair-autoquery" ${pair.autoQuery ? "checked" : ""} />
        <span style="font-size:0.75rem; color:#666;">Send zero-torque queries on connect</span>
      </div>
    `;
    // Tags
    renderTagsField(card, pair.tags || [], () => autoSave({ type: 'armPair', index: idx }));

    card.querySelector('[data-action="remove"]').addEventListener('click', () => {
      if (config.armPairs.length <= 1) return;
      card.remove();
      structuralChange();
      renderArmPairs();
    });
    // Live-update fields
    card.querySelectorAll('.pair-px, .pair-py, .pair-pz, .pair-rr, .pair-rp, .pair-ry').forEach(el => {
      el.addEventListener('input', () => autoSave({ type: 'armPair', index: idx }));
    });
    card.querySelector('.pair-label').addEventListener('input', () => autoSave({ type: 'armPair', index: idx }));
    // Structural fields
    card.querySelector('.pair-enabled').addEventListener('change', () => structuralChange());
    card.querySelectorAll('.pair-left, .pair-right').forEach(el => {
      el.addEventListener('input', () => structuralChange());
    });
    card.querySelectorAll('.pair-qrate, .pair-autoquery').forEach(el => {
      el.addEventListener('input', () => autoSave(null));
      el.addEventListener('change', () => autoSave(null));
    });
    return card;
  }

  function renderRealSenseCard(rs, idx) {
    const card = document.createElement("div");
    card.className = "item-card";
    card.dataset.id = rs.id;
    card.innerHTML = `
      <div class="item-header">
        <input type="checkbox" class="rs-enabled" ${rs.enabled !== false ? "checked" : ""} title="Enable/disable this RealSense" />
        <span>RealSense ${idx + 1}</span>
        <button class="btn-remove" data-action="remove">Remove</button>
      </div>
      <div class="field">
        <label>Label</label>
        <input type="text" class="rs-label" value="${esc(rs.label)}" />
      </div>
      <div class="field">
        <label>MoQ Path</label>
        <input type="text" class="rs-path" value="${esc(rs.path)}" placeholder="e.g. anon/realsense" />
      </div>
      <div class="field">
        <label>Position</label>
        <div class="field-group">
          <label>X</label><input type="number" class="rs-px" value="${rs.position.x}" step="0.01" />
          <label>Y</label><input type="number" class="rs-py" value="${rs.position.y}" step="0.01" />
          <label>Z</label><input type="number" class="rs-pz" value="${rs.position.z}" step="0.01" />
        </div>
      </div>
      <div class="field">
        <label>Rotation</label>
        <div class="field-group">
          <label>Roll</label><input type="number" class="rs-rr" value="${rs.rotation.roll}" step="1" />
          <label>Pitch</label><input type="number" class="rs-rp" value="${rs.rotation.pitch}" step="1" />
          <label>Yaw</label><input type="number" class="rs-ry" value="${rs.rotation.yaw}" step="1" />
        </div>
      </div>
      <div class="field">
        <label>Show Color</label>
        <input type="checkbox" class="rs-showColor" ${rs.showColor ? "checked" : ""} />
      </div>
      <div class="field">
        <label>Point Size</label>
        <input type="number" class="rs-ptsize" value="${rs.pointSize || 2}" min="0.5" max="8" step="0.5" />
      </div>
    `;
    // Tags
    renderTagsField(card, rs.tags || [], () => autoSave({ type: 'realsense', index: idx }));

    card.querySelector('[data-action="remove"]').addEventListener('click', () => {
      if (config.realsense.length <= 1) return;
      card.remove();
      structuralChange();
      renderRealSense();
    });
    // Live-update fields
    card.querySelectorAll('.rs-px, .rs-py, .rs-pz, .rs-rr, .rs-rp, .rs-ry').forEach(el => {
      el.addEventListener('input', () => autoSave({ type: 'realsense', index: idx }));
    });
    card.querySelector('.rs-label').addEventListener('input', () => autoSave({ type: 'realsense', index: idx }));
    card.querySelector('.rs-ptsize').addEventListener('input', () => autoSave({ type: 'realsense', index: idx }));
    // Structural fields
    card.querySelector('.rs-enabled').addEventListener('change', () => structuralChange());
    card.querySelector('.rs-path').addEventListener('input', () => structuralChange());
    card.querySelector('.rs-showColor').addEventListener('change', () => structuralChange());
    return card;
  }

  function renderCameraCard(cam, idx) {
    const card = document.createElement("div");
    card.className = "item-card";
    card.dataset.id = cam.id;
    card.innerHTML = `
      <div class="item-header">
        <input type="checkbox" class="cam-enabled" ${cam.enabled !== false ? "checked" : ""} title="Enable/disable this camera" />
        <span>Camera ${idx + 1}</span>
        <button class="btn-remove" data-action="remove">Remove</button>
      </div>
      <div class="field">
        <label>Label</label>
        <input type="text" class="cam-label" value="${esc(cam.label)}" />
      </div>
      <div class="field">
        <label>MoQ Path</label>
        <input type="text" class="cam-path" value="${esc(cam.path)}" placeholder="e.g. anon/webcam" />
      </div>
    `;
    // Tags
    renderTagsField(card, cam.tags || [], () => autoSave(null));

    card.querySelector('[data-action="remove"]').addEventListener('click', () => {
      card.remove();
      structuralChange();
      renderCameras();
    });
    // Structural fields
    card.querySelector('.cam-enabled').addEventListener('change', () => structuralChange());
    card.querySelector('.cam-path').addEventListener('input', () => structuralChange());
    card.querySelector('.cam-label').addEventListener('input', () => autoSave(null));
    return card;
  }

  // ─── List renderers ──────────────────────────────
  function renderArmPairs() {
    const list = q("armPairsList");
    list.innerHTML = "";
    config.armPairs.forEach((pair, i) => list.appendChild(renderArmPairCard(pair, i)));
  }

  function renderRealSense() {
    const list = q("realsenseList");
    list.innerHTML = "";
    config.realsense.forEach((rs, i) => list.appendChild(renderRealSenseCard(rs, i)));
  }

  function renderCameras() {
    const list = q("camerasList");
    list.innerHTML = "";
    config.cameras.forEach((cam, i) => list.appendChild(renderCameraCard(cam, i)));
  }

  function renderGeneral() {
    q("relay").value = config.general.relay;
    q("certHash").value = config.general.certHash;
  }

  function renderAll() {
    renderGeneral();
    renderArmPairs();
    renderRealSense();
    renderCameras();
    q("audioEnabled").checked = config.audio.enabled !== false;
    q("audioPath").value = config.audio.path;
    q("chatEnabled").checked = config.chat.enabled !== false;
    q("chatPath").value = config.chat.path;
    q("chatUsername").value = config.chat.username;
    attachGeneralAutoSave();
  }

  // Auto-save for general/audio/chat fields
  function attachGeneralAutoSave() {
    const fields = [q("relay"), q("certHash"), q("audioPath"), q("chatPath"), q("chatUsername")];
    fields.forEach(el => {
      el.removeEventListener("input", onGeneralStructural);
      el.addEventListener("input", onGeneralStructural);
    });
    const checks = [q("audioEnabled"), q("chatEnabled")];
    checks.forEach(el => {
      el.removeEventListener("change", onGeneralStructural);
      el.addEventListener("change", onGeneralStructural);
    });
  }
  function onGeneralStructural() { structuralChange(); }

  // ─── Add buttons ─────────────────────────────────
  q("addArmPair").addEventListener("click", () => {
    const n = config.armPairs.length + 1;
    config.armPairs.push({
      id: "pair" + n,
      enabled: true,
      label: "Arm Pair " + n,
      leftPath: "",
      rightPath: "",
      position: {x: (n - 1) * 2, y: 0, z: 0},
      rotation: {roll:0, pitch:0, yaw:0},
      queryRate: 100,
      autoQuery: false,
    });
    saveConfig(config);
    renderArmPairs();
    if (callbacks.onStructuralChange) callbacks.onStructuralChange();
  });

  q("addRealSense").addEventListener("click", () => {
    const n = config.realsense.length + 1;
    config.realsense.push({
      id: "rs" + n,
      enabled: true,
      label: "RealSense " + n,
      path: "",
      position: {x: -0.33 + (n - 1), y: 0.84, z: 0.29 + (n - 1)},
      rotation: {roll:90, pitch:-222, yaw:0},
      showColor: true,
      pointSize: 2,
    });
    saveConfig(config);
    renderRealSense();
    if (callbacks.onStructuralChange) callbacks.onStructuralChange();
  });

  q("addCamera").addEventListener("click", () => {
    const n = config.cameras.length + 1;
    config.cameras.push({
      id: "cam" + n,
      enabled: true,
      label: "Camera " + n,
      path: "",
    });
    saveConfig(config);
    renderCameras();
    if (callbacks.onStructuralChange) callbacks.onStructuralChange();
  });

  // ─── Footer actions ──────────────────────────────
  q("primaryBtn").addEventListener("click", () => {
    readFromUI();
    saveConfig(config);
    if (callbacks.onPrimaryAction) callbacks.onPrimaryAction();
  });

  q("resetDefaults").addEventListener("click", () => {
    if (!confirm("Reset all settings to defaults?")) return;
    Object.assign(config, defaultConfig());
    saveConfig(config);
    renderAll();
    if (callbacks.onStructuralChange) callbacks.onStructuralChange();
  });

  q("exportJson").addEventListener("click", () => {
    readFromUI();
    saveConfig(config);
    const blob = new Blob([JSON.stringify(config, null, 2)], { type: "application/json" });
    const a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = "openarm-config.json";
    a.click();
  });

  q("importJson").addEventListener("click", () => importFile.click());

  importFile.addEventListener("change", (e) => {
    const file = e.target.files[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = (ev) => {
      try {
        const imported = JSON.parse(ev.target.result);
        if (!imported.general || !imported.armPairs) { alert("Invalid config file"); return; }
        Object.assign(config, imported);
        saveConfig(config);
        renderAll();
        if (callbacks.onStructuralChange) callbacks.onStructuralChange();
      } catch (err) {
        alert("Failed to parse JSON: " + err.message);
      }
    };
    reader.readAsText(file);
    e.target.value = "";
  });

  // ─── Import Machine ──────────────────────────────
  function importMachineJson(text) {
    try {
      const m = JSON.parse(text);
      if (!m.services || !m.machine_id) {
        alert("Not a valid machine.json (missing services or machine_id)");
        return;
      }
      readFromUI();
      const hostname = m.hostname || m.machine_id;
      let added = [];

      const cans = m.services.can || [];
      for (let i = 0; i < cans.length; i += 2) {
        const left = cans[i], right = cans[i + 1];
        const n = config.armPairs.length + 1;
        config.armPairs.push({
          id: "pair" + n, enabled: true,
          label: hostname + " Arm " + n,
          leftPath: left ? left.moq_path : "",
          rightPath: right ? right.moq_path : "",
          position: { x: (n - 1) * 2, y: 0, z: 0 },
          rotation: { roll: 0, pitch: 0, yaw: 0 },
          queryRate: 100, autoQuery: false,
        });
        added.push("arm pair: " + (left ? left.interface : "?") + " + " + (right ? right.interface : "none"));
      }

      const fakeCans = m.services.fake_can || [];
      for (let i = 0; i < fakeCans.length; i += 2) {
        const left = fakeCans[i], right = fakeCans[i + 1];
        const n = config.armPairs.length + 1;
        config.armPairs.push({
          id: "pair" + n, enabled: true,
          label: hostname + " Fake Arm " + n,
          leftPath: left ? left.moq_path : "",
          rightPath: right ? right.moq_path : "",
          position: { x: (n - 1) * 2, y: 0, z: 0 },
          rotation: { roll: 0, pitch: 0, yaw: 0 },
          queryRate: 100, autoQuery: false,
        });
        added.push("fake arm pair: " + (left ? left.interface : "?") + " + " + (right ? right.interface : "none"));
      }

      const rsList = m.services.realsense || [];
      for (const rs of rsList) {
        const n = config.realsense.length + 1;
        config.realsense.push({
          id: "rs" + n, enabled: true,
          label: hostname + " RealSense " + n,
          path: rs.moq_path,
          position: { x: -0.33 + (n - 1), y: 0.84, z: 0.29 + (n - 1) },
          rotation: { roll: 90, pitch: -45, yaw: 0 },
          showColor: true, pointSize: 2,
        });
        added.push("realsense: " + rs.serial);
      }

      const camList = m.services.cameras || [];
      for (const cam of camList) {
        const n = config.cameras.length + 1;
        config.cameras.push({
          id: "cam" + n, enabled: true,
          label: hostname + " Camera " + n,
          path: cam.moq_path,
        });
        added.push("camera: " + cam.index);
      }

      if (added.length === 0) { alert("No services found in machine.json"); return; }
      saveConfig(config);
      renderAll();
      if (callbacks.onStructuralChange) callbacks.onStructuralChange();
      alert("Imported from " + hostname + ":\n" + added.join("\n"));
    } catch (err) {
      alert("Failed to parse machine.json: " + err.message);
    }
  }

  q("importMachine").addEventListener("click", () => importMachineFile.click());

  importMachineFile.addEventListener("change", (e) => {
    const file = e.target.files[0];
    if (!file) return;
    const reader = new FileReader();
    reader.onload = (ev) => importMachineJson(ev.target.result);
    reader.readAsText(file);
    e.target.value = "";
  });

  q("importClipboard").addEventListener("click", async () => {
    try {
      const text = await navigator.clipboard.readText();
      importMachineJson(text);
    } catch (err) {
      alert("Could not read clipboard. Try copying the machine.json content first.\n\n" + err.message);
    }
  });

  // ─── Init ────────────────────────────────────────
  renderAll();

  return {
    renderAll,
    destroy() { containerEl.innerHTML = ""; },
  };
}
