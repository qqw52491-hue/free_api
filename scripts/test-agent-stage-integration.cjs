// Exercise the actual StageSettingsFields change handler and Composer routing handler together.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const ts = require('typescript');
const vm = require('node:vm');
function functions(path, names, env) {
  const script = fs.readFileSync(path,'utf8').split('<script setup lang="ts">')[1].split('</script>')[0];
  const ast = ts.createSourceFile(path,script,ts.ScriptTarget.Latest,true,ts.ScriptKind.TS);
  const fns = ast.statements.filter(n => ts.isFunctionDeclaration(n) && names.includes(n.name?.text));
  assert.equal(fns.length,names.length);
  const js = ts.transpileModule(fns.map(n=>n.getText(ast)).join('\n')+'\n({'+names.join(',')+'})', {compilerOptions:{target:ts.ScriptTarget.ES2022}}).outputText;
  return vm.runInNewContext(js,env);
}
const helperSource = fs.readFileSync('src/components/agent/stageSettings.ts','utf8');
const helperJS = ts.transpileModule(helperSource,{compilerOptions:{module:ts.ModuleKind.CommonJS,target:ts.ScriptTarget.ES2022}}).outputText;
const sandbox = {exports:{}}; vm.runInNewContext(helperJS,sandbox); const helper = sandbox.exports;
const locked={value:false};
const parent={modelRouting:{stages:{plan:true},stage_reviewer_override:'disabled',stage_review_committee:true}};
let emissions=[];
const composer=functions('src/components/agent/AgentComposer.vue',['setFixedStage'],{workflowLocked:locked,props:parent,withFixedStageChange:helper.withFixedStageChange,emit:(name,value)=>{emissions.push(name);parent.modelRouting=value;}});
const fieldProps={modelValue:parent.modelRouting.stages,approval:false,legacyStageReviewDisabled:true};
const fields=functions('src/components/agent/StageSettingsFields.vue',['mode','change'],{props:fieldProps,emit:(name,...args)=>{if(name==='stage-change') composer.setFixedStage(...args);}});
function sync(){fieldProps.modelValue=parent.modelRouting.stages;fieldProps.legacyStageReviewDisabled=helper.stageReviewerIsDisabled(parent.modelRouting);}
assert.equal(fields.mode('stage_review'),'off','legacy disabled is displayed as off');
fields.change('stage_review','auto');sync();
assert.equal(fields.mode('stage_review'),'auto'); assert.equal(helper.stageReviewerIsDisabled(parent.modelRouting),false);
assert.equal(parent.modelRouting.stage_reviewer_override,undefined); assert.equal(parent.modelRouting.stage_review_committee,true);
fields.change('stage_review','off');sync();assert.equal(fields.mode('stage_review'),'off');assert.equal(helper.stageReviewerIsDisabled(parent.modelRouting),true);
fields.change('stage_review','auto');sync();assert.equal(fields.mode('stage_review'),'auto');assert.equal(helper.stageReviewerIsDisabled(parent.modelRouting),false);
parent.modelRouting.stage_reviewer_override='review-role';fields.change('stage_review','off');sync();assert.equal(parent.modelRouting.stage_reviewer_override,'review-role');
fields.change('stage_review','auto');sync();assert.equal(parent.modelRouting.stage_reviewer_override,'review-role');
locked.value=true; const old=JSON.stringify(parent.modelRouting), count=emissions.length;fields.change('stage_review','off');
assert.equal(JSON.stringify(parent.modelRouting),old,'locked resume cannot change current task settings');assert.equal(emissions.length,count);
console.log('PASS: actual stage field → composer integration, legacy fallback, reviewer preservation, resume lock');
