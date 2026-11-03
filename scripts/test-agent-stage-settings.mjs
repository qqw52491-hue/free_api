import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

const source = readFileSync(new URL('../src/components/agent/stageSettings.ts', import.meta.url), 'utf8');
const js = ts.transpileModule(source, {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.ES2022 },
}).outputText;
const { stageReviewerIsDisabled, withStageTriggerMode } = await import(
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

test('legacy close button also disables an explicitly enabled fixed stage', () => {
  const original = { stages: { plan: true, stage_review: true, final_review: false } };
  const updated = withStageTriggerMode(original, 'disabled');
  assert.equal(updated.stages.stage_review, false);
  assert.equal(stageReviewerIsDisabled(updated), true);
  assert.equal(updated.stages.plan, true);
  assert.equal(updated.stages.final_review, false);
  assert.equal(original.stages.stage_review, true);
});

test('automatic trigger presets reset both stage controls and preserve other settings', () => {
  for (const flag of [true, false, undefined]) {
    for (const mode of ['auto', 'cross_review']) {
      const updated = withStageTriggerMode({
        stages: { plan: false, plan_review: false, stage_review: flag, final_review: true },
        stage_reviewer_override: 'disabled', stage_review_committee: true,
      }, mode);
      assert.deepEqual(updated.stages, { plan: false, plan_review: false, final_review: true });
      assert.equal(updated.stage_reviewer_override, undefined);
      assert.equal(updated.stage_review_committee, true);
      assert.equal(stageReviewerIsDisabled(updated), false);
    }
  }
});
