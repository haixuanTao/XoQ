import { chromium } from "playwright";
import { createServer } from "vite";

const vite = await createServer({
  root: new URL("./examples", import.meta.url).pathname,
  server: { port: 0 },
  logLevel: "warn",
});
await vite.listen();
const port = vite.config.server.port || vite.httpServer.address().port;
console.log(`Vite running on port ${port}`);

const browser = await chromium.launch({
  headless: false,
  args: ["--ignore-certificate-errors", "--use-gl=angle"],
});
const page = await (await browser.newContext({ ignoreHTTPSErrors: true })).newPage();

page.on("console", (msg) => {
  const text = msg.text();
  if (msg.type() === "error" || text.includes("Depth") || text.includes("depth") ||
      text.includes("gravity") || text.includes("Intrinsics") || text.includes("point") ||
      text.includes("Point") || text.includes("[DIAG]")) {
    console.log(`[browser:${msg.type()}] ${text}`);
  }
});
page.on("pageerror", (err) => console.error(`[PAGE ERROR] ${err.message}`));

const url = `http://localhost:${port}/openarm.html`;
console.log(`Opening: ${url}`);
await page.goto(url);

// Wait for page to initialize, then inject diagnostic hook
await page.waitForTimeout(2000);

// Inject a hook into updatePointCloudGeneric via the import
await page.evaluate(() => {
  // Intercept the render loop to capture depth data
  window._depthDiag = [];
  const origRAF = window.requestAnimationFrame;
  let diagCount = 0;
  // We'll capture from the dumpDepth hook which has access to rsCams
});

console.log("Waiting 20s for connection + data...");
await page.waitForTimeout(20000);

// Now use page.evaluate to inspect depth state via dumpDepth's scope
const diag = await page.evaluate(() => {
  const out = { cameras: [] };

  // Check video elements
  for (let i = 0; i < 10; i++) {
    const v = document.getElementById('rsVideo' + i);
    if (!v) break;
    out.cameras.push({
      index: i,
      videoWidth: v.videoWidth,
      videoHeight: v.videoHeight,
      readyState: v.readyState,
      currentTime: v.currentTime,
    });
  }

  // Try to call dumpDepth for camera 0
  out.dumpDepth = 'not available';
  if (window.dumpDepth) {
    try {
      const r = window.dumpDepth();
      out.dumpDepth = r || 'returned undefined';
    } catch(e) {
      out.dumpDepth = e.message;
    }
  }

  // Check Three.js scene for Points objects
  try {
    const canvas = document.getElementById('threeCanvas');
    // Walk the scene looking for draw ranges
    // We can't access the scene directly, but we can check WebGL state
    const gl = canvas.getContext('webgl2') || canvas.getContext('webgl');
    if (gl) {
      out.webglOk = true;
      out.drawBufferWidth = gl.drawingBufferWidth;
      out.drawBufferHeight = gl.drawingBufferHeight;
    }
  } catch(e) {
    out.webglError = e.message;
  }

  return out;
});
console.log("\n=== CAMERA STATE ===");
console.log(JSON.stringify(diag, null, 2));

// Now the key test: expose rsCams by modifying setupDumpDepth
// Actually, let's just modify dumpDepth to check all cameras
const allDepth = await page.evaluate(() => {
  // Unfortunately rsCams is module-scoped. But dumpDepth captures rsCams[0].
  // The real issue: we need to see if latestY is populated for ANY camera.
  // Let's check the geometry draw ranges by looking at the Three.js scene:

  // Attempt to traverse Three.js scene from the renderer
  const canvas = document.getElementById('threeCanvas');
  if (!canvas) return { error: 'no canvas' };

  // Three.js stores __r3f or similar on canvas, but we need another approach
  // Let's check if there's a global THREE reference
  return { note: 'Cannot access module-scoped rsCams from page context' };
});

console.log("\n=== DEPTH ACCESS ===");
console.log(JSON.stringify(allDepth, null, 2));

// Get full log
const logText = await page.evaluate(() => document.getElementById('log')?.innerText);
console.log("\n=== FULL LOG ===");
console.log(logText);

await page.screenshot({ path: "/tmp/depth_check3.png" });
console.log("\nScreenshot: /tmp/depth_check3.png");

await browser.close();
await vite.close();
