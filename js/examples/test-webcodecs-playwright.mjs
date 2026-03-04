#!/usr/bin/env node
// Playwright WebKit test for WebCodecs AV1 VideoDecoder
import { webkit } from 'playwright';

const URL = 'http://localhost:5173/test-webcodecs-safari.html';

async function main() {
  console.log('Launching WebKit browser...');
  const browser = await webkit.launch({ headless: true });
  const context = await browser.newContext();
  const page = await context.newPage();

  // Capture console output
  page.on('console', msg => {
    console.log(`[WebKit] ${msg.text()}`);
  });
  page.on('pageerror', err => {
    console.error(`[WebKit ERROR] ${err.message}`);
  });

  console.log(`Navigating to ${URL}...`);
  await page.goto(URL, { waitUntil: 'domcontentloaded' });

  // Wait for tests to complete (up to 60 seconds)
  console.log('Waiting for tests to complete...');
  try {
    await page.waitForFunction(() => window.__testDone === true, { timeout: 60000 });
  } catch (e) {
    console.error('Tests did not complete within 60 seconds');
  }

  // Get full log output
  const logText = await page.$eval('#log', el => el.textContent);
  console.log('\n=== FULL TEST OUTPUT ===');
  console.log(logText);

  await browser.close();
}

main().catch(e => { console.error(e); process.exit(1); });
