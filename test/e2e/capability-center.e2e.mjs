import assert from 'node:assert/strict';
import { mkdirSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const artifactDir = path.join(rootDir, 'test-artifacts');
const deepSeekTestModels = Object.freeze(['deepseek-v4-pro', 'deepseek-v4-flash']);

describe('能力管理原生窗口界面', () => {
  before(async () => {
    mkdirSync(artifactDir, { recursive: true });
    await browser.tauri.switchWindow('main');
  });

  it('移除插件顶层标签，并在统一页面展示 MCP 与 Skill 管理说明', async () => {
    await browser.url('tauri://localhost/plugins');
    const heading = await $('h1');
    await heading.waitForDisplayed({ timeout: 15_000 });
    assert.equal(await heading.getText(), '能力管理');
    const navLabels = await browser.execute(() => Array.from(document.querySelectorAll('.nav-link'), (item) => item.textContent?.trim()));
    assert.deepEqual(navLabels, ['平台管理', 'AI Agent']);
    const summary = await $('.capability-summary');
    await summary.waitForDisplayed({ timeout: 10_000 });
    const summaryText = await summary.getText();
    assert.match(summaryText, /MCP 连接/);
    assert.match(summaryText, /WORKSPACE SKILLS/);
    assert.match(summaryText, /AGENTS.md/);
    assert.match(summaryText, /HUB.md/);
    await browser.saveScreenshot(path.join(artifactDir, 'capability-center-unified.png'));
  });

  it('旧 AI 聊天地址会安全重定向到平台管理', async () => {
    await browser.url('tauri://localhost/chat');
    const platformTitle = await $('.platform-panel .panel-title');
    await platformTitle.waitForDisplayed({ timeout: 15_000 });
    assert.match(await platformTitle.getText(), /AI 平台/);
    assert.deepEqual(
      await browser.execute(() => Array.from(document.querySelectorAll('.nav-link'), (item) => item.textContent?.trim())),
      ['平台管理', 'AI Agent'],
    );
  });

  it('可维护自定义提示词与 Skill，并在测试结束后清理条目', async () => {
    const itemName = `E2E Skill ${Date.now()}`;
    await browser.url('tauri://localhost/plugins');
    const addButton = await $('[data-testid=custom-library-add]');
    await addButton.waitForDisplayed({ timeout: 15_000 });
    await addButton.click();

    const modal = await $('.instruction-modal');
    await modal.waitForDisplayed({ timeout: 10_000 });
    const selectedKind = await browser.execute(() => {
      const select = document.querySelector('.instruction-modal select');
      if (!(select instanceof HTMLSelectElement)) return null;
      select.value = 'skill';
      select.dispatchEvent(new Event('change', { bubbles: true }));
      return select.value;
    });
    assert.equal(selectedKind, 'skill', '新建条目必须可选择 Skill 类型');
    const inputs = await $$('.instruction-modal input');
    await inputs[0].setValue(itemName);
    await inputs[1].setValue('验证可复用流程的边界与输出');
    const content = await $('.instruction-modal textarea');
    await content.setValue('先核对输入，再给出结论；不确定时说明边界并请求补充信息。');
    const saveButton = await $('.instruction-modal .save-btn');
    await saveButton.click();

    await browser.waitUntil(async () => {
      const names = await browser.execute(() => Array.from(
        document.querySelectorAll('.instruction-card h3'),
        (card) => card.textContent?.trim(),
      ));
      return names.includes(itemName);
    }, { timeout: 10_000, timeoutMsg: '自定义 Skill 保存后应展示在能力库中' });

    const cardIndex = await browser.execute((name) => Array.from(document.querySelectorAll('.instruction-card'))
      .findIndex((element) => element.querySelector('h3')?.textContent?.trim() === name), itemName);
    assert.ok(cardIndex >= 0, '应定位到刚创建的自定义 Skill 卡片');
    const itemCard = (await $$('.instruction-card'))[cardIndex];
    assert.match(await itemCard.getText(), /CUSTOM SKILL/);
    await browser.saveScreenshot(path.join(artifactDir, 'custom-skill-library.png'));

    await browser.execute(() => { window.confirm = () => true; });
    await (await itemCard.$('.text-btn.danger')).click();
    await browser.waitUntil(async () => {
      const names = await browser.execute(() => Array.from(
        document.querySelectorAll('.instruction-card h3'),
        (card) => card.textContent?.trim(),
      ));
      return !names.includes(itemName);
    }, { timeout: 10_000, timeoutMsg: '删除后测试 Skill 不应残留' });
  });

  it('平台管理仅检测指定 DeepSeek 模型并展示汇总', async () => {
    await browser.url('tauri://localhost/apis');
    const modelTitle = await $('.model-panel .panel-title');
    await modelTitle.waitForDisplayed({ timeout: 15_000 });
    const platformItems = await $$('.platform-item');
    const batchButton = await $('[data-testid=test-current-platform]');
    await batchButton.waitForDisplayed({ timeout: 15_000 });

    const deepSeekIndex = await browser.execute(() => Array.from(document.querySelectorAll('.platform-item'))
      .findIndex((element) => element.textContent?.trim().toLowerCase().includes('deepseek')));
    assert.ok(deepSeekIndex >= 0, '真实模型检测要求已配置 DeepSeek 平台；不允许回退至其他平台');
    await platformItems[deepSeekIndex].click();
    await browser.waitUntil(async () => /deepseek/i.test(await $('.model-panel .panel-title').getText()), {
      timeout: 15_000,
      timeoutMsg: '批量检测必须在 DeepSeek 平台中执行',
    });

    const renderedModelNames = await browser.execute(() => Array.from(
      document.querySelectorAll('.model-card .m-name'),
      (element) => element.textContent?.trim(),
    ));
    assert.deepEqual(
      [...renderedModelNames].sort(),
      [...deepSeekTestModels].sort(),
      '真实批量检测只能包含指定的 DeepSeek 模型',
    );
    assert.equal(await batchButton.isEnabled(), true, 'DeepSeek 的两个指定模型均已配置时，批量检测应可用');

    await batchButton.click();
    await browser.waitUntil(async () => /检测中 \d+\/\d+/.test(await batchButton.getText()), { timeout: 5_000 });
    assert.equal(await batchButton.getAttribute('aria-busy'), 'true', '批量检测进行时按钮应声明忙碌状态');

    const summary = await $('[data-testid=batch-test-summary]');
    await summary.waitForDisplayed({ timeout: 30_000 });
    assert.equal(await summary.getAttribute('role'), 'status', '批量检测汇总应使用状态语义');
    assert.equal(await summary.getAttribute('aria-live'), 'polite', '批量检测汇总应采用礼貌式播报');
    const summaryText = await summary.getText();
    const summaryMatch = summaryText.match(/^已检测 (\d+) 个：(\d+) 可用，(\d+) 不可用$/);
    assert.ok(summaryMatch, `批量检测汇总格式不正确：${summaryText}`);
    assert.equal(Number(summaryMatch[1]), deepSeekTestModels.length, '批量检测应恰好处理两个指定 DeepSeek 模型');
    assert.equal(Number(summaryMatch[1]), Number(summaryMatch[2]) + Number(summaryMatch[3]), '汇总中的可用与不可用数量应等于检测总数');
    await browser.waitUntil(async () => await $('[data-testid=test-current-platform]').isEnabled(), { timeout: 5_000 });
    assert.equal(await batchButton.getAttribute('aria-busy'), 'false', '批量检测结束后按钮应清除忙碌状态');
    await browser.saveScreenshot(path.join(artifactDir, 'api-manager-batch-test.png'));
  });
});
