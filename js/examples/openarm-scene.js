// openarm-scene.js — Three.js scene, URDF loading, point clouds, render loop

import * as THREE from "three";
import { OrbitControls } from "three/examples/jsm/controls/OrbitControls.js";
import { ColladaLoader } from "three/examples/jsm/loaders/ColladaLoader.js";
import { STLLoader } from "three/examples/jsm/loaders/STLLoader.js";
import URDFLoader from "urdf-loader";
import { JOINTS, URDF_PREFIXES } from "./openarm-can.js";
import { log, setStatus, formatBytes } from "./openarm-log.js";
import { updatePointCloudGeneric } from "./openarm-depth.js";

// ─── Build joint info panels ────────────────────────
export function buildJointRows(container, prefix) {
  const els = [];
  for (let i = 0; i < JOINTS.length; i++) {
    const j = JOINTS[i];
    const row = document.createElement("div");
    row.className = "joint-row";
    row.innerHTML = `
      <span class="joint-label" style="color:#${j.color.toString(16).padStart(6,'0')}">${j.name}</span>
      <span class="joint-angle" id="${prefix}-angle-${i}">0.0&deg;</span>
      <span class="joint-vel" id="${prefix}-vel-${i}">0.0</span>
      <span class="joint-tau" id="${prefix}-tau-${i}">0.0</span>
    `;
    container.appendChild(row);
    els.push({
      angle: row.querySelector(`#${prefix}-angle-${i}`),
      vel: row.querySelector(`#${prefix}-vel-${i}`),
      tau: row.querySelector(`#${prefix}-tau-${i}`),
    });
  }
  return els;
}

// ─── Camera XYZRPY transform ────────────────────────
const _rx = new THREE.Matrix4(), _ry = new THREE.Matrix4(), _rz = new THREE.Matrix4(), _tmp = new THREE.Matrix4();
export function applyCamPoseFromConfig(group, camCfg) {
  const p = camCfg.position || {}, r = camCfg.rotation || {};
  group.position.set(p.x || 0, p.y || 0, p.z || 0);
  const roll  = (r.roll  || 0) * Math.PI / 180;
  const pitch = (r.pitch || 0) * Math.PI / 180;
  const yaw   = (r.yaw   || 0) * Math.PI / 180;
  _rx.makeRotationX(roll);
  _ry.makeRotationY(pitch);
  _rz.makeRotationZ(yaw);
  _tmp.multiplyMatrices(_rz, _ry);
  _tmp.multiply(_rx);
  group.setRotationFromMatrix(_tmp);
}

// ─── Frustum wireframe ──────────────────────────────
const FALLBACK_INTRINSICS = { fx: 920, fy: 920, ppx: 640, ppy: 360, width: 1280, height: 720 };
const NEAR_M = 0.3, FAR_M = 1.023;

function computeFrustumVerts(intr) {
  const halfW = intr.width / 2, halfH = intr.height / 2;
  const nw = NEAR_M * halfW / intr.fx, nh = NEAR_M * halfH / intr.fy;
  const fw = FAR_M  * halfW / intr.fx, fh = FAR_M  * halfH / intr.fy;
  return new Float32Array([
    -nw,  nh, NEAR_M,   nw,  nh, NEAR_M,   nw, -nh, NEAR_M,  -nw, -nh, NEAR_M,
    -fw,  fh, FAR_M,    fw,  fh, FAR_M,    fw, -fh, FAR_M,   -fw, -fh, FAR_M,
  ]);
}

function buildFrustumGeometry(color) {
  const verts = computeFrustumVerts(FALLBACK_INTRINSICS);
  const idx = [0,1, 1,2, 2,3, 3,0, 4,5, 5,6, 6,7, 7,4, 0,4, 1,5, 2,6, 3,7];
  const geo = new THREE.BufferGeometry();
  geo.setAttribute('position', new THREE.BufferAttribute(verts, 3));
  geo.setIndex(idx);
  return new THREE.LineSegments(geo, new THREE.LineBasicMaterial({ color, opacity: 0.5, transparent: true }));
}

function updateFrustumFromIntrinsics(group, intrinsics, camIdx) {
  const frustum = group.children.find(c => c.isLineSegments);
  if (!frustum) return;
  const verts = computeFrustumVerts(intrinsics);
  frustum.geometry.attributes.position.array.set(verts);
  frustum.geometry.attributes.position.needsUpdate = true;
}

// ─── Gripper conversion ─────────────────────────────
const GRIPPER_MOTOR_OPEN_RAD = -1.0472;
const GRIPPER_JOINT_OPEN_M = 0.044;
function gripperMotorToJoint(motorRad) {
  return Math.max(0, Math.min(GRIPPER_JOINT_OPEN_M,
    GRIPPER_JOINT_OPEN_M * (motorRad / GRIPPER_MOTOR_OPEN_RAD)));
}

// ─── Lerp smoothing ─────────────────────────────────
const LERP_FACTOR = 0.2;
function lerpJointStates(states) {
  for (const s of states) {
    s.angle += (s.targetAngle - s.angle) * LERP_FACTOR;
  }
}

// ─── Label text with tags ────────────────────────────
export function labelWithTags(label, tags) {
  if (!tags || tags.length === 0) return label;
  return label + " \u00b7 " + tags.join(" \u00b7 ");
}

// ─── Text label sprite ──────────────────────────────
export function makeTextSprite(text, color = "#aaaaaa") {
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d");
  const fontSize = 52;
  const font = `300 ${fontSize}px "Helvetica Neue", "Segoe UI", Roboto, Helvetica, Arial, sans-serif`;
  ctx.font = font;
  const width = ctx.measureText(text).width + 24;
  canvas.width = width;
  canvas.height = fontSize + 16;
  ctx.font = font;
  ctx.fillStyle = color;
  ctx.globalAlpha = 0.85;
  ctx.textBaseline = "middle";
  ctx.fillText(text, 12, canvas.height / 2);
  const tex = new THREE.CanvasTexture(canvas);
  tex.minFilter = THREE.LinearFilter;
  const mat = new THREE.SpriteMaterial({ map: tex, transparent: true, depthTest: false });
  const sprite = new THREE.Sprite(mat);
  sprite.scale.set(width / 1000, canvas.height / 1000, 1);
  return sprite;
}

// ─── Init scene ─────────────────────────────────────
export function initScene(config, armConfigs, cameraSplitEl) {
  const canvas = document.getElementById("threeCanvas");
  let renderer;
  try {
    renderer = new THREE.WebGLRenderer({ canvas, antialias: true });
    renderer.setPixelRatio(window.devicePixelRatio);
    renderer.setClearColor(0x1a1a2e);
  } catch (e) {
    log(`WebGL not available: ${e.message} (3D disabled, MoQ still works)`, "error");
    renderer = null;
  }

  const scene = new THREE.Scene();

  // Lighting
  scene.add(new THREE.AmbientLight(0xffffff, 0.4));
  const dirLight = new THREE.DirectionalLight(0xffffff, 0.8);
  dirLight.position.set(5, 10, 7);
  scene.add(dirLight);
  const dirLight2 = new THREE.DirectionalLight(0x4488ff, 0.3);
  dirLight2.position.set(-5, 3, -5);
  scene.add(dirLight2);

  // Camera
  const camera = new THREE.PerspectiveCamera(50, 1, 0.01, 100);
  camera.position.set(1.0, 0.8, 1.2);
  camera.lookAt(0, 0.4, 0);

  const controls = new OrbitControls(camera, canvas);
  controls.target.set(0, 0.4, 0);
  controls.enableDamping = true;
  controls.dampingFactor = 0.1;
  controls.update();

  // Per-arm-pair grids (sized to robot reach ~1m)
  const grids = [];
  config.armPairs.forEach(pair => {
    const p = pair.position || { x: 0, y: 0, z: 0 };
    const grid = new THREE.GridHelper(1, 10, 0x333355, 0x222244);
    grid.position.set(p.x, p.y, p.z);
    scene.add(grid);
    grids.push(grid);
  });
  scene.add(new THREE.AxesHelper(0.1));

  // ─── RealSense cameras (video + depth + 3D point cloud) ──
  const maxPts = 1280 * 720;
  const DOT_COLORS = [0xff2222, 0x2222ff, 0x22ff22, 0xffff22];
  const FRUSTUM_COLORS = [0xff4444, 0x4444ff, 0x44ff44, 0xffff44];

  const pointClouds = [];
  const rsVideoEls = [];
  const rsCamGroups = [];
  const rsCams = config.realsense.map(() => ({ conn: null, colorPlayer: null, depthDecoder: null, running: false }));

  config.realsense.forEach((rsCfg, i) => {
    const ptSize = (rsCfg.pointSize || 2) * 0.001;

    // Video element
    const feed = document.createElement("div");
    feed.className = "camera-feed";
    feed.innerHTML = `<span class="cam-label">${rsCfg.label || ("RS " + (i+1))}</span>`;
    const video = document.createElement("video");
    video.autoplay = true; video.muted = true; video.playsInline = true;
    video.id = "rsVideo" + i;
    feed.appendChild(video);
    cameraSplitEl.appendChild(feed);
    rsVideoEls.push(video);

    // Point cloud buffers
    const posArr = new Float32Array(maxPts * 3);
    const colArr = new Float32Array(maxPts * 3);
    const geometry = new THREE.BufferGeometry();
    geometry.setAttribute('position', new THREE.BufferAttribute(posArr, 3));
    geometry.setAttribute('color', new THREE.BufferAttribute(colArr, 3));
    geometry.setDrawRange(0, 0);
    const material = new THREE.PointsMaterial({ size: ptSize, vertexColors: true, sizeAttenuation: true });
    const points = new THREE.Points(geometry, material);
    points.scale.set(0.001, 0.001, 0.001);

    // Camera group
    const group = new THREE.Group();
    scene.add(group);
    group.add(points);
    group.add(new THREE.Mesh(
      new THREE.SphereGeometry(0.012, 12, 8),
      new THREE.MeshBasicMaterial({ color: DOT_COLORS[i % DOT_COLORS.length] })
    ));
    group.add(buildFrustumGeometry(FRUSTUM_COLORS[i % FRUSTUM_COLORS.length]));

    // Label above the camera
    const label = makeTextSprite(labelWithTags(rsCfg.label || ("RS " + (i + 1)), rsCfg.tags), "#ff8c42");
    label.position.set(0, 0.06, 0);
    group.add(label);

    rsCamGroups.push(group);

    // Offscreen canvas for color extraction
    const colorCanvas = document.createElement('canvas');
    colorCanvas.width = 1280; colorCanvas.height = 720;
    const colorCtx = colorCanvas.getContext('2d', { willReadFrequently: true });

    pointClouds.push({ posArr, colArr, geometry, material, points, group, colorCtx });
  });

  // Apply RealSense camera poses
  config.realsense.forEach((rsCfg, i) => {
    if (rsCamGroups[i]) applyCamPoseFromConfig(rsCamGroups[i], rsCfg);
  });

  // ─── Plain cameras (video only, no 3D) ────────────
  const camVideoEls = [];
  const camState = config.cameras.map(() => ({ conn: null, colorPlayer: null, running: false }));

  config.cameras.forEach((camCfg, i) => {
    const feed = document.createElement("div");
    feed.className = "camera-feed";
    feed.innerHTML = `<span class="cam-label">${camCfg.label || ("Cam " + (i+1))}</span>`;
    const video = document.createElement("video");
    video.autoplay = true; video.muted = true; video.playsInline = true;
    video.id = "camVideo" + i;
    feed.appendChild(video);
    cameraSplitEl.appendChild(feed);
    camVideoEls.push(video);
  });

  // ─── Load URDF models ────────────────────────────
  const robots = new Array(config.armPairs.length).fill(null);
  const robotGroups = [];

  const urdfLoader = new URDFLoader();
  urdfLoader.loadMeshCb = (path, manager, done) => {
    if (path.endsWith(".stl")) {
      new STLLoader(manager).load(path, geom => {
        done(new THREE.Mesh(geom, new THREE.MeshPhongMaterial()));
      }, null, err => done(null, err));
    } else if (path.endsWith(".dae")) {
      new ColladaLoader(manager).load(path, result => done(result.scene), null, err => done(null, err));
    } else {
      done(null, new Error(`Unknown mesh format: ${path}`));
    }
  };

  setStatus("Loading 3D model...");
  log("Loading URDF model...");
  let modelsLoaded = 0;
  config.armPairs.forEach((pair, pairIdx) => {
    const group = new THREE.Group();
    scene.add(group);
    applyCamPoseFromConfig(group, pair);
    robotGroups.push(group);

    // Label above the arm pair
    const label = makeTextSprite(labelWithTags(pair.label || ("Arm Pair " + (pairIdx + 1)), pair.tags), "#00d4ff");
    label.position.set(0, 0.85, 0);
    group.add(label);

    urdfLoader.load("./assets/openarm_v10.urdf", result => {
      robots[pairIdx] = result;
      result.rotation.x = -Math.PI / 2;
      group.add(result);
      modelsLoaded++;
      if (modelsLoaded === config.armPairs.length) {
        setStatus("Idle");
        log(`${modelsLoaded} robot model(s) loaded`, "success");
      }
    }, undefined, err => {
      log(`URDF load error (pair ${pairIdx + 1}): ${err}`, "error");
      modelsLoaded++;
      if (modelsLoaded === config.armPairs.length) setStatus("Model load failed");
    });
  });

  // Resize handler
  function onResize() {
    if (!renderer) return;
    const container = canvas.parentElement;
    const w = container.clientWidth;
    const h = container.clientHeight;
    renderer.setSize(w, h);
    camera.aspect = w / h;
    camera.updateProjectionMatrix();
  }

  window.addEventListener("resize", onResize);
  requestAnimationFrame(onResize);

  return {
    renderer, scene, camera, controls, canvas, robots, robotGroups, grids,
    // RealSense (video + depth + 3D)
    rsVideoEls, rsCams, pointClouds, rsCamGroups,
    // Plain cameras (video only)
    camVideoEls, camState,
    onResize,
  };
}

// ─── Update arm pose ────────────────────────────────
export function updateArmPose(armStates, robots) {
  armStates.forEach((state, idx) => {
    lerpJointStates(state);
    const pairIdx = Math.floor(idx / 2);
    const sideIdx = idx % 2;
    const robot = robots[pairIdx];
    if (!robot) return;
    const prefix = URDF_PREFIXES[sideIdx];
    const jointNames = ["J1","J2","J3","J4","J5","J6","J7"].map(j => prefix + j);
    for (let i = 0; i < 7; i++) {
      if (robot.joints[jointNames[i]]) robot.joints[jointNames[i]].setJointValue(state[i].angle);
    }
    const grip = gripperMotorToJoint(state[7].angle);
    if (robot.joints[prefix + "EE"]) robot.joints[prefix + "EE"].setJointValue(grip);
  });
}

// ─── Update UI panel ────────────────────────────────
export function updatePanel(armStates, armJointEls, appState) {
  function updateArmEls(els, state) {
    for (let i = 0; i < JOINTS.length; i++) {
      const s = state[i];
      const deg = (s.angle * 180 / Math.PI).toFixed(1);
      els[i].angle.innerHTML = `${deg}&deg;`;
      els[i].vel.textContent = s.velocity.toFixed(1);
      els[i].tau.textContent = s.torque.toFixed(1);
    }
  }
  armStates.forEach((state, idx) => {
    if (armJointEls[idx]) updateArmEls(armJointEls[idx], state);
  });

  document.getElementById("frameCount").textContent = appState.frameCount;
  document.getElementById("bytesReceived").textContent = formatBytes(appState.bytesTotal);

  const now = performance.now();
  if (now - appState.lastFpsTime >= 1000) {
    document.getElementById("canFps").textContent = appState.fpsCounter;
    appState.fpsCounter = 0;
    appState.lastFpsTime = now;
  }

  document.getElementById("lastUpdate").textContent =
    new Date().toLocaleTimeString().split(" ")[0];

  // Stream stats (ping + fps)
  const vfpsNow = performance.now();
  if (vfpsNow - appState.videoFps.lastTime >= 1000) {
    appState.videoFps.value = appState.videoFps.count;
    appState.videoFps.count = 0;
    appState.latency.display = appState.latency.samples > 0 ? Math.round(appState.latency.sum / appState.latency.samples) : null;
    appState.latency.sum = 0;
    appState.latency.samples = 0;
    appState.videoFps.lastTime = vfpsNow;

    const pingEl = document.getElementById('streamPing');
    const fpsEl = document.getElementById('streamFps');
    const hasLatency = appState.latency.display !== null && Date.now() - appState.latency.lastUpdate <= 5000;
    if (hasLatency) {
      const v = appState.latency.display;
      const abs = Math.abs(v);
      pingEl.textContent = (v < 0 ? '~' : '') + 'ping ' + abs + 'ms';
      pingEl.style.color = abs < 50 ? '#2ed573' : abs < 150 ? '#ffa502' : '#ff4757';
    } else {
      pingEl.textContent = '--'; pingEl.style.color = '#555';
    }
    if (appState.videoFps.value !== null) {
      fpsEl.textContent = appState.videoFps.value + ' fps';
      fpsEl.style.color = '#aaa';
    } else {
      fpsEl.textContent = '--'; fpsEl.style.color = '#555';
    }
  }
}

// ─── Render loop ────────────────────────────────────
export function startRenderLoop(sceneHandle, armStates) {
  const { renderer, scene, camera, controls, pointClouds, rsVideoEls, rsCams } = sceneHandle;

  function animate() {
    requestAnimationFrame(animate);
    updateArmPose(armStates, sceneHandle.robots);
    if (!renderer) return;
    controls.update();
    pointClouds.forEach((pc, i) => {
      if (rsCams[i] && rsVideoEls[i]) {
        updatePointCloudGeneric(rsVideoEls[i], rsCams[i].depthDecoder, pc.colorCtx, pc.posArr, pc.colArr, pc.geometry, rsCams[i].intrinsics);
        // Update frustum wireframe when intrinsics arrive
        if (rsCams[i].intrinsics && !rsCams[i]._frustumUpdated) {
          updateFrustumFromIntrinsics(pc.group, rsCams[i].intrinsics, i);
          rsCams[i]._frustumUpdated = true;
        }
      }
    });
    renderer.render(scene, camera);
  }
  animate();
}

// ─── Debug: dumpDepth ───────────────────────────────
export function setupDumpDepth(rsCams) {
  window.dumpDepth = function() {
    const dec = rsCams[0] && rsCams[0].depthDecoder;
    if (!dec || !dec.latestY) { console.log('No depth data yet'); return; }
    const Y = dec.latestY;
    const w = dec.width, h = dec.height;
    const rows = [0, Math.floor(h/4), Math.floor(h/2), Math.floor(3*h/4), h-1];
    const samples = {};
    for (const r of rows) {
      samples['row_'+r] = [];
      for (let c = 0; c < w; c += 10) samples['row_'+r].push(Y[r * w + c]);
    }
    let min = Infinity, max = 0, nonzero = 0;
    for (let i = 0; i < Y.length; i++) { const v = Y[i]; if (v > 0) { nonzero++; if (v < min) min = v; if (v > max) max = v; } }
    let rawSamples = null;
    if (dec.copyBuf && dec._rawDiag) {
      const d = dec._rawDiag;
      const src = new Uint8Array(dec.copyBuf);
      const stride = d.layouts[0].stride;
      const off = d.layouts[0].offset;
      rawSamples = { row0: [], midRow: [] };
      const midR = Math.floor(h / 2);
      for (let c = 0; c < w && c < d.codedWidth; c += 10) {
        const i0 = off + 0 * stride + c * 4;
        const iM = off + midR * stride + c * 4;
        rawSamples.row0.push(Array.from(src.slice(i0, i0 + 4)));
        rawSamples.midRow.push(Array.from(src.slice(iM, iM + 4)));
      }
    }
    const result = {
      browser: navigator.userAgent.includes('Firefox') ? 'Firefox' : navigator.userAgent.includes('Chrome') ? 'Chrome' : 'Other',
      format: dec.fmtLogged ? 'see log' : 'unknown', codec: dec.configuredCodec,
      width: w, height: h, is10bit: dec.is10bit, totalPixels: w*h, nonzeroPixels: nonzero, min, max,
      rawDiag: dec._rawDiag || null, rawSamples, samples
    };
    console.log(JSON.stringify(result));
    navigator.clipboard.writeText(JSON.stringify(result, null, 2)).then(() => console.log('Copied to clipboard'));
    return result;
  };
}
