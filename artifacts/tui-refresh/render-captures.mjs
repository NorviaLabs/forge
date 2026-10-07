// Render buffers captured by the opt-in Rust refresh_capture harness.
// Usage: node artifacts/tui-refresh/render-captures.mjs CAPTURE_DIR OUTPUT_HTML [BASELINE_DIR]
import { readFile, readdir, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';

const [input, output, baseline] = process.argv.slice(2);
if (!input || !output) throw new Error('Pass CAPTURE_DIR OUTPUT_HTML [BASELINE_DIR]');
async function frames(directory) {
  const result = {};
  for (const name of (await readdir(directory)).sort()) {
    if (!name.endsWith('.json') || name === 'timings.json') continue;
    const frame = JSON.parse(await readFile(join(directory, name), 'utf8'));
    if (frame.cells) {
      const styles = [], styleIds = new Map();
      const cells = frame.cells.map(cell => {
        const style = [cell.fg, cell.bg, !!cell.bold, !!cell.inverse];
        const key = JSON.stringify(style);
        if (!styleIds.has(key)) { styleIds.set(key, styles.length); styles.push(style); }
        return [cell.text ?? cell.symbol ?? ' ', styleIds.get(key)];
      });
      const runs = [];
      for (const [text, style] of cells) {
        const last = runs.at(-1);
        if (last && last[0] === text && last[1] === style) last[2]++;
        else runs.push([text, style, 1]);
      }
      result[name.replace('.json', '')] = { width: frame.width, height: frame.height, styles, runs };
    }
  }
  return result;
}
const data = { after: await frames(input), before: baseline ? await frames(baseline) : {} };
// The raw cells remain the evidence. The browser uses representative monospace
// typography; it does not reproduce a particular user's terminal font.
const html = `<!doctype html><html lang="en"><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Forge compiled UI captures</title><style>
body{margin:0;padding:24px;background:#101318;color:#edf0f5;font:14px system-ui}
h1{margin:0 0 8px;font-size:24px}p{color:#b5becb;max-width:900px;line-height:1.5}
select{font:inherit;padding:8px;background:#1d2229;color:inherit;border:1px solid #394350;border-radius:6px}
label{display:inline-block;margin:8px 12px 16px 0}canvas{display:block;max-width:100%;height:auto;border:1px solid #394350}
.pair{display:flex;gap:24px;flex-wrap:wrap;width:max-content;max-width:100%}h2{font-size:14px;font-weight:500}
</style><h1>Forge — compiled UI captures</h1>
<p>Real Ratatui draw output. Job and child fixtures execute disposable commands; provider responses and narrated plans are mocked. Dirty child edits are test setup. Rendering evidence, not measured usability or model quality. Font: representative system monospace.</p>
<label>State <select id="state"></select></label><label>Size <select id="size"></select></label><label>Theme <select id="theme"></select></label>
<div class="pair"><section id="before"><h2>Previous build</h2><canvas></canvas></section><section id="after"><h2>Refresh</h2><canvas></canvas></section></div>
<script>const frames=${JSON.stringify(data).replaceAll('<', '\\u003c')};
const names=Object.keys(frames.after), state=document.querySelector('#state'), size=document.querySelector('#size'), theme=document.querySelector('#theme');
const parts=names.map(name=>name.split('-'));
for(const [select,index] of [[state,0],[size,2],[theme,1]])for(const value of [...new Set(parts.map(p=>p[index]))])select.add(new Option(value,value));
state.value='start';size.value='120x40';theme.value='dark';
function color(value,fallback){const rgb=/Rgb\\((\\d+), (\\d+), (\\d+)\\)/.exec(value);if(rgb)return 'rgb('+rgb.slice(1).join(',')+')';return ({Black:'#000',White:'#fff',Gray:'#bbb',DarkGray:'#666',Red:'#e66',Green:'#6c8',Yellow:'#ed8',Blue:'#68e',Cyan:'#6cc',Magenta:'#c8d'})[value]||fallback;}
function render(which,key){const pane=document.getElementById(which),frame=frames[which][key];pane.hidden=!frame;if(!frame)return;const cells=frame.runs.flatMap(([text,style,count])=>Array.from({length:count},()=>[text,style]));const canvas=pane.querySelector('canvas'),ctx=canvas.getContext('2d'),cw=9,ch=18,dpr=2;canvas.width=frame.width*cw*dpr;canvas.height=frame.height*ch*dpr;canvas.style.width=frame.width*cw+'px';ctx.scale(dpr,dpr);const resetBg=key.includes('-light-')?'#fff':'#14171b',resetFg=key.includes('-light-')?'#202936':'#edf0f5';
cells.forEach((cell,i)=>{const [fg,bg,bold,inverse]=frame.styles[cell[1]];ctx.fillStyle=color(inverse?fg:bg,inverse?resetFg:resetBg);ctx.fillRect(i%frame.width*cw,Math.floor(i/frame.width)*ch,cw,ch);});
cells.forEach((cell,i)=>{const [fg,bg,bold,inverse]=frame.styles[cell[1]];ctx.fillStyle=color(inverse?bg:fg,inverse?resetBg:resetFg);ctx.font=(bold?'bold ':'')+'14px ui-monospace, SFMono-Regular, Menlo, monospace';ctx.fillText(cell[0],i%frame.width*cw,Math.floor(i/frame.width)*ch+14);});}
function draw(){for(const option of state.options)option.disabled=!frames.after[option.value+'-'+theme.value+'-'+size.value];if(state.selectedOptions[0].disabled)state.value=[...state.options].find(option=>!option.disabled).value;const key=state.value+'-'+theme.value+'-'+size.value;render('before',key);render('after',key);document.title='Forge '+key;}
for(const select of [state,size,theme])select.onchange=draw;draw();
</script></html>`;
await writeFile(resolve(output), html);
console.log(`Rendered ${Object.keys(data.after).length} captured frames: ${resolve(output)}`);
