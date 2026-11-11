// Exercise the desktop Vue UI with a stubbed model catalog and task launch.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, readFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import vue from '@vitejs/plugin-vue';
import { createServer } from 'vite';

const root = fileURLToPath(new URL('../', import.meta.url));
const chromeBin = process.env.CHROME_BIN || 'chromium';
const storageKey = 'free-api.agent.model-routing.v1';
const entry = `
import { createApp } from 'vue';
import { mockIPC } from '@tauri-apps/api/mocks';
import AgentView from '/src/views/AgentView.vue';

const model = (id, label) => ({ id, name: id, display_name: label, platform_name: '测试平台' });
const existing = model('existing', '已有模型');
const addedModels = [model('new-pro', '新规划'), model('new-flash', '新执行'), model('new-vision', '新视觉')];
window.fixture = {
    models: sessionStorage.getItem('models-added') ? [existing, ...addedModels] : [existing],
    addModels() {
        this.models.push(...addedModels);
        sessionStorage.setItem('models-added', 'true');
    },
    modelReads: 0,
    launches: [],
};
const project = { id: 'project', name: '测试项目', root_path: '/fixture', created_at: '', updated_at: '' };
mockIPC((cmd, args) => {
    switch (cmd) {
        case 'get_all_models_with_platform':
            fixture.modelReads++;
            if (fixture.catalogFails) throw Error('catalog unavailable');
            return structuredClone(fixture.models);
        case 'list_agent_projects': return [project];
        case 'create_session': return { id: 'saved-session' };
        case 'run_agent_main_loop': fixture.launches.push(args.modelRouting); return;
        case 'get_agent_checkpoint': return null;
        default: return [];
    }
}, { shouldMockEvents: true });
createApp(AgentView).mount('#app');
`;

const entryId = '\0agent-model-routing-test';
const server = await createServer({
    root,
    configFile: false,
    logLevel: 'error',
    server: { host: '127.0.0.1', port: 0 },
    plugins: [vue(), {
        name: 'agent-model-routing-test',
        resolveId: id => id === 'agent-model-routing-test' ? entryId : undefined,
        load: id => id === entryId ? entry : undefined,
        configureServer(vite) {
            vite.middlewares.use('/__model_routing_test', (_req, res) => {
                res.setHeader('Content-Type', 'text/html');
                res.end('<div id="app"></div><script type="module" src="/@id/__x00__agent-model-routing-test"></script>');
            });
        },
    }],
});

async function until(read, description) {
    for (let i = 0; i < 200; i++) {
        const result = await read();
        if (result) return result;
        await delay(50);
    }
    throw Error(`Timed out waiting for ${description}`);
}

const profile = await mkdtemp(join(tmpdir(), 'agent-model-routing-'));
let chrome, socket;
try {
    await server.listen();
    chrome = spawn(chromeBin, ['--headless=new', '--no-sandbox', '--disable-gpu', '--remote-debugging-port=0', `--user-data-dir=${profile}`, 'about:blank'], { stdio: 'ignore' });
    let launchError;
    chrome.on('error', error => { launchError = error; });
    const port = await until(async () => {
        if (launchError) throw launchError;
        return readFile(join(profile, 'DevToolsActivePort'), 'utf8').then(raw => raw.split('\n')[0], () => null);
    }, 'Chrome');
    const pages = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
    socket = new WebSocket(pages.find(page => page.type === 'page').webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
        socket.addEventListener('open', resolve, { once: true });
        socket.addEventListener('error', reject, { once: true });
    });
    let sequence = 0;
    const pending = new Map();
    socket.addEventListener('message', event => {
        const { id, result, error } = JSON.parse(event.data);
        const request = pending.get(id);
        if (!request) return;
        pending.delete(id);
        if (error) request.reject(Error(JSON.stringify(error)));
        else request.resolve(result);
    });
    const call = (method, params = {}) => new Promise((resolve, reject) => {
        const id = ++sequence;
        pending.set(id, { resolve, reject });
        socket.send(JSON.stringify({ id, method, params }));
    });
    const evaluate = async expression => {
        const { result, exceptionDetails } = await call('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
        if (exceptionDetails) throw Error(result.description || JSON.stringify(exceptionDetails));
        return result.value;
    };
    const element = selector => `document.querySelector(${JSON.stringify(selector)})`;
    const text = selector => evaluate(`${element(selector)}?.textContent.trim()`);
    const click = selector => evaluate(`${element(selector)}.click()`);
    const waitFor = (expression, description) => until(() => evaluate(expression), description);
    const stored = () => evaluate(`JSON.parse(localStorage.getItem(${JSON.stringify(storageKey)}))`);
    const selections = () => evaluate(`Array.from(document.querySelectorAll('.model-routing-popover .route-controls .model-select-trigger'), button => button.querySelector('.selected-display-name')?.textContent.trim() || '')`);
    const choose = async (slot, label) => {
        await waitFor(`!${element('.model-dropdown-popover')}`, 'previous dropdown to close');
        await click(`.model-routing-popover .${slot}-tier .model-select-trigger`);
        const option = `Array.from(document.querySelectorAll('.model-dropdown-popover .model-option-row')).find(button => button.querySelector('.option-display-name')?.textContent.trim() === ${JSON.stringify(label)})`;
        await waitFor(`!!${option}`, label);
        await evaluate(`${option}.click()`);
    };
    const chip = '.agent-panel-frame .model-chip';
    const chipName = `${chip} .model-name-text`;
    const hint = '.agent-panel-frame .model-setup-hint';
    const submit = '.agent-panel-frame .submit-control';
    const textarea = '.agent-panel-frame .composer-input-surface textarea';
    const openApp = () => call('Page.navigate', { url: `http://127.0.0.1:${server.httpServer.address().port}/__model_routing_test` });
    const enterGoal = () => evaluate(`(() => {
        const input = ${element(textarea)};
        input.value = '验证已选择的模型';
        input.dispatchEvent(new Event('input', { bubbles: true }));
    })()`);

    await openApp();
    await waitFor(`${element(chipName)}?.textContent.trim() === '已有模型'`, 'initial model defaults');
    await enterGoal();
    await evaluate('fixture.addModels()');
    await click(chip);
    await choose('pro', '新规划');
    await choose('flash', '新执行');
    await choose('vision', '新视觉');
    assert.deepEqual(await selections(), ['新规划', '新执行', '新视觉']);
    assert.equal(await text(chipName), '新规划', 'the chip must see models added after the page loaded');
    assert.equal(await text('.agent-panel-frame .flash-tag'), '快速 · 新执行');
    assert.equal(await evaluate(`!!${element(hint)}`), false);
    assert.equal(await evaluate(`${element(submit)}.disabled`), false);
    const { pro, flash, vision } = await stored();
    assert.deepEqual({ pro, flash, vision }, { pro: 'new-pro', flash: 'new-flash', vision: 'new-vision' });

    const beforeRefresh = await evaluate('fixture.modelReads');
    await evaluate('fixture.catalogFails = true');
    await click('.model-routing-popover .close-popover-btn');
    await click(chip);
    await waitFor(`fixture.modelReads > ${beforeRefresh}`, 'catalog refresh');
    assert.deepEqual(await selections(), ['新规划', '新执行', '新视觉']);
    assert.equal(await text(chipName), '新规划', 'a failed refresh must preserve the last successful catalog');
    await evaluate('fixture.catalogFails = false');
    await click('.model-routing-popover .vision-tier .btn-trigger-clear');
    assert.equal((await stored()).vision, '');
    assert.equal(await evaluate(`${element(submit)}.disabled`), false, 'Vision is optional');
    await click('.model-routing-popover .flash-tier .btn-trigger-clear');
    assert.equal((await stored()).flash, '');
    assert.equal(await text(hint), '请选择执行模型。');
    assert.equal(await evaluate(`${element(submit)}.disabled`), true);

    await evaluate('fixture.beforeReload = true');
    await call('Page.reload');
    await waitFor(`window.fixture && !fixture.beforeReload && ${element(chipName)}?.textContent.trim() === '新规划'`, 'reopened conversation');
    await click(chip);
    assert.deepEqual(await selections(), ['新规划', '', '']);
    assert.equal(await text(hint), '请选择执行模型。');
    await enterGoal();
    await choose('flash', '新规划');
    await click('.model-routing-popover .close-popover-btn');
    await click(submit);
    await waitFor('fixture.launches.length > 0', 'desktop task launch');
    const launched = await evaluate('fixture.launches[0]');
    assert.deepEqual([launched.pro, launched.flash, launched.vision], ['new-pro', 'new-pro', '']);

    await click('.project-row');
    await waitFor(`!!${element('.project-workspace-view .model-chip')}`, 'project workspace');
    const beforeProjectRefresh = await evaluate('fixture.modelReads');
    await click('.project-workspace-view .model-chip');
    await waitFor(`fixture.modelReads > ${beforeProjectRefresh}`, 'project catalog refresh');
    await evaluate(`fixture.models.push({ id: 'project-pro', name: 'project-pro', display_name: '项目规划' })`);
    await choose('pro', '项目规划');
    assert.equal(await text('.project-workspace-view .model-badge-chip'), '项目规划');
    assert.equal((await stored()).pro, 'project-pro');
    await click('.model-routing-popover .pro-tier .btn-trigger-clear');
    assert.equal(await text('.project-workspace-view .model-badge-chip'), '未配置规划模型');
    assert.equal((await stored()).pro, '');

    console.log('PASS: desktop Agent and project model choices, shared catalog, save and reload, clear, and launch payload');
} finally {
    socket?.close();
    if (chrome?.pid && chrome.exitCode === null && chrome.signalCode === null) {
        await new Promise(resolve => {
            chrome.once('exit', resolve);
            chrome.kill();
        });
    }
    await server.close();
    await rm(profile, { recursive: true, force: true });
}
