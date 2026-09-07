const tabs=[...document.querySelectorAll('[data-screen]')];
const panels=[...document.querySelectorAll('[data-screen-panel]')];
tabs.forEach(btn=>btn.addEventListener('click',()=>{
  tabs.forEach(b=>b.classList.toggle('active',b===btn));
  panels.forEach(p=>p.classList.toggle('active',p.dataset.screenPanel===btn.dataset.screen));
}));

const previews={
  parser:{kicker:'NEEDS YOU',title:'parser-fix',status:'Waiting for approval',branch:'forge/parser-fix-21',worktree:'~/.forge/local/worktrees/task-21-parser-fix',model:'OpenAI · GPT-5.6 Sol · High',session:'8ab62b5e…21d3',body:'Parser tests expose one generated-file edit outside the expected set. The agent is asking permission before overwriting it.'},
  auth:{kicker:'WORKING',title:'auth-refresh',status:'Running tests',branch:'forge/auth-refresh-17',worktree:'~/.forge/local/worktrees/task-17-auth-refresh',model:'OpenAI · GPT-5.6 Sol · High',session:'57092c11…44a8',body:'Lease ownership is fixed. The agent is running the focused refresh tests and checking one concurrency edge case.'},
  index:{kicker:'WORKING',title:'index-cache',status:'Editing',branch:'forge/index-cache-24',worktree:'~/.forge/local/worktrees/task-24-index-cache',model:'OpenCode Go · Qwen3.8 Flash · Medium',session:'7e1c380f…b9c2',body:'The task is replacing repeated full-index scans with a bounded cache and updating invalidation tests.'},
  main:{kicker:'READY',title:'main',status:'Ready',branch:'main',worktree:'~/src/forge',model:'OpenAI · GPT-5.6 Sol · High',session:'1cba118a…e071',body:'Primary repository session is idle and ready for the next prompt.'},
  api:{kicker:'READY',title:'api-cleanup',status:'Stopped',branch:'forge/api-cleanup-22',worktree:'~/.forge/local/worktrees/task-22-api-cleanup',model:'OpenAI · GPT-5.6 Sol · High',session:'d7096b4c…02fa',body:'Cleanup is stopped with its changes preserved in the task worktree.'},
  billing:{kicker:'UNAVAILABLE',title:'billing-retry',status:'Branch drift',branch:'forge/billing-retry-18',worktree:'~/.forge/local/worktrees/task-18-billing-retry',model:'OpenAI · GPT-5.6 Sol · High',session:'99ed5db0…f118',body:'The worktree branch no longer matches the immutable session binding. Forge will not resume this task until the drift is resolved externally.'}
};
function renderPreview(id){
  const p=previews[id]||previews.parser;
  const box=document.getElementById('task-preview');
  if(!box)return;
  const warning=id==='parser';
  box.innerHTML=`
    <div class="preview-kicker">${p.kicker}</div>
    <h2>${p.title}</h2>
    <div class="preview-status"><span class="marker ${warning?'wait':id==='billing'?'err':id==='auth'||id==='index'?'run':'idle'}">${warning?'[?]':id==='billing'?'[!]':id==='auth'||id==='index'?'[>]':'[ ]'}</span> ${p.status}</div>
    <dl>
      <div><dt>Branch</dt><dd>${p.branch}</dd></div>
      <div><dt>Worktree</dt><dd>${p.worktree}</dd></div>
      <div><dt>Model</dt><dd>${p.model}</dd></div>
      <div><dt>Session</dt><dd>${p.session}</dd></div>
    </dl>
    <div class="preview-section"><div class="section-label">LATEST TURN</div><p>${p.body}</p></div>
    ${warning?'<div class="approval-card"><div><span class="marker wait">[?]</span><b>Approval required</b></div><p>Write generated grammar snapshot<br><code>tests/fixtures/parser.snap</code></p><div class="approval-actions"><span>[A] Approve once</span><span>[D] Deny</span></div></div>':''}
    <div class="preview-section"><div class="section-label">RECENT ACTIVITY</div><ul class="recent"><li><span>[✓]</span><b>Latest repository operation</b><small>Task-local state preserved</small></li><li><span>[✓]</span><b>Workspace binding</b><small>${p.branch}</small></li></ul></div>`;
}
document.querySelectorAll('.task-row').forEach(row=>row.addEventListener('click',()=>{
  document.querySelectorAll('.task-row').forEach(r=>r.classList.toggle('selected',r===row));
  renderPreview(row.dataset.rowTask);
}));
const search=document.getElementById('task-search');
if(search)search.addEventListener('input',()=>{
  const q=search.value.toLowerCase();
  document.querySelectorAll('.task-row').forEach(row=>{
    row.style.display=row.innerText.toLowerCase().includes(q)?'grid':'none';
  });
});
