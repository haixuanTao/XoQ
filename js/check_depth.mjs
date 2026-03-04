import { chromium } from "playwright";
import { createServer } from "vite";

const relay = "https://cdn.1ms.ai";

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

// Capture ALL console messages
page.on("console", (msg) => {
  const text = msg.text();
  console.log(`[browser:${msg.type()}] ${text}`);
});
page.on("pageerror", (err) => console.error(`[PAGE ERROR] ${err.message}`));

const url = `http://localhost:${port}/openarm.html`;
console.log(`Opening: ${url}`);
await page.goto(url);

// Wait for auto-connect + some frames
console.log("Waiting 15s for connection + data...");
await page.waitForTimeout(15000);

// Check depth decoder state
const depthState = await page.evaluate(() => {
  if (window.dumpDepth) {
    try { return window.dumpDepth(); } catch(e) { return { error: e.message }; }
  }
  return { error: "dumpDepth not available" };
});
console.log("\n=== DEPTH STATE ===");
console.log(JSON.stringify(depthState, null, 2));

// Also check what the log panel says
const logText = await page.evaluate(() => {
  const logEl = document.getElementById('log');
  return logEl ? logEl.innerText : 'no log element';
});
console.log("\n=== LOG PANEL ===");
console.log(logText);

await page.screenshot({ path: "/tmp/depth_check.png", fullPage: false });
console.log("\nScreenshot saved: /tmp/depth_check.png");

await browser.close();
await vite.close();
