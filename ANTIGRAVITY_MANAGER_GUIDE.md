# Antigravity-Manager 本地反代配置与使用指南

本文档记录了基于 **[lbjlaq/Antigravity-Manager](https://github.com/lbjlaq/Antigravity-Manager)** 的本地 AI 模型反代中转站部署、客户端对接与日常运维配置。

---

## 📌 当前环境运行状态

| 配置项 | 当前值 | 说明 |
| :--- | :--- | :--- |
| **运行模式** | Docker 容器 (`antigravity-manager`) | 后台无头常驻，带 Web 管理端 |
| **自启动状态** | ✅ **已开启** (`RestartPolicy: unless-stopped`) | 随系统开机自动启动 |
| **Web 管理后台** | [http://localhost:8045](http://localhost:8045) | 管理账号、配额、模型映射 |
| **后台登录密码** | `admin123456` | 用于登录 Web UI |
| **API 调用 Key** | `sk-antigravity` | 客户端鉴权 API Key |
| **OpenAI 兼容端点** | `http://127.0.0.1:8045/v1` | 适合 Cherry Studio、NextChat、Python 等 |
| **Anthropic 兼容端点** | `http://127.0.0.1:8045` | 适合 Claude Code CLI 原生协议 |
| **数据持久化路径** | `~/.antigravity_tools` | 存储账号 Token、SQLite 数据库及配置 |

---

## 🚀 跨平台部署指引

### 1. Docker 部署（Linux / macOS 通用，自启动推荐）

```bash
# 创建数据目录
mkdir -p ~/.antigravity_tools

# 启动容器并开启开机自启
docker run -d --name antigravity-manager \
  -p 8045:8045 \
  -e API_KEY=sk-antigravity \
  -e WEB_PASSWORD=admin123456 \
  -e ABV_MAX_BODY_SIZE=104857600 \
  -v ~/.antigravity_tools:/root/.antigravity_tools \
  --restart unless-stopped \
  lbjlaq/antigravity-manager:latest
```

> **macOS 说明**：只需在 Docker Desktop / OrbStack 的设置中勾选 **「Start Docker on login」**，容器就会在 Mac 开机后自动启动。

---

### 2. macOS 原生桌面客户端安装 (Homebrew)

如果希望在 Mac 上使用原生 GUI 界面：

```bash
# 订阅官方 Tap 并安装
brew tap lbjlaq/antigravity-manager https://github.com/lbjlaq/Antigravity-Manager
brew install --cask antigravity-tools
```

---

## 🔑 账号授权与配置流程

1. 打开管理后台：**[http://localhost:8045](http://localhost:8045)**，输入密码 `admin123456` 登录。
2. 进入左侧 **「Accounts / 账号管理」** → 点击 **「添加账号」** → 选择 **「OAuth」**。
3. 复制生成的授权链接，在主力浏览器中打开并登录 Google 账号完成授权。
4. 授权成功后，页面会自动同步账号配额与健康状态。
5. **推荐操作**：添加 2 ~ 3 个备用 Google 账号，开启自动轮换负载均衡，消除单号限流冷却。

---

## 🔌 客户端接入配置

### 1. Claude Code CLI

在终端直接注入环境变量启动：

```bash
export ANTHROPIC_API_KEY="sk-antigravity"
export ANTHROPIC_BASE_URL="http://127.0.0.1:8045"
claude
```

> 可将上述两行 `export` 添加到你的 `~/.bashrc` 或 `~/.zshrc` 中永久生效。

---

### 2. Cherry Studio / NextChat / Chatbox

在客户端设置中新建或配置 **OpenAI 自定义服务商**：

- **API 域名 (Base URL)**: `http://127.0.0.1:8045/v1`
- **API Key**: `sk-antigravity`
- **常用模型选择**:
  - `gemini-3.7-flash`（日常问答与高频调用）
  - `gemini-3-pro-high` / `gemini-2.5-pro`（代码分析与深度推理）
  - `claude-3-5-sonnet` / `claude-sonnet-4-5-thinking`（思考模型）

---

### 3. Python (OpenAI SDK)

```python
import openai

client = openai.OpenAI(
    api_key="sk-antigravity",
    base_url="http://127.0.0.1:8045/v1"
)

response = client.chat.completions.create(
    model="gemini-3.7-flash",
    messages=[
        {"role": "user", "content": "你好！"}
    ]
)

print(response.choices[0].message.content)
```

---

### 4. Imagen 3 图片生成接口

```python
import openai
import base64

client = openai.OpenAI(
    api_key="sk-antigravity",
    base_url="http://127.0.0.1:8045/v1"
)

response = client.images.generate(
    model="gemini-3-pro-image",
    prompt="未来赛博朋克城市，霓虹光影，4k细节",
    size="1920x1080",
    quality="hd",
    n=1,
    response_format="b64_json"
)

# 保存图片
with open("output.png", "wb") as f:
    f.write(base64.b64decode(response.data[0].b64_json))
```

---

## 🛠️ 运维与管理命令

```bash
# 查看实时运行日志
docker logs -f antigravity-manager

# 重启反代服务
docker restart antigravity-manager

# 停止反代服务
docker stop antigravity-manager

# 启动已有服务
docker start antigravity-manager

# 升级到最新版本镜像
docker pull lbjlaq/antigravity-manager:latest
docker stop antigravity-manager
docker rm antigravity-manager
# 重新执行 run 命令（数据保存在 ~/.antigravity_tools，不会丢失）
```

---

## ⚠️ 安全与防封建议

1. **小号隔离**：严禁使用日常主力 Google 账号，务必使用专用备用小号。
2. **多账号轮替**：导入 2~3 个账号，利用内置的调度器自动分散 QPS 压力。
3. **后台任务降级**：在后台「模型路由」设置中，可将后台静默任务（如标题生成、摘要）映射至 Flash 模型，保护 Pro 配额。
4. **长文本超时配置**：由于包含长推理或思维链模型，请求超时时间建议保持在 120 秒以上。
