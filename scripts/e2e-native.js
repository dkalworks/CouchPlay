// End-to-end check of the native Rust host against a real headless Chrome viewer.
// Usage: npm run build && cd host && cargo build, then (server running) npm run e2e:native
// Env: E2E_URL (default http://localhost:3001), HOST_BIN (default host/target/debug/couchplay-host(.exe)),
//      CHROME_PATH, EXPECT_VIDEO=1 to also require decoded video frames, E2E_SCREENSHOT=file.png to save a frame,
//      EXPECT_AUDIO=1 to play a short sound and require the viewer to decode it (needs EXPECT_VIDEO=1).
import { spawn } from 'node:child_process';
import { existsSync } from 'node:fs';
import { resolve } from 'node:path';
import puppeteer from 'puppeteer-core';

const BASE = process.env.E2E_URL ?? 'http://localhost:3001';
const exe = process.platform === 'win32' ? '.exe' : '';
const HOST_BIN = process.env.HOST_BIN ?? resolve(import.meta.dirname, `../host/target/debug/couchplay-host${exe}`);
if (!existsSync(HOST_BIN)) throw new Error(`Host binary not found: ${HOST_BIN} (run cargo build in host/)`);

const CHROME_CANDIDATES = [
  process.env.CHROME_PATH,
  'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
  'C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe',
  '/usr/bin/google-chrome',
  '/usr/bin/chromium',
];
const executablePath = CHROME_CANDIDATES.find((p) => p && existsSync(p));
if (!executablePath) throw new Error('Chrome not found; set CHROME_PATH');

const wait = (page, fn, timeout = 5000) => page.waitForFunction(fn, { timeout, polling: 100 });
const diag = (page) =>
  page.$$eval('#diag > div', (rows) => Object.fromEntries(rows.map((r) => [r.firstChild.textContent, r.lastChild.textContent])));

// E2E_WINDOW=1: stream a window the script itself opens (scripts/e2e-window.ps1, which resizes
// itself after 14 s) instead of the primary monitor.
const WINDOW_TITLE = 'CouchPlay E2E Window';
let target = null;
if (process.env.E2E_WINDOW) {
  target = spawn('powershell', ['-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', resolve(import.meta.dirname, 'e2e-window.ps1'), '-Title', WINDOW_TITLE, '-ResizeAfterMs', '14000'], { stdio: 'ignore' });
  await new Promise((r) => setTimeout(r, 2500));
}

let log = '';
const hostArgs = ['--server', BASE.replace(/^http/, 'ws'), ...(target ? ['--window', WINDOW_TITLE] : [])];
const host = spawn(HOST_BIN, hostArgs, {
  env: { ...process.env, RUST_LOG: 'couchplay_host=debug,warn', NO_COLOR: '1' },
});
const onData = (d) => (log += d.toString());
host.stdout.on('data', onData);
host.stderr.on('data', onData);
const until = async (re, timeout = 8000) => {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    const m = log.match(re);
    if (m) return m;
    await new Promise((r) => setTimeout(r, 100));
  }
  throw new Error(`Timed out waiting for host log ${re}\n--- host log ---\n${log}`);
};

const browser = await puppeteer.launch({
  executablePath,
  headless: true,
  args: ['--autoplay-policy=no-user-gesture-required'],
});
try {
  const [, code] = await until(/Code ([A-Z2-9]{5})/);
  console.log(`✓ native host created room ${code}`);

  const viewer = await browser.newPage();
  await viewer.goto(`${BASE}/?room=${code}`);
  await viewer.type('#name', 'Tester');
  await viewer.click('#join-form button');
  await until(/Tester joined/);
  await wait(viewer, () => [...document.querySelectorAll('#diag > div')].some((r) => /WebRTC/.test(r.firstChild.textContent) && /connected/.test(r.lastChild.textContent)), 15000);
  console.log('✓ WebRTC connected');

  if (process.env.EXPECT_VIDEO) {
    await wait(viewer, () => document.querySelector('#video').videoWidth > 0, 15000);
    console.log('✓ viewer receiving video', await viewer.$eval('#video', (v) => `${v.videoWidth}x${v.videoHeight}`));
    if (target) {
      await new Promise((r) => setTimeout(r, 1000));
      if (process.env.E2E_SCREENSHOT) await (await viewer.$('#video')).screenshot({ path: process.env.E2E_SCREENSHOT });
      await until(/capture size/, 40000); // the window resizes itself a while after it opens
      await new Promise((r) => setTimeout(r, 2500));
      if (process.env.E2E_SCREENSHOT) {
        await (await viewer.$('#video')).screenshot({ path: process.env.E2E_SCREENSHOT.replace(/(\.\w+)$/, '-resized$1') });
      }
      const lines = log.split(/\r?\n/).filter((l) => /capture size|capturing/.test(l));
      console.log(lines.map((l) => l.replace(/^\S+\s+/, '')).join('\n'));
      console.log('✓ host followed the window resize; stream continues', await viewer.$eval('#video', (v) => `${v.videoWidth}x${v.videoHeight}`));
    }
    if (process.env.E2E_SCREENSHOT && !target) {
      await new Promise((r) => setTimeout(r, 6000));
      await (await viewer.$('#video')).screenshot({ path: process.env.E2E_SCREENSHOT });
      console.log('✓ saved screenshot', process.env.E2E_SCREENSHOT);
      console.log(log.split(/\r?\n/).filter((l) => l.includes('video:')).slice(-3).join('\n'));
    }
  }

  if (process.env.EXPECT_AUDIO) {
    // Plays a short Windows sound on the default output; the host loops it back to the viewer.
    await wait(viewer, () => document.querySelector('#video').srcObject?.getAudioTracks().length > 0, 15000);
    await viewer.evaluate(() => {
      const ctx = new AudioContext();
      const an = ctx.createAnalyser();
      ctx.createMediaStreamSource(document.querySelector('#video').srcObject).connect(an);
      window.__peak = 0;
      setInterval(() => {
        const d = new Float32Array(an.fftSize);
        an.getFloatTimeDomainData(d);
        for (const x of d) window.__peak = Math.max(window.__peak, Math.abs(x));
      }, 50);
    });
    const wav = 'C:/Windows/Media/chimes.wav';
    spawn('powershell', ['-NoProfile', '-Command', `(New-Object Media.SoundPlayer '${wav}').PlaySync()`], { stdio: 'ignore' });
    await wait(viewer, () => window.__peak > 0.01, 10000);
    console.log('✓ viewer decoded audio, peak', await viewer.evaluate(() => window.__peak.toFixed(3)));
  }

  await viewer.click('#toggle');
  await until(/controller is now [A-Z2-9]{8}/);
  console.log('✓ host sees control token move to the viewer');

  await viewer.bringToFront();
  await viewer.keyboard.down('KeyZ');
  await until(/input: [1-9]\d* accepted/);
  console.log('✓ host accepted input packets from the controller');
  await viewer.keyboard.up('KeyZ');

  await viewer.click('#toggle');
  await until(/controller is now the host/);
  console.log('✓ control passed back to the host');

  console.log('\nviewer diagnostics', await diag(viewer));
  console.log('\nNative E2E passed.');
} catch (e) {
  console.error('Native E2E failed:', e.message, `\n--- host log ---\n${log}`);
  process.exitCode = 1;
} finally {
  await browser.close();
  target?.kill();
  host.kill();
}
