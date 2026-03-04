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
  // Only show important messages
  if (msg.type() === "error" || text.includes("Depth") || text.includes("depth") || text.includes("gravity") || text.includes("Intrinsics")) {
    console.log(`[browser:${msg.type()}] ${text}`);
  }
});
page.on("pageerror", (err) => console.error(`[PAGE ERROR] ${err.message}`));

const url = `http://localhost:${port}/openarm.html`;
console.log(`Opening: ${url}`);
await page.goto(url);

console.log("Waiting 20s for connection + data...");
await page.waitForTimeout(20000);

// Deep inspect ALL rsCams depth decoders
const result = await page.evaluate(() => {
  const out = {};
  // Find rsCams — they're stored in the sceneHandle closure, but dumpDepth accesses rsCams[0]
  // Let's check all cameras by looking at the global dumpDepth

  // The rsCams array is not directly accessible, but we can patch it through the module
  // Actually, let's check if there are depth decoder stats we can access

  // Try to manually check by finding the depth decoder through the video elements
  const rsVideos = document.querySelectorAll('video[id^="rsVideo"]');
  out.videoCount = rsVideos.length;
  out.videos = [];
  for (let i = 0; i < rsVideos.length; i++) {
    const v = rsVideos[i];
    out.videos.push({
      id: v.id,
      videoWidth: v.videoWidth,
      videoHeight: v.videoHeight,
      readyState: v.readyState,
      paused: v.paused,
      currentTime: v.currentTime,
    });
  }

  // Check the point cloud geometry draw ranges
  const canvasEl = document.getElementById('threeCanvas');
  out.canvasExists = !!canvasEl;

  // dumpDepth only checks rsCams[0], let's see if there's another way
  // Actually the page auto-connects, so the rsCams should be populated via closure
  // We need to expose them. Let's try a workaround:

  // Check if window.dumpDepth exists and try it
  if (window.dumpDepth) {
    try {
      const d = window.dumpDepth();
      out.dumpDepth0 = d || 'returned undefined (no data for rsCams[0])';
    } catch(e) {
      out.dumpDepth0 = `error: ${e.message}`;
    }
  }

  return out;
});
console.log("\n=== BASIC STATE ===");
console.log(JSON.stringify(result, null, 2));

// Now inject a deeper diagnostic by patching the processFrame method
const depthDiag = await page.evaluate(async () => {
  // We can't access the module scope directly, but we can check the Three.js scene
  // Find all Points objects in the scene
  const canvas = document.getElementById('threeCanvas');
  if (!canvas || !canvas.__three_renderer) {
    // Try to find the renderer another way
    return { error: "Can't access Three.js renderer directly" };
  }
  return { error: "renderer found but no scene access" };
});
console.log("\n=== THREE.JS DIAG ===");
console.log(JSON.stringify(depthDiag, null, 2));

// Take a screenshot with the log panel visible
await page.screenshot({ path: "/tmp/depth_check2.png", fullPage: false });
console.log("\nScreenshot saved: /tmp/depth_check2.png");

// Now let's check the log panel more carefully
const logText = await page.evaluate(() => {
  const logEl = document.getElementById('log');
  return logEl ? logEl.innerText : 'no log element';
});
console.log("\n=== FULL LOG ===");
console.log(logText);

await browser.close();
await vite.close();
