import { chromium } from "playwright";

const url = "https://cdn.1ms.ai/anon/realsense";

(async () => {
  // Bypass ALL Playwright default args to get a clean Chrome
  const context = await chromium.launchPersistentContext("/tmp/pw-chrome-wt", {
    executablePath: "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    headless: false,
    ignoreAllDefaultArgs: true,
    args: [
      "--remote-debugging-port=0",
      "--no-first-run",
      "--no-default-browser-check",
      "--enable-quic",
    ],
  });
  const page = context.pages()[0] || await context.newPage();

  page.on("console", (msg) => {
    console.log(`[BROWSER ${msg.type()}] ${msg.text()}`);
  });

  // Navigate to a secure context first — WebTransport requires HTTPS origin
  // Navigate to a secure context first — WebTransport requires HTTPS origin
  await page.goto("https://example.com", { waitUntil: "domcontentloaded", timeout: 10000 });
  await page.waitForTimeout(1000);

  const apiCheck = await page.evaluate(() => ({
    WebTransport: typeof WebTransport,
    WebSocket: typeof WebSocket,
    userAgent: navigator.userAgent,
  }));
  console.log("API check:", JSON.stringify(apiCheck, null, 2));

  if (apiCheck.WebTransport === "function") {
    console.log(`\n=== Testing WebTransport to ${url} ===`);
    const result = await page.evaluate(async (testUrl) => {
      try {
        const t0 = performance.now();
        const wt = new WebTransport(testUrl);
        const r = await Promise.race([
          wt.ready.then(() => ({ status: "ready", ms: (performance.now() - t0).toFixed(0) })),
          wt.closed.then(i => ({ status: "closed", info: JSON.stringify(i) })).catch(e => ({ status: "close_error", error: e.message })),
          new Promise(r => setTimeout(() => r({ status: "timeout_10s" }), 10000)),
        ]);
        if (r.status === "ready") {
          const stream = await wt.createBidirectionalStream();
          wt.close();
          return { success: true, ms: r.ms };
        }
        return r;
      } catch (e) {
        return { error: e.message };
      }
    }, url);
    console.log("WebTransport Result:", JSON.stringify(result, null, 2));
  } else {
    console.log("WebTransport STILL not available");
  }

  // Always test WebSocket
  console.log(`\n=== Testing WebSocket ===`);
  const wsResult = await page.evaluate(async () => {
    return new Promise(resolve => {
      const ws = new WebSocket("wss://cdn.1ms.ai/anon/realsense");
      const t0 = performance.now();
      ws.onopen = () => { ws.close(); resolve({ success: true, ms: (performance.now() - t0).toFixed(0) }); };
      ws.onerror = () => resolve({ error: "failed" });
      setTimeout(() => resolve({ error: "timeout" }), 5000);
    });
  });
  console.log("WebSocket Result:", JSON.stringify(wsResult, null, 2));

  await context.close();
})();
