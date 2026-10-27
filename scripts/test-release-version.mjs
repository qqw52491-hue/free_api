// Integration tests against disposable Git repositories; never pushes this project.
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';

const script = fs.readFileSync('scripts/release-version.mjs', 'utf8');
const sandbox = fs.mkdtempSync(path.join(os.tmpdir(), 'free-api-release-test-'));
function run(program, args, cwd, allow = [0]) {
  const result = spawnSync(program, args, { cwd, encoding: 'utf8', env: { ...process.env, GIT_TERMINAL_PROMPT: '0' }, maxBuffer: 4 * 1024 * 1024 });
  if (result.error || !allow.includes(result.status)) throw new Error(`${program} ${args.join(' ')}: ${result.error?.message || result.stderr || result.stdout}`);
  return result;
}
function git(cwd, ...args) { return run('git', args, cwd).stdout.trim(); }
function fixture(name) {
  const dir = path.join(sandbox, name), repo = path.join(dir, 'repo'), remote = path.join(dir, 'remote.git');
  fs.mkdirSync(path.join(repo, 'scripts'), { recursive: true });
  fs.mkdirSync(path.join(repo, 'src-tauri', 'src'), { recursive: true });
  fs.writeFileSync(path.join(repo, 'scripts', 'release-version.mjs'), script);
  const writeJson = (file, value) => fs.writeFileSync(path.join(repo, file), JSON.stringify(value, null, 2) + '\n');
  writeJson('package.json', { name: 'tauri-app', version: '0.1.0', scripts: { release: 'node scripts/release-version.mjs' } });
  writeJson('package-lock.json', { name: 'tauri-app', version: '0.1.0', lockfileVersion: 3, packages: { '': { name: 'tauri-app', version: '0.1.0' }, 'node_modules/example': { version: '2.0.0' } } });
  writeJson('src-tauri/tauri.conf.json', { productName: 'Free API Chat', version: '0.1.0' });
  fs.writeFileSync(path.join(repo, 'src-tauri/Cargo.toml'), '[package]\nname = "free-api-chat"\nversion = "0.1.0"\nedition = "2021"\n\n[dependencies]\n');
  fs.writeFileSync(path.join(repo, 'src-tauri/Cargo.lock'), '# generated\nversion = 4\n\n[[package]]\nname = "free-api-chat"\nversion = "0.1.0"\n');
  fs.writeFileSync(path.join(repo, 'src-tauri/src/main.rs'), 'fn main() {}\n');
  run('git', ['init', '--bare', remote], dir);
  git(repo, 'init', '-b', 'main'); git(repo, 'config', 'user.name', 'Release Test'); git(repo, 'config', 'user.email', 'release-test@example.invalid');
  git(repo, 'add', '.'); git(repo, 'commit', '-m', 'initial'); git(repo, 'remote', 'add', 'origin', remote); git(repo, 'push', '-u', 'origin', 'main');
  return { repo, remote, dir, original: git(repo, 'rev-parse', 'HEAD') };
}
function release(f, ...args) { return run(process.execPath, ['scripts/release-version.mjs', ...args], f.repo, [0, 1]); }
function assertNoChanges(f) { assert.equal(git(f.repo, 'status', '--porcelain'), ''); assert.equal(git(f.repo, 'rev-parse', 'HEAD'), f.original); }
try {
  {
    const f = fixture('complete');
    const dry = release(f, 'v1.2', '--dry-run');
    assert.equal(dry.status, 0, dry.stderr); assert.match(dry.stdout, /v1\.2\.0/); assertNoChanges(f);
    const completed = release(f, 'v1.2');
    assert.equal(completed.status, 0, completed.stderr); assert.match(completed.stdout, /v1\.2\.0/);
    const sha = git(f.repo, 'rev-parse', 'HEAD'); assert.notEqual(sha, f.original);
    assert.equal(git(f.repo, 'rev-parse', 'v1.2.0'), sha);
    assert.equal(git(f.repo, '--git-dir', f.remote, 'rev-parse', 'refs/heads/main'), sha);
    assert.equal(git(f.repo, '--git-dir', f.remote, 'rev-parse', 'refs/tags/v1.2.0'), sha);
    const front = JSON.parse(fs.readFileSync(path.join(f.repo, 'package.json')));
    const lock = JSON.parse(fs.readFileSync(path.join(f.repo, 'package-lock.json')));
    const conf = JSON.parse(fs.readFileSync(path.join(f.repo, 'src-tauri/tauri.conf.json')));
    assert.deepEqual([front.version, lock.version, lock.packages[''].version, conf.version], Array(4).fill('1.2.0'));
    assert.equal(lock.packages['node_modules/example'].version, '2.0.0');
    assert.match(fs.readFileSync(path.join(f.repo, 'src-tauri/Cargo.toml'), 'utf8'), /\nversion = "1\.2\.0"\n/);
    assert.match(fs.readFileSync(path.join(f.repo, 'src-tauri/Cargo.lock'), 'utf8'), /name = "free-api-chat"\nversion = "1\.2\.0"/);
    run('cargo', ['metadata', '--locked', '--offline', '--no-deps', '--manifest-path', 'src-tauri/Cargo.toml', '--format-version', '1'], f.repo);
    assert.equal(git(f.repo, 'status', '--porcelain'), '');
  }
  {
    const f = fixture('same-version'); const result = release(f, 'v0.1');
    assert.equal(result.status, 0, result.stderr); assert.match(result.stdout, /版本号未变化/);
    assert.equal(git(f.repo, 'rev-parse', 'HEAD'), f.original);
    assert.equal(git(f.repo, '--git-dir', f.remote, 'rev-parse', 'refs/tags/v0.1.0'), f.original);
  }
  {
    const f = fixture('dirty'); fs.writeFileSync(path.join(f.repo, 'work-in-progress.txt'), 'work');
    const preview = release(f, 'v0.2', '--dry-run'); assert.equal(preview.status, 0, preview.stderr); assert.match(preview.stdout, /未提交/);
    const result = release(f, 'v0.2'); assert.equal(result.status, 1); assert.match(result.stderr, /未提交或未跟踪/);
    assert.equal(git(f.repo, 'rev-parse', 'HEAD'), f.original);
    assert.equal(JSON.parse(fs.readFileSync(path.join(f.repo, 'package.json'))).version, '0.1.0');
  }
  {
    const f = fixture('duplicate-local'); git(f.repo, 'tag', 'v0.2.0');
    const result = release(f, 'v0.2'); assert.equal(result.status, 1); assert.match(result.stderr, /本地标签.*已存在/); assertNoChanges(f);
  }
  {
    const f = fixture('duplicate-remote'); git(f.repo, '--git-dir', f.remote, 'tag', 'v0.2.0', f.original);
    const result = release(f, 'v0.2'); assert.equal(result.status, 1); assert.match(result.stderr, /远端标签.*已存在/); assertNoChanges(f);
  }
  {
    const f = fixture('diverged'); const clone = path.join(f.dir, 'other');
    run('git', ['clone', '-b', 'main', f.remote, clone], f.dir);
    git(clone, 'config', 'user.name', 'Other'); git(clone, 'config', 'user.email', 'other@example.invalid');
    fs.writeFileSync(path.join(clone, 'remote.txt'), 'new'); git(clone, 'add', 'remote.txt'); git(clone, 'commit', '-m', 'remote change'); git(clone, 'push', 'origin', 'main');
    const result = release(f, 'v0.2'); assert.equal(result.status, 1); assert.match(result.stderr, /远端 main 有本地尚未包含的提交/); assertNoChanges(f);
  }
  {
    const f = fixture('invalid');
    for (const value of ['v1', 'v1.02', 'v1.2.3-rc.1', 'v1.2.3.4']) {
      const result = release(f, value); assert.equal(result.status, 1); assert.match(result.stderr, /版本格式不正确/);
    }
    assertNoChanges(f);
  }
  if (process.platform !== 'win32') {
    const f = fixture('tag-push-failure'); const hook = path.join(f.remote, 'hooks/pre-receive');
    fs.writeFileSync(hook, '#!/bin/sh\nwhile read old new ref; do case "$ref" in refs/tags/*) exit 1;; esac; done\nexit 0\n'); fs.chmodSync(hook, 0o755);
    const result = release(f, '0.2'); assert.equal(result.status, 1); assert.match(result.stderr, /推送标签/); assert.match(result.stderr, /git push origin v0\.2\.0/);
    const sha = git(f.repo, 'rev-parse', 'HEAD');
    assert.equal(git(f.repo, 'rev-parse', 'v0.2.0'), sha); assert.equal(git(f.repo, '--git-dir', f.remote, 'rev-parse', 'refs/heads/main'), sha);
  }
  console.log('Release integration: preview, version commit, local/remote tag and push, same-version tagging, dirty/duplicate/diverged safeguards, invalid versions and push recovery passed.');
} finally { fs.rmSync(sandbox, { recursive: true, force: true }); }
