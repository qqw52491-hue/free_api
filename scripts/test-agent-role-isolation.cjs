// Exercise the actual role editor handlers with a stubbed AI response.
const assert = require('node:assert/strict');
const fs = require('node:fs');
const ts = require('typescript');
const vm = require('node:vm');
const source = fs.readFileSync('src/views/RoleManagerView.vue', 'utf8').split('<script setup lang="ts">')[1].split('</script>')[0];
const ast = ts.createSourceFile('role.ts', source, ts.ScriptTarget.Latest, true, ts.ScriptKind.TS);
const names = ['generateWithAi', 'applyPreset'];
const functions = ast.statements.filter(n => ts.isFunctionDeclaration(n) && names.includes(n.name?.text));
assert.equal(functions.length, names.length);
const js = ts.transpileModule(functions.map(n => n.getText(ast)).join('\n') + '\n({' + names.join(',') + '});', {compilerOptions: {target: ts.ScriptTarget.ES2022}}).outputText;
const ref = value => ({value});
const workflow = JSON.stringify({profile:'task',execution:{allowed_tools:['read_file'],stages:{plan:true,final_review:true},workflow:{plan_mode:'plan_approval',completion:'delivery'}}});
const form = {name:'existing',label:'Existing',model_id:'role-model',persona_prompt:'original',allowed_kinds:['task'],scope_hints:'',default_test_command:'',focus_domains:[],input_fields:[],schema_fields:[],execution_defaults:workflow};
const env = {form, isCreating:ref(false), isEditing:ref(true), aiPrompt:ref('只微调职责'), aiModelId:ref('editor-model'), aiChatHistory:ref([]), aiKindHints:ref([]), aiGenerating:ref(false), feedbackMsg:ref(''), feedbackIsError:ref(false), setTimeout:()=>{}};
let answer = false;
const confirmations = [], calls = [];
const handlers = vm.runInNewContext(js, {...env, window:{confirm:message=>{confirmations.push(message);return answer;}}, invoke:async(name,params)=>{calls.push({name,params});return {reply:'updated',card:{name:'renamed',label:'Updated',persona_prompt:'updated'}};}});
async function main() {
  for (const selected of ['role-model','']) {
    form.model_id = selected;
    env.aiPrompt.value = '只微调职责';
    await handlers.generateWithAi();
    assert.equal(calls.at(-1).params.modelId,'editor-model');
    assert.equal(form.model_id,selected,'AI editor choice must not bind or replace the runtime model');
    assert.equal(form.name,'existing');
    assert.equal(form.execution_defaults,workflow,'AI persona edits retain permissions and approval policy');
  }
  const template = {name:'new_template',label:'Template',allowed_kinds:'worker',persona_prompt:'template',scope_hints:'',default_test_command:''};
  form.model_id = 'role-model';
  const before = JSON.stringify(form);
  handlers.applyPreset(template);
  assert.equal(JSON.stringify(form),before,'cancelling template replacement retains the entire draft');
  assert.equal(env.isEditing.value,true);
  assert.match(confirmations.at(-1),/工具.*流程/);
  answer = true;
  handlers.applyPreset(template);
  assert.equal(form.name,'new_template');
  assert.equal(form.model_id,'','a new template cannot inherit the previous role model');
  assert.equal(JSON.parse(form.execution_defaults).profile,'worker');
  assert.equal(env.isEditing.value,false);
  assert.equal(env.isCreating.value,true);
  env.isCreating.value=false;
  const count = confirmations.length;
  handlers.applyPreset(template);
  assert.equal(confirmations.length,count,'loading into an inactive editor needs no replacement confirmation');
  console.log('PASS: role AI editor model isolation; template replacement confirmation and stale model reset');
}
main().catch(error=>{console.error(error);process.exitCode=1;});
