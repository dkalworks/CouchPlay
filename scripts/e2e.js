// Real-browser end-to-end check: host shares a (fake) screen, a viewer joins,
// receives video, takes control, sends input, and passes control back.
// Usage: npm run build && npm start (in another terminal), then npm run e2e
// Env: E2E_URL (default http://localhost:3001), CHROME_PATH (auto-detected on Windows/Linux/macOS)
import { existsSync } from 'node:fs';
import puppeteer from 'puppeteer-core';

const BASE = process.env.E2E_URL ?? 'http://localhost:3001';
const CHROME_CANDIDATES = [
  process.env.CHROME_PATH,
  'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium',
  '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome',
];
const executablePath = CHROME_CANDIDATES.find((p) => p && existsSync(p));
if (!executablePath) throw new Error('Chrome not found; set CHROME_PATH');

// Polling by interval, not requestAnimationFrame: rAF doesn't run in background tabs.
const wait = (page, fn, timeout = 5000) => page.waitForFunction(fn, { timeout, polling: 100 });
const diag = (page) =>
  page.$$eval('#diag > div', (rows) => Object.fromEntries(rows.map((r) => [r.firstChild.textContent, r.lastChild.textContent])));

const browser = await puppeteer.launch({
  executablePath,
  headless: true,
  args: [
    '--use-fake-ui-for-media-stream',
    '--use-fake-device-for-media-stream',
    '--auto-select-desktop-capture-source=Entire screen',
  ],
});

try {
  const host = await browser.newPage();
  await host.goto(`${BASE}/host.html`);
  await host.click('#start');
  await wait(host, () => /^[A-Z2-9]{5}$/.test(document.querySelector('#code').textContent), 8000);
  const code = await host.$eval('#code', (e) => e.textContent);
  console.log(`✓ host created room ${code}`);

  const viewer = await browser.newPage();
  await viewer.goto(`${BASE}/?room=${code}`);
  await viewer.type('#name', 'Tester');
  await viewer.click('#join-form button');
  await wait(viewer, () => document.querySelector('#video').videoWidth > 0, 15000);
  console.log('✓ viewer receiving video', await viewer.$eval('#video', (v) => `${v.videoWidth}x${v.videoHeight}`));

  await viewer.click('#toggle');
  await wait(host, () => document.querySelector('#holder').textContent.includes('Tester'));
  console.log('✓ host sees:', await host.$eval('#holder', (e) => e.textContent));

  await viewer.bringToFront();
  await viewer.keyboard.down('KeyZ');
  await wait(host, () => document.querySelector('#pad .pad-buttons span').classList.contains('on'));
  console.log('✓ host virtual pad shows A pressed');
  await viewer.keyboard.up('KeyZ');

  await viewer.click('#toggle');
  await wait(host, () => document.querySelector('#holder').textContent.includes('You are playing'));
  await wait(host, () => !document.querySelector('#pad .pad-buttons span').classList.contains('on'));
  console.log('✓ control passed back, pad reset to neutral');

  console.log('\nviewer diagnostics', await diag(viewer));
  console.log('host diagnostics', await diag(host));
  console.log('\nE2E passed.');
} catch (e) {
  console.error('E2E failed:', e.message);
  process.exitCode = 1;
} finally {
  await browser.close();
}
