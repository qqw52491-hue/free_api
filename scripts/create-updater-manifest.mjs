import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

// These names match releaseAssetNamePattern in release.yml. The matrix supplies
// platform/architecture; do not guess it from product names or runner hardware.
export function createManifest({ paths, tag, target, repository, readSignature }) {
  const formats = target.startsWith('darwin-')
    ? [{ ext: '.app.tar.gz', kind: 'app', setup: '' }]
    : target.startsWith('windows-')
      ? [{ ext: '.exe', kind: 'nsis', setup: '-setup' }, { ext: '.msi', kind: 'msi', setup: '' }]
      : [{ ext: '.AppImage', kind: 'appimage', setup: '' }];
  const platforms = {};
  for (const { ext, kind, setup } of formats) {
    const candidates = paths.filter(file => file.endsWith(ext));
    if (candidates.length !== 1) throw new Error(`${target}: 预期一个 ${ext} 更新包，实际 ${candidates.length} 个`);
    const signature = readSignature(`${candidates[0]}.sig`).trim();
    if (!signature) throw new Error(`${target}: ${ext} 签名为空`);
    const filename = `free-api-chat_${tag}_${target}${setup}${ext}`;
    const entry = {
      signature,
      url: `https://github.com/${repository}/releases/download/${encodeURIComponent(tag)}/${encodeURIComponent(filename)}`,
    };
    platforms[`${target}-${kind}`] = entry;
    platforms[target] ??= entry;
  }
  return { version: tag.replace(/^v/, ''), notes: `Free API Chat ${tag}`, pub_date: new Date().toISOString(), platforms };
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) {
  const manifest = createManifest({
    paths: JSON.parse(process.env.ARTIFACT_PATHS),
    tag: process.env.RELEASE_TAG,
    target: process.env.RELEASE_TARGET,
    repository: process.env.RELEASE_REPOSITORY,
    readSignature: file => fs.readFileSync(file, 'utf8'),
  });
  fs.writeFileSync('latest.json', `${JSON.stringify(manifest, null, 2)}\n`);
}
