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
      text.includes("Intrinsics") || text.includes("[DIAG]") || text.includes("gravity")) {
    console.log(`[browser:${msg.type()}] ${text}`);
  }
});
page.on("pageerror", (err) => console.error(`[PAGE ERROR] ${err.message}`));

const url = `http://localhost:${port}/openarm.html`;
console.log(`Opening: ${url}`);
await page.goto(url);

console.log("Waiting 20s for data...");
await page.waitForTimeout(20000);

// Deep diagnostic of all rsCams depth decoders
const diag = await page.evaluate(() => {
  const rsCams = window._rsCams;
  const pointClouds = window._pointClouds;
  if (!rsCams) return { error: 'rsCams not exposed' };

  const result = { cameraCount: rsCams.length, cameras: [] };

  for (let i = 0; i < rsCams.length; i++) {
    const cam = rsCams[i];
    const pc = pointClouds && pointClouds[i];
    const info = {
      index: i,
      hasConn: !!cam.conn,
      hasColorPlayer: !!cam.colorPlayer,
      hasDepthDecoder: !!cam.depthDecoder,
      hasIntrinsics: !!cam.intrinsics,
      hasGravity: !!cam.gravity,
    };

    if (cam.depthDecoder) {
      const dec = cam.depthDecoder;
      info.depth = {
        configured: dec.configured,
        configuredCodec: dec.configuredCodec,
        disabled: dec.disabled,
        useMse: dec._useMse,
        is10bit: dec.is10bit,
        width: dec.width,
        height: dec.height,
        frameCount: dec.frameCount,
        hasLatestY: !!dec.latestY,
        latestYType: dec.latestY ? dec.latestY.constructor.name : null,
        latestYLength: dec.latestY ? dec.latestY.length : 0,
      };

      // Sample latestY values
      if (dec.latestY && dec.latestY.length > 0) {
        const Y = dec.latestY;
        const w = dec.width, h = dec.height;
        let min = Infinity, max = 0, nonzero = 0, sum = 0;
        for (let j = 0; j < Y.length; j++) {
          const v = Y[j];
          if (v > 0) {
            nonzero++;
            sum += v;
            if (v < min) min = v;
            if (v > max) max = v;
          }
        }
        info.depth.stats = {
          totalPixels: Y.length,
          nonzeroPixels: nonzero,
          zeroPixels: Y.length - nonzero,
          min,
          max,
          avg: nonzero > 0 ? (sum / nonzero).toFixed(1) : 0,
        };

        // Sample specific pixels (center, corners)
        const samples = {};
        const midR = Math.floor(h / 2), midC = Math.floor(w / 2);
        samples.center = Y[midR * w + midC];
        samples.topLeft = Y[0];
        samples.topRight = Y[w - 1];
        samples.bottomLeft = Y[(h-1) * w];
        samples.bottomRight = Y[(h-1) * w + (w-1)];
        // Row of samples across center
        samples.centerRow = [];
        for (let c = 0; c < w; c += Math.floor(w/20)) {
          samples.centerRow.push(Y[midR * w + c]);
        }
        info.depth.samples = samples;
      }
    }

    // Point cloud geometry info
    if (pc) {
      const geom = pc.geometry;
      info.pointCloud = {
        drawRangeStart: geom.drawRange.start,
        drawRangeCount: geom.drawRange.count,
        positionCount: geom.attributes.position.count,
      };
    }

    if (cam.intrinsics) {
      info.intrinsicsDetail = cam.intrinsics;
    }

    result.cameras.push(info);
  }

  return result;
});

console.log("\n=== FULL DEPTH DIAGNOSTIC ===");
console.log(JSON.stringify(diag, null, 2));

// Get log
const logText = await page.evaluate(() => document.getElementById('log')?.innerText);
console.log("\n=== LOG ===");
console.log(logText);

await page.screenshot({ path: "/tmp/depth_check4.png" });
console.log("\nScreenshot: /tmp/depth_check4.png");

await browser.close();
await vite.close();
