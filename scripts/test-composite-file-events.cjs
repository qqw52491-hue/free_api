// Real session event handling and historical normalization, mocked Tauri I/O.
const assert = require('node:assert/strict');
const path = require('node:path');
const { load, session, emit } = require('./test-agent-task-resume.cjs');
const { computeParseStepFileChanges: files } = load(path.resolve(__dirname, '../src/components/agent/diff/stepFileParser.ts'));
const params = {action:'edit',path:'a.txt',target_content:'old',replacement_content:'new'};
const view = {kind:'tool_results',tool_views:[
    {tool:'local_file',params,success:true,route:'local_file:edit:success',view:{kind:'diff',path:'a.txt',start_line:4}},
    {tool:'local_file',params:{...params,path:'failed.txt'},success:false,route:'local_file:edit:failed'},
]};
async function main() {
    const s = await session('failed');
    await emit('channel-run-started', {session_id:'existing-session',run_id:'current',goal:'edit files'});
    await emit('agent-progress-existing-session', {type:'step_new',step:{id:0,tool:'pipeline',command:{commands:[]}}});
    await emit('agent-progress-existing-session', {type:'step_error',step_id:0,output:'second call failed',view});
    const live = files(s.steps.value[0]);
    assert.equal(live.chips.length,1);
    assert.equal(live.chips[0].path,'a.txt');
    assert.equal(live.chips[0].isIntercepted,false);
    const restored = await session('failed',{traces:[{
        id:'current',goal:'edit files',status:'failed',started_at:'2026-10-28T02:00:00Z',
        updated_at:'2026-10-28T02:01:00Z',steps:[{id:'0-attempt-1',tool:'pipeline',status:'failed',output:'second call failed',view}],
    }]});
    const history = files(restored.historyTurns.value[0].steps[0]);
    assert.equal(history.chips.length,1);
    assert.equal(history.chips[0].newContent,'new');
    console.log('PASS: partial-failure file receipts survive live events and historical reload');
}
main().then(() => process.exit(0), error => { console.error(error); process.exit(1); });
