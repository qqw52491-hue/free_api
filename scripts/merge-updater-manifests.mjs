// Merge each runner's own platform entries, independent of concurrent Release writes.
import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

export const targets = ['darwin-aarch64', 'darwin-x86_64', 'linux-x86_64', 'windows-x86_64'];

export function mergeManifests(manifests, tag, repository, assets) {
  const version = tag.replace(/^v/, '');
  const platforms = {};
  const assetUrls = new Set(assets.flatMap(asset => [
    asset.url,
    `https://api.github.com/repos/${repository}/releases/assets/${asset.id}`,
    `https://github.com/${repository}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(asset.name)}`,
  ]).filter(Boolean));
  for (const target of targets) {
    const manifest = manifests[target];
    if (!manifest || manifest.version?.replace(/^v/, '') !== version) {
      throw new Error(`${target}: 缺少本次版本 ${tag} 的更新清单`);
    }
    if (!manifest.platforms?.[target]) throw new Error(`${target}: 缺少主更新包`);
    for (const [key, entry] of Object.entries(manifest.platforms)) {
      if (key !== target && !key.startsWith(`${target}-`)) continue;
      if (typeof entry?.signature !== 'string' || !entry.signature.trim()) {
        throw new Error(`${key}: 缺少签名`);
      }
      // API asset URL 是 tauri-action v1 的实际输出；同时接受同一 Release 的浏览器下载 URL。
      if (!assetUrls.has(entry.url)) throw new Error(`${key}: 下载地址不属于本次 Release 的产物`);
      const url = new URL(entry.url);
      if (url.protocol !== 'https:' || url.username || url.password || url.search || url.hash) {
        throw new Error(`${key}: 更新下载地址必须为无凭据的 HTTPS URL`);
      }
      platforms[key] = { signature: entry.signature, url: entry.url };
    }
  }
  return {
    version,
    notes: manifests[targets[0]].notes ?? '',
    pub_date: manifests[targets[0]].pub_date ?? new Date().toISOString(),
    platforms,
  };
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const [directory, tag, repository, assetsFile, output] = process.argv.slice(2);
  if (!output) throw new Error('用法: node merge-updater-manifests.mjs <输入目录> <tag> <owner/repo> <assets.json> <输出文件>');
  const manifests = Object.fromEntries(targets.map(target => [target,
    JSON.parse(fs.readFileSync(path.join(directory, `updater-${target}`, 'latest.json'), 'utf8')),
  ]));
  const releases = JSON.parse(fs.readFileSync(assetsFile, 'utf8')).flat();
  const release = releases.find(release => release.tag_name === tag);
  if (!release) throw new Error(`找不到 Release ${tag}`);
  const { assets } = release;
  const merged = mergeManifests(manifests, tag, repository, assets);
  fs.writeFileSync(output, `${JSON.stringify(merged, null, 2)}\n`);
  console.log(`更新清单已齐套：${Object.keys(merged.platforms).join(', ')}`);
}
