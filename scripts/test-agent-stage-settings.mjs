import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(new URL('../src/components/agent/stageSettings.ts', import.meta.url), 'utf8');
const js = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 },
}).outputText;
const { stageReviewerIsDisabled, withFixedStageChange } = await import(
  'data:text/javascript;base64,' + Buffer.from(js).toString('base64')
);

test('fixed stage selection takes precedence over legacy disabled values', () => {
  for (const legacy of [undefined, 'disabled', ' NONE ']) {
    for (const flag of [undefined, true, false]) {
      assert.equal(stageReviewerIsDisabled({ stages: { stage_review: flag }, stage_reviewer_override: legacy }),
        typeof flag === 'boolean' ? !flag : legacy !== undefined);
    }
  }
  assert.equal(stageReviewerIsDisabled({ stage_reviewer_override: 'review-role' }), false);
});

test('one fixed-stage control restores auto without losing a selected reviewer', () => {
  for (const sentinel of ['disabled', ' NONE ']) {
    const old = { stages: { plan:true }, stage_reviewer_override:sentinel, stage_review_committee:true };
    const off = withFixedStageChange(old,'stage_review','off',{plan:true,stage_review:false});
    assert.equal(stageReviewerIsDisabled(off),true);
    const auto = withFixedStageChange(off,'stage_review','auto',{plan:true});
    assert.equal(stageReviewerIsDisabled(auto),false);
    assert.equal(auto.stage_reviewer_override,undefined);
    assert.equal(auto.stage_review_committee,true);
    assert.equal(old.stage_reviewer_override,sentinel);
  }
  const named = { stages: {plan:true}, stage_reviewer_override:'review-role' };
  const off = withFixedStageChange(named,'stage_review','off',{plan:true,stage_review:false});
  assert.equal(off.stage_reviewer_override,'review-role');
  const auto = withFixedStageChange(off,'stage_review','auto',{plan:true});
  assert.equal(auto.stage_reviewer_override,'review-role');
  assert.equal(stageReviewerIsDisabled(auto),false);
  const untouched = withFixedStageChange({stages:{},stage_reviewer_override:'disabled'},'final_review','on',{final_review:true});
  assert.equal(untouched.stage_reviewer_override,'disabled');
});
