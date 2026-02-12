import { chromium } from "playwright";

const url = "https://cdn.1ms.ai/anon/realsense";

(async () => {
  const browser = await chromium.launch({
    channel: "chrome",
    headless: false,  // WebTransport requires full browser
    args: [
      "--headless=new",  // New headless mode supports WebTransport
      "--enable-features=WebTransport",
      "--origin-to-force-quic-on=cdn.1ms.ai:443",
    ],
  });
  const page = await browser.newPage();

  // Collect console messages
  page.on("console", (msg) => {
    const type = msg.type();
    const text = msg.text();
    if (type === "error") console.log(`[BROWSER ERROR] ${text}`);
    else if (type === "warning") console.log(`[BROWSER WARN] ${text}`);
    else console.log(`[BROWSER] ${text}`);
  });

  // Test 1: Raw WebTransport
  console.log(`\n=== Test 1: Raw WebTransport to ${url} ===`);
  const wtResult = await page.evaluate(async (testUrl) => {
    if (typeof WebTransport === "undefined") {
      return { success: false, error: "WebTransport is not defined in this browser" };
    }
    try {
      const t0 = performance.now();
      const wt = new WebTransport(testUrl, {
        allowPooling: false,
        congestionControl: "low-latency",
      });

      // Listen for close
      const closePromise = wt.closed.then(info => ({ status: "closed_before_ready", info }))
        .catch(e => ({ status: "closed_before_ready", error: e.message }));

      // Race ready vs timeout vs close
      const result = await Promise.race([
        wt.ready.then(() => ({ status: "ready", ms: (performance.now() - t0).toFixed(0) })),
        closePromise,
        new Promise(r => setTimeout(() => r({ status: "timeout_5s" }), 5000)),
      ]);

      if (result.status === "ready") {
        try {
          const stream = await wt.createBidirectionalStream();
          wt.close();
          return { success: true, ms: result.ms, message: "WebTransport + bidi stream works!" };
        } catch (e) {
          wt.close();
          return { success: true, ms: result.ms, streamError: e.message };
        }
      }

      return result;
    } catch (e) {
      return { success: false, error: e.message, name: e.name };
    }
  }, url);

  console.log("Result:", JSON.stringify(wtResult, null, 2));

  // Test 2: WebSocket (wss://)
  console.log(`\n=== Test 2: WebSocket to wss://cdn.1ms.ai/anon/realsense ===`);
  const wsResult = await page.evaluate(async () => {
    return new Promise((resolve) => {
      try {
        const ws = new WebSocket("wss://cdn.1ms.ai/anon/realsense");
        ws.binaryType = "arraybuffer";
        const t0 = performance.now();
        ws.onopen = () => {
          const ms = (performance.now() - t0).toFixed(0);
          ws.close();
          resolve({ success: true, ms, message: "WebSocket connected!" });
        };
        ws.onerror = () => resolve({ success: false, error: "WebSocket error" });
        ws.onclose = (e) => {
          if (!e.wasClean) resolve({ success: false, error: `closed: code=${e.code}` });
        };
        setTimeout(() => resolve({ success: false, error: "timeout" }), 5000);
      } catch (e) {
        resolve({ success: false, error: e.message });
      }
    });
  });

  console.log("Result:", JSON.stringify(wsResult, null, 2));

  await browser.close();
})();
