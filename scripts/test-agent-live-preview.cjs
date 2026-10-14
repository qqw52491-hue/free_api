// Real stream-event path and Vue computed projections; mocked Tauri, no live model calls.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const {load, session, emit} = require('./test-agent-task-resume.cjs');
const base = path.resolve(__dirname, '../src/components/agent');
const {createIncrementalThoughtPreview, getWorkerLiveLine} = load(path.join(base, 'liveThoughtPreview.ts'));
const {extractActiveThoughtLineInfo: full} = load(path.join(base, 'diff/stagePreview.ts'));

async function main() {
    const fixtures = [
        '', '检查登录\n验证上传\n', '\n\n### 标题\n- **检查** `接口`\n1. 下一步\n',
        '普通文本\r\n> 引用\r\n\r\n末行', '🔎 检查𠮷和中文\n结果正确',
        '前文\n```rust\nlet x = 1;\n```\n后文', '同一行```json\n{}\n```后续\n尾行',
        '前文\n```rust\n尚未闭合\n继续输出', '<think>检查\n下一步</think>\n结束',
        '前文<a\n title="内容">正文</a>\n后文', 'a < b\n不是标签\n最后一行',
        '# \n- \n1. \n\n真实内容', 'inline `code` and ``ticks``\nlast',
    ];
    let parityChecks = 0;
    for (const raw of fixtures) for (const width of [1,2,3,7,31,256]) {
        const tracker = createIncrementalThoughtPreview();
        let prefix = '';
        assert.deepEqual(tracker.append(''), full('',120));
        for (let i=0;i<raw.length;i+=width) {
            const delta = raw.slice(i,i+width); prefix += delta;
            assert.deepEqual(tracker.append(delta),full(prefix,120), `prefix parity: width=${width}, ${JSON.stringify(prefix)}`);
            parityChecks++;
        }
    }
    // Deterministic mixed-markup/chunk boundaries guard normalization parity.
    let seed = 1297;
    const random = n => { seed = (Math.imul(seed,1664525)+1013904223)>>>0; return seed%n; };
    const parts = ['字','abc','\n','\r\n',' ','```','`','``','<think>','</think>',
        '<tag x="y\n z">','<','>','- ','### ','𠮷','*','_','<tag','</','\t'];
    for (let trial=0; trial<500; trial++) {
        let source='';
        for (let i=0;i<50;i++) source += parts[random(parts.length)];
        const preview = createIncrementalThoughtPreview();
        let prefix='';
        for (let pos=0;pos<source.length;) {
            const width=1+random(15), chunk=source.slice(pos,pos+width);
            pos += width; prefix += chunk;
            assert.deepEqual({...preview.append(chunk)},{...full(prefix,120)},JSON.stringify(prefix));
            parityChecks++;
        }
    }
    let processed = 0, largestInput = 0;
    const tracker = createIncrementalThoughtPreview(120, (raw,maxLen) => {
        processed += raw.length; largestInput = Math.max(largestInput,raw.length); return full(raw,maxLen);
    });
    const long = '检查接口授权，并验证失败不会覆盖已有许可证。\n'.repeat(3000);
    for(let i=0;i<long.length;i+=64) tracker.append(long.slice(i,i+64));
    assert.deepEqual(tracker.append(''),full(long,120));
    assert(processed < long.length*4, 'completed lines must not be rescanned as the stream grows');
    assert(largestInput < 160, 'normalization never receives the accumulated multi-line document');
    const single = createIncrementalThoughtPreview();
    assert.deepEqual(single.append('长'.repeat(60000)),full('长'.repeat(60000),120), 'long unfinished lines remain correct');
    assert.deepEqual(single.append('\n结束'),full('长'.repeat(60000)+'\n结束',120));

    const s = await session('failed');
    await emit('channel-run-started',{session_id:'existing-session',goal:'incremental preview regression'});
    const progress = p => emit('agent-progress-existing-session',p);
    const a='preview-reviewer-a', b='preview-reviewer-b';
    const stream = (id,kind,chunk) => progress({type:'child_reviewer_stream',child_run_id:id,role_label:'技术主管',stream_type:kind,chunk});
    stream(a,'thinking','');
    let w=s.teamWorkers.value.find(w=>w.id===a);
    w.currentStep='生成审查判决';w.currentTool='deliberate';
    const placeholder=getWorkerLiveLine(w);
    w.steps=[{title:'irrelevant step'}];
    assert.equal(getWorkerLiveLine(w).key,placeholder.key, 'identical placeholder must not flip on hidden step changes');
    let raw='';
    for (const chunk of ['检查登录\n','核查上传','\n已确认失败保留旧许可证']) {raw+=chunk;stream(a,'thinking',chunk);}
    assert.equal(w.thought,raw,'complete thought is preserved');
    assert.deepEqual({...w.livePreview},{...full(raw,120),streamType:'thinking'});
    assert(getWorkerLiveLine(w).text.includes('已确认失败保留旧许可证'), 'actual review text replaces deliberate placeholder');
    const streamKey=getWorkerLiveLine(w).key;
    stream(a,'thinking','\n检查下一项');
    assert.equal(getWorkerLiveLine(w).key,streamKey,'new stream lines update in place, not via repeated remounts');
    stream(b,'thinking','另一个审查员独立内容');
    assert.equal(s.teamWorkers.value.find(w=>w.id===b).livePreview.text,'另一个审查员独立内容');
    assert(!w.livePreview.text.includes('另一个'),'reviewers never share preview state');
    stream(a,'content','{"approved":');stream(a,'content','false}');
    assert.equal(w.output,'{"approved":false}');
    assert.equal(w.livePreview.streamType,'content');
    assert(getWorkerLiveLine(w).text.includes('approved'), 'content output is also visible');
    progress({type:'child_reviewer_action',child_run_id:a,action:'read_file',params:{path:'src/LicenseService.java'},step:1});
    assert.equal(w.livePreview,undefined,'a real tool action supersedes the earlier stream');
    const action=getWorkerLiveLine(w);
    assert(action.text.includes('LicenseService.java'));
    stream(a,'thinking','\n完成文件核查');
    assert(getWorkerLiveLine(w).text.includes('完成文件核查'),'new thinking supersedes the last read action');
    assert.notEqual(getWorkerLiveLine(w).key,action.key);
    stream(a,'thinking','\n'+long);
    assert(w.thought.endsWith(long),'large source must not be truncated');

    // Execute the actual component's computed, with many steps pointing to the same child.
    const source=fs.readFileSync(path.join(base,'AgentChatStream.vue'),'utf8').split('<script setup lang="ts">')[1].split('</script>')[0];
    const ast=ts.createSourceFile('component.ts',source,ts.ScriptTarget.Latest,true,ts.ScriptKind.TS);
    const declarations=ast.statements.filter(n=>ts.isVariableStatement(n)&&n.declarationList.declarations.some(d=>d.name.getText(ast)==='workerLiveLines'));
    assert.equal(declarations.length,1);
    const props=vue.reactive({steps:Array.from({length:40},(_,i)=>({id:`step-${i}`,tool:i%2?'wait_jobs':'child_reviewer'}))});
    let selections=0;
    const context={computed:vue.computed,props,isSubAgentTool:t=>t==='child_reviewer',isWaitJobsTool:t=>t==='wait_jobs',
        getStepActiveWorkers:()=>[w],getWorkerLiveLine:item=>{selections++;return getWorkerLiveLine(item);}};
    const code=ts.transpileModule(declarations[0].getText(ast)+'\nworkerLiveLines;',{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText;
    const lines=vm.runInNewContext(code,context);
    const first=lines.value;
    assert.equal(selections,1,'one worker projection, not 40 duplicate projections');
    assert.equal(first.get('step-0'),first.get('step-39'));
    stream(a,'thinking','\n'+'x'.repeat(120));
    const frozen=lines.value; const before=selections;
    for(let i=0;i<100;i++) {stream(a,'thinking','x');assert.equal(lines.value,frozen);}
    assert.equal(selections,before,'unchanged visible preview does not invalidate the step preview map');
    assert(w.thought.endsWith('x'.repeat(220)), 'raw stream keeps growing even when preview is unchanged');

    // Reusing a child id in a new run must not reuse the previous run's accumulator.
    s.clearSession();
    await emit('channel-run-started',{session_id:'existing-session',goal:'new run'});
    stream(a,'thinking','全新任务');
    w=s.teamWorkers.value.find(w=>w.id===a);
    assert.equal(w.thought,'全新任务');assert.equal(w.livePreview.text,'全新任务');
    stream(b,'thinking','');
    const restored=s.teamWorkers.value.find(w=>w.id===b);restored.thought='恢复前缀\n';
    stream(b,'thinking','续接内容');
    assert.deepEqual({...restored.livePreview},{...full(restored.thought,120),streamType:'thinking'},'existing source seeds once before append');
    console.log(`PASS: ${parityChecks} prefix-parity checks; long stream normalization ${processed}/${long.length} characters, max input ${largestInput}; real event preservation/isolation/reset, stable ticker, 40-step dedupe, 100-chunk reactivity`);
}
main().then(()=>process.exit(0),e=>{console.error(e);process.exit(1)});
