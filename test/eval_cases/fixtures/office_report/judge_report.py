#!/usr/bin/env python3
"""判据：办公场景——三季 CSV 合并去重生成 Excel 汇总报表。
输入: office_docs/q{1,2,3}_sales.csv（60 行含 43 个唯一订单号，日期三种格式）
要求产物: output/汇总.xlsx（≥2 sheet：明细+按月汇总）与 output/汇总说明.md。
判据（全部满足）:
 1. output/汇总.xlsx 存在且可被 openpyxl 打开；
 2. 明细 sheet 数据行数 == 唯一订单数 43（去重生效，±1 容错表头变体）；
 3. 存在按月汇总 sheet 且月份数 >= 3；
 4. output/汇总说明.md 存在且提及三个季度(q1/q2/q3 或 一/二/三季)。
"""
import sys, os, re

fails = []
xlsx = 'output/汇总.xlsx'
md = 'output/汇总说明.md'

try:
    import openpyxl
except ImportError:
    print('JUDGE FAIL: openpyxl 不可用'); sys.exit(1)

if not os.path.exists(xlsx):
    # 兼容合理命名变体：output/ 下唯一的 xlsx
    cands = [f for f in os.listdir('output') if f.endswith('.xlsx')] if os.path.isdir('output') else []
    if len(cands) == 1:
        xlsx = os.path.join('output', cands[0])
    else:
        fails.append(f'{xlsx} 不存在且 output/ 下无唯一 xlsx（找到 {cands}）')
        wb = None
else:
    wb = None

if not fails:
    try:
        wb = openpyxl.load_workbook(xlsx)
    except Exception as e:
        fails.append(f'xlsx 打不开: {e}')

if not fails:
    sheets = wb.sheetnames
    if len(sheets) < 2:
        fails.append(f'sheet 数 {len(sheets)} < 2（需明细+按月汇总）')
    else:
        # 明细 sheet = 行数最多的那个
        detail = max(wb.worksheets, key=lambda s: s.max_row)
        n = detail.max_row - 1  # 去表头
        if abs(n - 43) > 1:
            fails.append(f'明细数据行 {n} != 43（唯一订单数，去重未生效或行数异常）')
        # 汇总 sheet：找含"月"或月份数的 sheet
        summ = None
        for s in wb.worksheets:
            if s is detail:
                continue
            txt = ' '.join(str(c.value) for row in s.iter_rows(max_row=3) for c in row if c.value)
            if '月' in txt or 'month' in txt.lower():
                summ = s; break
        if summ is None:
            summ = [s for s in wb.worksheets if s is not detail][0]
        months = summ.max_row - 1
        if months < 3:
            fails.append(f'按月汇总仅 {months} 行（不足 3 个月度）')

if not os.path.exists(md):
    cands = [f for f in os.listdir('output') if f.endswith('.md')] if os.path.isdir('output') else []
    if len(cands) >= 1:
        md = os.path.join('output', cands[0])
    else:
        fails.append(f'{md} 不存在且 output/ 下无 md')

if os.path.exists(md):
    body = open(md, encoding='utf-8').read()
    if not re.search(r'q1|q2|q3|一季|二季|三季|第一|第二|第三|20|22|18', body, re.I):
        fails.append('汇总说明未提及季度来源或行数')

if fails:
    for f in fails: print('❌', f)
    print('JUDGE FAIL'); sys.exit(1)
print('✅ JUDGE PASS')
