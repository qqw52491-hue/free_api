// Run: node scripts/test-github-updater.mjs
import assert from 'node:assert/strict';
import fs from 'node:fs';
import vm from 'node:vm';
import ts from 'typescript';
import { createRequire } from 'node:module';
import os from 'node:os';
import path from 'node:path';
import { spawnSync, execFileSync } from 'node:child_process';
import { mergeManifests, targets } from './merge-updater-manifests.mjs';
import { createManifest } from './create-updater-manifest.mjs';
const require = createRequire(import.meta.url);
const vue = require('vue');
const source = ts.transpileModule(fs.readFileSync('src/composables/useUpdater.ts', 'utf8'), {
  compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 },
}).outputText;
const deferred = () => { let resolve, reject; const promise = new Promise((yes, no) => { resolve = yes; reject = no; }); return { promise, resolve, reject }; };
function load({ check, relaunch = async () => {}, bundle = 'app' }) {
  const module = { exports: {} }; let timer;
  vm.runInNewContext(source, {
    module, exports: module.exports,
    require(name) {
      if (name === 'vue') return vue;
      if (name === '@tauri-apps/plugin-updater') return { check };
      if (name === '@tauri-apps/plugin-process') return { relaunch };
      if (name === '@tauri-apps/api/app') return { getBundleType: async () => bundle };
      throw new Error(`Unexpected dependency ${name}`);
    },
    window: { setInterval(callback) { timer = callback; return 1; } },
    document: { hidden: false },
  });
  return { state: module.exports, tick: () => timer() };
}
const update = (install, extra = {}) => ({ version: '0.2.0', close: async () => {}, downloadAndInstall: install, ...extra });
let count = 0;
{
  const pending = deferred();
  const { state } = load({ check: () => { count++; return pending.promise; } });
  const first = state.checkUpdateOnce();
  assert.equal(state.checkUpdateOnce(), first, 'manual check must share the pending background check');
  const found = update(async () => {}); pending.resolve(found);
  assert.equal(await first, found); assert.equal(count, 1);
  assert.equal(state.updateChecking.value, false);
}
{
  let fail = true;
  const { state } = load({ check: async () => { if (fail) throw new Error('offline'); return null; } });
  await assert.rejects(state.checkUpdateOnce());
  assert.match(state.updateError.value, /offline/);
  assert.doesNotMatch(state.updateStatusText.value, /最新/);
  fail = false; await state.checkUpdateOnce();
  assert.equal(state.updateError.value, ''); assert.equal(state.updateStatusText.value, '已是最新版本');
}
{
  let installs = 0, restarts = 0, closed = 0;
  const installed = deferred();
  const found = update(async callback => {
    installs++;
    callback({ event: 'Started', data: { contentLength: 100 } });
    callback({ event: 'Progress', data: { chunkLength: 40 } });
    assert.equal(state.downloadProgress.value, 40);
    callback({ event: 'Progress', data: { chunkLength: 60 } });
    callback({ event: 'Finished' });
    await installed.promise;
  });
  const { state } = load({ check: async () => found, relaunch: async () => { restarts++; throw new Error('restart unavailable'); } });
  state.availableUpdate.value = update(async () => {}, { close: async () => { closed++; } });
  await state.checkUpdateOnce(); assert.equal(closed, 1);
  const first = state.installAvailableUpdate();
  await state.installAvailableUpdate(); assert.equal(installs, 1, 'double click must not install twice');
  installed.resolve(); await first;
  assert.equal(state.availableUpdate.value, null); assert.equal(state.updateInstalled.value, true);
  assert.match(state.updateError.value, /重启失败/); assert.equal(restarts, 1);
  await state.installAvailableUpdate(); assert.equal(installs, 1);
  await state.restartAfterUpdate(); assert.equal(restarts, 2);
}
{
  let installs = 0, restarts = 0;
  const { state } = load({
    check: async () => update(async () => { installs++; throw new Error('signature mismatch'); }),
    relaunch: async () => { restarts++; },
  });
  await state.checkUpdateOnce(); await state.installAvailableUpdate();
  assert.match(state.updateError.value, /signature mismatch/);
  assert.equal(restarts, 0); assert.equal(state.updateInstalled.value, false);
  assert.ok(state.availableUpdate.value); assert.equal(state.updateInstalling.value, false);
  assert.equal(installs, 1);
}
{
  let installs = 0;
  const { state } = load({ check: async () => update(async () => { installs++; }), bundle: 'deb' });
  await state.checkUpdateOnce(); await state.installAvailableUpdate();
  assert.equal(state.manualUpdateRequired.value, true); assert.equal(installs, 0);
  assert.match(state.updateError.value, /发布页/);
}
{
  let checks = 0;
  const { state, tick } = load({ check: async () => { checks++; throw new Error('not published'); } });
  state.startUpdateWatcher(); state.startUpdateWatcher();
  await state.checkUpdateOnce().catch(() => {});
  assert.equal(checks, 1);
  tick(); await state.checkUpdateOnce().catch(() => {}); assert.equal(checks, 2);
  assert.match(state.updateError.value, /not published/);
}
{
  let installs = 0;
  const refreshing = deferred();
  const { state } = load({ check: () => refreshing.promise });
  state.availableUpdate.value = update(async () => { installs++; });
  const checking = state.checkUpdateOnce();
  const installing = state.installAvailableUpdate();
  refreshing.resolve(null); await checking; await installing;
  assert.equal(installs, 0, 'must wait for the refreshed manifest instead of using an invalidated cache');
}
{
  const repository = 'owner/repo', tag = 'v0.2.0';
  const assets = targets.map((target, index) => ({ id: index + 1, name: `${target}.bin`, url: `https://api.github.com/repos/${repository}/releases/assets/${index + 1}` }));
  const manifests = Object.fromEntries(targets.map((target, index) => [target, {
    version: '0.2.0', notes: 'release notes', platforms: {
      // A runner's snapshot may include stale entries written by other runners.
      'unrelated-arch': { signature: 'stale', url: 'http://localhost/stale' },
      [target]: { signature: `signature-${target}`, url: assets[index].url },
    },
  }]));
  const merged = mergeManifests(manifests, tag, repository, assets);
  assert.deepEqual(Object.keys(merged.platforms), targets);
  assert.equal(merged.notes, 'release notes');
  const reverse = Object.fromEntries(Object.entries(manifests).reverse());
  assert.deepEqual(mergeManifests(reverse, tag, repository, assets).platforms, merged.platforms);
  for (const change of [
    m => delete m['darwin-aarch64'],
    m => { m['darwin-aarch64'].version = '0.1.0'; },
    m => { m['linux-x86_64'].platforms['linux-x86_64'].signature = ''; },
    m => { m['windows-x86_64'].platforms['windows-x86_64'].url = 'https://github.com/other/repo/releases/download/v0.2.0/app.exe'; },
  ]) {
    const broken = structuredClone(manifests); change(broken);
    assert.throws(() => mergeManifests(broken, tag, repository, assets));
  }
  const browser = structuredClone(manifests);
  browser['linux-x86_64'].platforms['linux-x86_64'].url = `https://github.com/${repository}/releases/download/${tag}/linux-x86_64.bin`;
  assert.ok(mergeManifests(browser, tag, repository, assets));
}
{
  for (const [target, files] of [
    ['darwin-aarch64', ['/target/aarch64-apple-darwin/release/bundle/macos/Free API Chat.app.tar.gz']],
    ['darwin-x86_64', ['/target/x86_64-apple-darwin/release/bundle/macos/Free API Chat.app.tar.gz']],
    ['windows-x86_64', ['C:\\bundle\\nsis\\Free API Chat_0.2.0_x64-setup.exe', 'C:\\bundle\\msi\\Free API Chat_0.2.0_x64_en-US.msi']],
    ['linux-x86_64', ['/target/release/bundle/appimage/Free API Chat_0.2.0_amd64.AppImage']],
  ]) {
    const options = { paths: files, tag: 'v0.2.0', target, repository: 'owner/repo', readSignature: file => {
      assert.ok(files.includes(file.slice(0, -4))); return 'signature\n';
    }};
    const manifest = createManifest(options);
    assert.equal(manifest.version, '0.2.0'); assert.ok(manifest.platforms[target]);
    assert.equal(manifest.platforms[target].signature, 'signature');
    assert.ok(manifest.platforms[target].url.includes(`/v0.2.0/free-api-chat_v0.2.0_${target}`));
    if (target.startsWith('windows')) {
      assert.match(manifest.platforms[target].url, /-setup.exe$/);
      assert.match(manifest.platforms[`${target}-msi`].url, /x86_64.msi$/);
    }
    assert.throws(() => createManifest({ ...options, paths: [] }));
    assert.throws(() => createManifest({ ...options, paths: [...files, files[0]] }));
    assert.throws(() => createManifest({ ...options, readSignature: () => '' }));
  }
}
{
  const yaml = require('yaml');
  const workflow = yaml.parse(fs.readFileSync('.github/workflows/release.yml', 'utf8'));
  const script = workflow.jobs['release-config'].steps.find(step => step.id === 'version').run;
  const temp = fs.mkdtempSync(path.join(os.tmpdir(), 'free-api-release-config-'));
  try {
    const version = JSON.parse(fs.readFileSync('src-tauri/tauri.conf.json', 'utf8')).version;
    const output = path.join(temp, 'output');
    const run = spawnSync('bash', ['-c', script], { encoding: 'utf8', env: { ...process.env, RELEASE_TAG: `v${version}`, GITHUB_OUTPUT: output } });
    assert.equal(run.status, 0, run.stderr);
    const fields = Object.fromEntries(fs.readFileSync(output, 'utf8').trim().split('\n').map(line => line.split('=')));
    assert.equal(fields.tag, `v${version}`);
    assert.equal(fields.commit, execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim());
    assert.match(fields.commit, /^[0-9a-f]{40}$/);
    const action = workflow.jobs['publish-tauri'].steps.find(step => step.uses === 'tauri-apps/tauri-action@v1');
    assert.equal(action.with.releaseCommitish, '${{ needs.release-config.outputs.commit }}');
    const invalid = spawnSync('bash', ['-c', script], { encoding: 'utf8', env: { ...process.env, RELEASE_TAG: 'main', GITHUB_OUTPUT: output } });
    assert.notEqual(invalid.status, 0);
  } finally { fs.rmSync(temp, { recursive: true, force: true }); }
}
console.log('GitHub updater: concurrent checks, network errors, progress, duplicate installation, signature failure, restart recovery, native-package fallback and manifest merging passed.');
