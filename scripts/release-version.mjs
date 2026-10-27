#!/usr/bin/env node
// Prepare a stable version, tag the current commit and push it to origin.
// Run: npm run release -- v1.2    (or add --dry-run to preview locally)
import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
process.chdir(root);
const versionFiles = ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock'];
const usage = '用法：npm run release -- v1.2.3 [--dry-run]\n例如：npm run release -- v1.2（自动补成 v1.2.0）';
const args = process.argv.slice(2);
if (args.includes('--help') || args.includes('-h')) { console.log(usage); process.exit(0); }
const dryRun = args.includes('--dry-run');
const values = args.filter(arg => arg !== '--dry-run');
if (values.length !== 1 || args.filter(arg => arg === '--dry-run').length > 1) fail(usage);
const match = /^v?(0|[1-9]\d*)\.(0|[1-9]\d*)(?:\.(0|[1-9]\d*))?$/.exec(values[0]);
if (!match) fail(`版本格式不正确：${values[0]}。仅接受稳定版本，例如 v1.2 或 v1.2.3。\n${usage}`);
const version = `${match[1]}.${match[2]}.${match[3] ?? '0'}`;
const tag = `v${version}`;
let stage = '检查';

function fail(message) { console.error(message); process.exit(1); }
function git(args, { allow = [0] } = {}) {
  const result = spawnSync('git', args, { cwd: root, encoding: 'utf8', maxBuffer: 10 * 1024 * 1024 });
  if (result.error) throw new Error(`无法执行 git ${args[0]}：${result.error.message}`);
  if (!allow.includes(result.status)) {
    // Avoid exposing credentials if a remote URL contains userinfo.
    const detail = (result.stderr || result.stdout || '').trim().replace(/(https?:\/\/)[^\s/@]+@/g, '$1<已隐藏>@');
    throw new Error(`git ${args.join(' ')} 失败${detail ? `：${detail}` : ''}`);
  }
  return { status: result.status, output: result.stdout.trim() };
}
function readJson(file) {
  const raw = fs.readFileSync(file, 'utf8');
  return { raw, value: JSON.parse(raw) };
}
function prepareVersions() {
  const frontend = readJson('package.json');
  const lock = readJson('package-lock.json');
  const tauri = readJson('src-tauri/tauri.conf.json');
  const cargo = fs.readFileSync('src-tauri/Cargo.toml', 'utf8');
  const cargoLock = fs.readFileSync('src-tauri/Cargo.lock', 'utf8');
  const cargoPackage = /^(\[package\]\s*\n)([\s\S]*?)(?=^\[|$(?![\s\S]))/m.exec(cargo);
  if (!cargoPackage) throw new Error('Cargo.toml 缺少 [package] 配置');
  const cargoVersion = /^version\s*=\s*"([^"]+)"\s*$/m.exec(cargoPackage[2]);
  const lockEntry = /^\[\[package\]\]\r?\nname = "free-api-chat"\r?\nversion = "([^"]+)"/gm;
  const entries = [...cargoLock.matchAll(lockEntry)];
  if (!cargoVersion || entries.length !== 1 || !lock.value.packages?.['']) throw new Error('找不到项目版本或锁文件的根包版本');
  const previous = tauri.value.version;
  if (![frontend.value.version, lock.value.version, lock.value.packages[''].version, cargoVersion[1], entries[0][1]].every(v => v === previous)) {
    throw new Error('现有版本文件不一致，请先核对项目配置；没有修改任何文件');
  }
  if (!/^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.test(previous)) throw new Error('当前项目版本不是稳定的三段版本号');
  if (previous === version) return { previous, changes: new Map() };
  frontend.value.version = version;
  lock.value.version = version;
  lock.value.packages[''].version = version;
  tauri.value.version = version;
  const newCargoSection = cargoPackage[2].replace(/^version\s*=\s*"[^"]+"\s*$/m, `version = "${version}"`);
  const newCargo = cargo.slice(0, cargoPackage.index) + cargoPackage[1] + newCargoSection + cargo.slice(cargoPackage.index + cargoPackage[0].length);
  const newCargoLock = cargoLock.replace(lockEntry, block => block.replace(/version = "[^"]+"$/, `version = "${version}"`));
  return { previous, changes: new Map([
    ['package.json', JSON.stringify(frontend.value, null, 2) + '\n'],
    ['package-lock.json', JSON.stringify(lock.value, null, 2) + '\n'],
    ['src-tauri/tauri.conf.json', JSON.stringify(tauri.value, null, 2) + '\n'],
    ['src-tauri/Cargo.toml', newCargo],
    ['src-tauri/Cargo.lock', newCargoLock],
  ]) };
}

try {
  const branch = git(['branch', '--show-current']).output;
  if (branch !== 'main') throw new Error(`当前分支是 ${branch || '未选中分支'}；请切换到 main 后发布`);
  const dirty = !!git(['status', '--porcelain', '--untracked-files=normal']).output;
  if (dirty && !dryRun) throw new Error('工作区有未提交或未跟踪的文件；请先检查并提交需要发布的改动');
  if (dirty) console.log('注意：工作区有未提交的改动；实际发布前需先检查并提交。');
  const { previous, changes } = prepareVersions();
  const localTag = git(['show-ref', '--verify', '--quiet', `refs/tags/${tag}`], { allow: [0, 1] });
  if (localTag.status === 0) throw new Error(`本地标签 ${tag} 已存在，不会覆盖；请使用新版本号`);
  console.log(`版本：${previous} → ${version}；标签：${tag}`);
  console.log(changes.size ? `将更新 ${changes.size} 个版本文件并提交。` : '版本号未变化，将直接为当前提交创建新标签。');
  console.log(`将推送 origin 的 main 和 ${tag}；GitHub Actions 会构建 Release 草稿。`);
  if (dryRun) {
    console.log('预览结束：未修改文件、创建标签或联系远端；实际发布时会检查远端。');
    process.exit(0);
  }
  const remoteTag = git(['ls-remote', '--exit-code', '--tags', 'origin', `refs/tags/${tag}`], { allow: [0, 2] });
  if (remoteTag.status === 0) throw new Error(`远端标签 ${tag} 已存在，不会覆盖；请使用新版本号`);
  git(['fetch', '--no-tags', 'origin', 'main']);
  if (git(['merge-base', '--is-ancestor', 'FETCH_HEAD', 'HEAD'], { allow: [0, 1] }).status !== 0) {
    throw new Error('远端 main 有本地尚未包含的提交，请先同步并检查，再发布');
  }
  if (changes.size) {
    stage = '更新版本文件';
    for (const [file, contents] of changes) fs.writeFileSync(file, contents);
    stage = '提交版本';
    git(['add', '--', ...versionFiles]);
    git(['commit', '-m', `chore: release ${tag}`]);
    console.log(`已提交 ${tag} 的版本变更。`);
  }
  stage = '创建标签';
  git(['tag', tag]);
  stage = '推送 main';
  git(['push', 'origin', 'main']);
  stage = '推送标签';
  git(['push', 'origin', `refs/tags/${tag}`]);
  console.log(`已推送 ${tag}，GitHub Actions 将自动构建。构建完成后，请检查并发布 Release 草稿。`);
} catch (error) {
  console.error(`发布在“${stage}”阶段停止：${error instanceof Error ? error.message : String(error)}`);
  if (stage !== '检查') {
    console.error('已完成的本地更改会保留。请先运行 git status 查看状态。');
    if (stage === '推送 main') console.error(`处理推送问题后，可依次执行：git push origin main；git push origin ${tag}`);
    if (stage === '推送标签') console.error(`处理推送问题后，可执行：git push origin ${tag}`);
  }
  process.exitCode = 1;
}
