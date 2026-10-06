import { createRequire } from 'node:module';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { dirname, join } from 'node:path';
import { mkdir, writeFile, mkdtemp, rename, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import assert from 'node:assert/strict';

const require = createRequire(import.meta.url);
const { chromium } = require(process.env.FORGE_PLAYWRIGHT_MODULE || '/opt/homebrew/lib/node_modules/@playwright/test');
const root = dirname(fileURLToPath(import.meta.url));
const browser = await chromium.launch({ headless: true });
const errors = [];
const screenshots = join(root, 'screens');
await mkdir(screenshots, { recursive: true });
try {
  const page = await browser.newPage({ viewport: { width: 1440, height: 1200 }, deviceScaleFactor: 1 });
  page.on('pageerror', error => errors.push(error.message));
  if (process.argv.includes('--references')) {
    for (const [name, url] of [['reasonix-official-site', 'https://reasonix.io/'], ['deepseek-official-site', 'https://www.deepseek.com/harness/']]) {
      for (let attempt = 0; ; attempt++) {
        try { await page.goto(url, { waitUntil: 'domcontentloaded', timeout: 25000 }); break; }
        catch (error) { if (attempt >= 2) throw error; await page.waitForTimeout(800); }
      }
      await page.waitForTimeout(2200);
      if (name === 'reasonix-official-site') {
        await page.evaluate(() => window.scrollTo(0, 760));
        await page.waitForTimeout(400);
      }
      await page.screenshot({ path: join(root, 'evidence', name + '.png'), fullPage: false });
      console.log('Captured official reference:', name);
    }
    process.exitCode = 0;
  } else if (process.argv.includes('--video')) {
    await page.close();
    const videoDir = await mkdtemp(join(tmpdir(), 'forge-refresh-motion-'));
    const context = await browser.newContext({ viewport: { width: 1080, height: 794 }, recordVideo: { dir: videoDir, size: { width: 1080, height: 794 } } });
    const movie = await context.newPage();
    await movie.goto(pathToFileURL(join(root, 'index.html')).href + '?recording=1&scene=home');
    const show = async (scene, ms) => { await movie.evaluate(scene => window.ForgeStudy.setScene(scene, true), scene); await movie.waitForTimeout(ms); };
    await show('home', 1200);
    await show('plan', 1600);
    await show('working', 3500);
    await show('approval', 1300);
    await movie.locator('#terminal').focus();
    await movie.keyboard.press('ArrowDown');
    await movie.waitForTimeout(500);
    await movie.keyboard.press('Enter');
    await movie.waitForTimeout(2200);
    await show('review', 2000);
    const choose = async (id, value) => movie.evaluate(({id,value}) => { const element=document.getElementById(id); element.value=value; element.dispatchEvent(new Event('change', {bubbles:true})); }, {id,value});
    await choose('size', '80x18');
    await movie.locator('#terminal').focus();
    await movie.keyboard.press('F6');
    await movie.waitForTimeout(1800);
    await choose('size', '120x40');
    await choose('theme', 'light');
    await movie.waitForTimeout(1200);
    await choose('theme', 'dark');
    await show('sessions', 1700);
    await show('palette', 1700);
    await show('terminal', 1500);
    await show('error', 1700);
    const video = movie.video();
    await context.close();
    await rename(await video.path(), join(root, 'motion.webm'));
    console.log('Recorded motion walkthrough, including a simulated explicit approval.');
  } else {
    await page.goto(pathToFileURL(join(root, 'index.html')).href);
    await page.waitForFunction(() => !!window.ForgeStudy);
    await page.click('#motion');
    const states = ['home', 'plan', 'working', 'review', 'approval', 'sessions', 'palette', 'terminal', 'error'];
    const dimensions = ['120x40', '160x50', '80x24', '80x18'];
    const snapshots = [];
    for (const theme of ['dark', 'light', 'mono']) {
      await page.selectOption('#theme', theme);
      for (const size of dimensions) {
        await page.selectOption('#size', size);
        for (const scene of states) {
          await page.evaluate(scene => window.ForgeStudy.setScene(scene), scene);
          const details = await page.evaluate(() => {
            const grid = window.ForgeStudy.grid();
            return { rows: grid.rows, cols: grid.cols, text: grid.cells.map(row => row.map(c => c.ch).join('')).join('\n'), focus: window.ForgeStudy.state.focus, hits: window.ForgeStudy.hits() };
          });
          assert.equal(details.cols + 'x' + details.rows, size);
          assert.ok(details.text.includes('FORGE'));
          assert.ok(details.hits.every(h => h.x >= 0 && h.y >= 0 && h.x + h.w <= details.cols && h.y + h.h <= details.rows), `${scene} ${size}: hit region outside frame`);
          assert.ok(details.text.includes('OpenAI'));
          if (scene === 'approval') {
            assert.equal(details.focus, 'approval');
            assert.ok(details.text.includes('Don’t run') && details.text.includes('Allow once'));
            assert.ok(details.text.includes('cargo test -p forge-model --locked retry'));
            assert.ok(details.text.includes('Paused'));
          }
          if (scene === 'error') assert.ok(details.text.includes('Retry this turn'));
          if (theme === 'dark' && size === '120x40') {
            await page.locator('.window').screenshot({ path: join(screenshots, `${scene}-dark-120x40.png`) });
            const svg = await page.evaluate(() => new XMLSerializer().serializeToString(document.getElementById('screen')));
            snapshots.push({ scene, svg });
          }
          if (['review', 'approval', 'sessions'].includes(scene) && ['80x18', '160x50'].includes(size) && theme === 'dark') {
            await page.locator('.window').screenshot({ path: join(screenshots, `${scene}-dark-${size}.png`) });
          }
          if (scene === 'review' && size === '120x40' && theme === 'light') {
            await page.locator('.window').screenshot({ path: join(screenshots, 'review-light-120x40.png') });
          }
        }
      }
    }
    await page.selectOption('#theme', 'dark');
    await page.selectOption('#size', '80x18');
    await page.evaluate(() => window.ForgeStudy.setScene('review'));
    await page.locator('#draft').fill('Also cover exhausted retries with a regression test.');
    await page.locator('#terminal').focus();
    await page.keyboard.press('F6');
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.view), 'diff');
    assert.equal(await page.inputValue('#draft'), 'Also cover exhausted retries with a regression test.');
    await page.selectOption('#size', '160x50');
    assert.equal(await page.inputValue('#draft'), 'Also cover exhausted retries with a regression test.');
    await page.evaluate(() => window.ForgeStudy.setScene('approval'));
    await page.locator('#terminal').focus();
    await page.keyboard.press('Enter');
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.scene), 'error', 'Default approval must decline');
    assert.equal(await page.inputValue('#draft'), 'Also cover exhausted retries with a regression test.');
    await page.evaluate(() => window.ForgeStudy.setScene('approval'));
    await page.locator('#terminal').focus();
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Enter');
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.scene), 'working');
    await page.evaluate(() => window.ForgeStudy.setScene('palette'));
    await page.locator('#terminal').focus();
    await page.keyboard.type('terminal');
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.query), 'terminal');
    await page.keyboard.press('Enter');
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.scene), 'terminal');
    await page.keyboard.press('Shift+Tab');
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.focus), 'composer');
    assert.equal(await page.inputValue('#draft'), 'Also cover exhausted retries with a regression test.');
    await page.evaluate(() => window.ForgeStudy.setScene('review'));
    await page.locator('#terminal').focus();
    await page.keyboard.press('Control+p');
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.scene), 'files');
    await page.keyboard.press('Enter');
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.sourceMode), true);
    await page.keyboard.press('ArrowRight');
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.diffCol), 4);
    await page.keyboard.press('Escape');
    assert.equal(await page.inputValue('#draft'), 'Also cover exhausted retries with a regression test.');
    await page.evaluate(() => { window.ForgeStudy.state.sourceMode = false; window.ForgeStudy.state.diffCol = 0; });
    await page.evaluate(() => window.ForgeStudy.setScene('sessions'));
    await page.locator('#terminal').focus();
    await page.keyboard.press('Space');
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.peek), true);
    await page.keyboard.press('Escape');
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.peek), false);
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.scene), 'sessions');
    await page.evaluate(() => window.ForgeStudy.setScene('review'));
    const exportPromise = page.waitForEvent('download');
    await page.click('#save-svg');
    const svgExport = await exportPromise;
    const exportPath = join(screenshots, 'review-export.svg');
    await svgExport.saveAs(exportPath);
    assert.ok((await readFile(exportPath, 'utf8')).includes('xmlns="http://www.w3.org/2000/svg"'));
    const pngPromise = page.waitForEvent('download');
    await page.click('#save-png');
    const pngExport = await pngPromise;
    const pngPath = join(screenshots, 'review-export.png');
    await pngExport.saveAs(pngPath);
    assert.equal((await readFile(pngPath)).subarray(0, 8).toString('hex'), '89504e470d0a1a0a');
    await page.selectOption('#size', '120x40');
    for (const size of dimensions) {
      await page.selectOption('#size', size);
      await page.click('#before');
      assert.equal(await page.evaluate(() => window.ForgeStudy.state.reference), 'forge-home-' + size);
      assert.ok(await page.evaluate(() => !!window.FORGE_REFERENCES[window.ForgeStudy.state.reference]));
      await page.click('#before');
    }
    await page.evaluate(() => window.ForgeStudy.setScene('review'));
    await page.selectOption('#size', '120x40');
    await page.screenshot({ path: join(root, 'overview.png'), fullPage: true });
    const reduced = await browser.newPage({ reducedMotion: 'reduce' });
    await reduced.goto(pathToFileURL(join(root, 'index.html')).href);
    assert.equal(await reduced.evaluate(() => window.ForgeStudy.state.motion), false);
    await reduced.close();
    await page.clock.install();
    await page.click('#play');
    await page.clock.runFor(11000);
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.scene), 'approval');
    await page.clock.runFor(20000);
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.scene), 'approval', 'Walkthrough must wait for a person');
    await page.locator('#terminal').focus();
    await page.keyboard.press('ArrowDown');
    await page.keyboard.press('Enter');
    await page.clock.runFor(4600);
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.scene), 'review');
    assert.equal(await page.evaluate(() => window.ForgeStudy.state.playing), false);
    const sheet = `<!doctype html><html><head><meta charset="utf-8"><style>*{box-sizing:border-box}body{margin:0;padding:44px;background:#101113;color:#edf0f5;font-family:-apple-system,BlinkMacSystemFont,sans-serif}h1{font-size:36px;letter-spacing:-1px;font-weight:550;margin:7px 0 13px}p{color:#aab5c6;font-size:13px}.kicker{font:11px monospace;letter-spacing:2px;color:#9fbbe4}.grid{display:grid;grid-template-columns:repeat(3,1fr);gap:27px 22px;margin-top:32px}article{min-width:0}header{display:flex;justify-content:space-between;font-size:12px;padding-bottom:9px}header span{font:10px monospace;color:#9fabbe}svg{width:100%;height:auto;display:block;background:#14171b;border:1px solid #3b4553;border-radius:5px}svg text{font-family:Menlo,Consolas,monospace;font-size:14px;white-space:pre}.hit{fill-opacity:0}footer{margin-top:30px;color:#a4b0c2;font-size:11px}</style></head><body><div class="kicker">FORGE / INTERFACE STUDY / 06 OCT 2026</div><h1>A quieter workspace. A clearer next action.</h1><p>Nine states · one character-cell design · all task content is simulated</p><div class="grid">${snapshots.map((s,i)=>`<article><header>${String(i+1).padStart(2,'0')} / ${s.scene}<span>120 × 40</span></header>${s.svg}</article>`).join('')}</div><footer>Dark, light and monochrome · 80×18 through 160×50 · interactive prototype in index.html</footer></body></html>`;
    await writeFile(join(root, 'contact-sheet.html'), sheet);
    await page.setViewportSize({ width: 1920, height: 900 });
    await page.goto(pathToFileURL(join(root, 'contact-sheet.html')).href);
    await page.screenshot({ path: join(root, 'contact-sheet.png'), fullPage: true });
    assert.deepEqual(errors, []);
    await writeFile(join(root, 'validation.json'), JSON.stringify({ checked: '2026-10-06', renders: states.length * dimensions.length * 3, states, dimensions, themes: ['dark', 'light', 'mono'], interactions: ['draft survives view switch and resize', 'default approval declines', 'explicit allow resumes', 'command query opens terminal', 'terminal returns to saved prompt', 'Ctrl+P opens files', 'file choice opens source', 'horizontal code panning', 'session peek and return', 'SVG and PNG export', 'actual before captures exist at each preset', 'OS reduced motion disables animation', 'walkthrough waits at approval', 'walkthrough resumes only after explicit choice'], browserErrors: errors }, null, 2) + '\n');
    console.log('PASS: 108 state/size/theme renders, draft retention, explicit approvals, command routing, before captures, reduced motion. Screenshots saved.');
  }
} finally {
  await browser.close();
}
