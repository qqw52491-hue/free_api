import { spawnSync } from 'node:child_process';

const database = '/Users/wx/Library/Application Support/free-api-chat/data_v2.db';
// 真实聊天连通性检测只能使用 Free API 内已配置的 DeepSeek 模型。
const deepSeekModels = Object.freeze(['deepseek-v4-pro', 'deepseek-v4-flash']);
const query = `
  SELECT p.name, p.base_url, p.api_key, m.name
  FROM platforms p
  JOIN models m ON m.platform_id = p.id
  WHERE lower(p.name) = 'deepseek'
    AND m.name IN ('deepseek-v4-pro', 'deepseek-v4-flash')
  ORDER BY CASE m.name
    WHEN 'deepseek-v4-pro' THEN 1
    WHEN 'deepseek-v4-flash' THEN 2
    ELSE 99
  END;
`;

const result = spawnSync('sqlite3', ['-separator', '|', database, query], { encoding: 'utf8' });
if (result.status !== 0) {
  throw new Error('无法读取已配置的 DeepSeek 平台：' + (result.stderr || '未知错误'));
}

const targets = result.stdout.trim().split('\n').filter(Boolean).map((line) => {
  const [platform, baseUrl, apiKey, model] = line.split('|');
  return { platform, baseUrl, apiKey, model };
});
const foundModels = targets.map((target) => target.model);
if (targets.length !== deepSeekModels.length || deepSeekModels.some((model) => !foundModels.includes(model))) {
  throw new Error(`DeepSeek 连通性检测要求同时配置 ${deepSeekModels.join('、')}；当前仅发现 ${foundModels.join('、') || '无'}`);
}

function sanitizeDetail(value) {
  return value
    .replace(/\s+/g, ' ')
    .replace(/(\"(?:user_id|api_key|authorization)\"\s*:\s*\")[^\"]+(\")/gi, '$1***$2')
    .slice(0, 180);
}

const report = [];
for (const target of targets) {
  const endpoint = target.baseUrl.replace(/\/$/, '') + '/chat/completions';
  const started = performance.now();
  try {
    const response = await fetch(endpoint, {
      method: 'POST',
      headers: {
        Authorization: 'Bearer ' + target.apiKey,
        'Content-Type': 'application/json',
      },
      body: JSON.stringify({
        model: target.model,
        messages: [{ role: 'user', content: 'Hi' }],
        max_tokens: 5,
      }),
      signal: AbortSignal.timeout(20_000),
    });
    const body = await response.text();
    report.push({
      platform: target.platform,
      model: target.model,
      endpoint: new URL(endpoint).origin,
      status: response.ok ? 'passed' : 'failed',
      httpStatus: response.status,
      latencyMs: Math.round(performance.now() - started),
      detail: response.ok ? 'DeepSeek 最小聊天请求成功' : sanitizeDetail(body),
    });
  } catch (error) {
    report.push({
      platform: target.platform,
      model: target.model,
      endpoint: new URL(endpoint).origin,
      status: 'failed',
      httpStatus: null,
      latencyMs: Math.round(performance.now() - started),
      detail: error instanceof Error ? error.message : String(error),
    });
  }
}

console.log(JSON.stringify({
  testedAt: new Date().toISOString(),
  testScope: 'DeepSeek-only Free API chat connectivity',
  testedCount: report.length,
  report,
}, null, 2));
