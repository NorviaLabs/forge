// Browser smoke check and representative screenshots of compiled cell buffers.
import { createRequire } from 'node:module';
import { mkdir } from 'node:fs/promises';
import { resolve, join } from 'node:path';
import { pathToFileURL } from 'node:url';
import assert from 'node:assert/strict';
const require = createRequire(import.meta.url);
const { chromium } = require(process.env.FORGE_PLAYWRIGHT_MODULE || '/opt/homebrew/lib/node_modules/@playwright/test');
const [file, out] = process.argv.slice(2);
if (!file || !out) throw new Error('Pass CAPTURE_HTML SCREENSHOT_DIRECTORY');
await mkdir(out, { recursive: true });
const browser = await chromium.launch({ headless: true });
const errors = [];
let checked = 0;
try {
  const page = await browser.newPage({ viewport: { width: 2800, height: 1400 }, deviceScaleFactor: 1 });
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(pathToFileURL(resolve(file)).href);
  const available = new Set(await page.evaluate(() => Object.keys(frames.after)));
  const options = id => page.locator('#' + id + ' option').evaluateAll(rows => rows.map(row => row.value));
  for (const theme of await options('theme')) {
    await page.selectOption('#theme', theme);
    for (const size of await options('size')) {
      await page.selectOption('#size', size);
      for (const state of await options('state')) {
        if (!available.has(`${state}-${theme}-${size}`)) continue;
        await page.selectOption('#state', state);
        assert.ok(await page.locator('#after canvas').isVisible(), `${state} ${theme} ${size}: capture missing`);
        if (['start', 'caret', 'working', 'stillworking', 'review', 'source', 'approval', 'details', 'recovery', 'help', 'commands', 'files', 'sessions', 'models', 'terminal', 'dock', 'queue', 'jobs', 'agents', 'jobstop', 'joboutput', 'jobcontrols', 'jobpartial', 'jobinsert', 'childlist', 'childpeek', 'childdecision', 'childstop', 'childpartial', 'childinsert'].includes(state) && ['80x18', '120x40', '116x28', '132x40'].includes(size)) {
          await page.locator('.pair').screenshot({ path: join(out, `${state}-${theme}-${size}.png`) });
        }
        checked++;
      }
    }
  }
  assert.deepEqual(errors, []);
  console.log(`Browser rendered ${checked} compiled captures without errors; screenshots: ${resolve(out)}`);
} finally { await browser.close(); }
