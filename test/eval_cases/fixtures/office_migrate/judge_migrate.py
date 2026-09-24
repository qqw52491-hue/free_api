#!/usr/bin/env python3
"""判据：办公场景——混合格式遗留配置统一迁移 JSON。
输入: test/eval_cases/fixtures/office_migrate/input/ 4 个不同格式配置文件。
要求: output/config.json 合并全部键（同键冲突取"更新日期"较新文件的值），
      output/migration_report.md 记录冲突键取舍。
预期关键事实（判据据此核对）:
 - server.port = 9090（db.properties 2024-05 新于 app.ini 2024-03）
 - server.workers = 8（notes.txt 2024-07 最新）
 - cache_enabled = false（features.conf 2024-06 新于 app.ini）
 - beta_flag = true（features.conf）
 - db.port = 6432（features.conf 新于 db.properties）
判据:
 1. output/config.json 可解析且至少含上述 5 个键且值正确（值以字符串/布尔/数字形态均可）；
 2. output/migration_report.md 存在且提及冲突键（server.port 或 port）与取舍依据。
"""
import sys, os, json, re

fails = []
cfg = 'output/config.json'
rpt = 'output/migration_report.md'

if not os.path.exists(cfg):
    cands = [os.path.join('output', f) for f in os.listdir('output') if f.endswith('.json')] if os.path.isdir('output') else []
    if cands: cfg = cands[0]
    else: fails.append('output/config.json 不存在')

data = None
if not fails:
    try:
        data = json.load(open(cfg, encoding='utf-8'))
    except Exception as e:
        fails.append(f'config.json 解析失败: {e}')

def get(d, key):
    # 兼容嵌套 server.port / 扁平 server.port 两种写法
    if '.' in key:
        top, sub = key.split('.', 1)
        if isinstance(d.get(top), dict):
            return d[top].get(sub)
    return d.get(key)

if data is not None:
    expect = {
        'server.port': ('9090', 9090),
        'server.workers': ('8', 8),
        'cache_enabled': (False, 'false'),
        'beta_flag': (True, 'true'),
        'db.port': ('6432', 6432),
    }
    for k, vals in expect.items():
        v = get(data, k)
        ok = v in vals or str(v).lower() in [str(x).lower() for x in vals]
        if not ok:
            fails.append(f'{k} = {v!r}（期望 {vals[0]!r}）')

if not os.path.exists(rpt):
    cands = [os.path.join('output', f) for f in os.listdir('output') if f.endswith('.md')] if os.path.isdir('output') else []
    if cands: rpt = cands[0]
    else: fails.append('migration_report.md 不存在')
if os.path.exists(rpt):
    body = open(rpt, encoding='utf-8').read()
    if not re.search(r'server\.port|port.*9090|冲突|conflict', body, re.I):
        fails.append('migration_report 未提及端口冲突或取舍')

if fails:
    for f in fails: print('❌', f)
    print('JUDGE FAIL'); sys.exit(1)
print('✅ JUDGE PASS')
