---
name: video_production
description: 程序化视频/动画制作（Procedural Video in Code）。当用户要求制作视频、动画短片、动态排版片、产品宣传片、音乐 MV、手绘风格动画时触发。基于已安装的 opus-video-skills 工程模板（每帧是时间纯函数，无头 Chrome 逐帧渲染 + FFmpeg 编码）。
when_to_use: 制作 MP4 视频、动画短片、动态排版/运镜短片、产品宣传视频、音乐可视化、手绘水彩动画、3D 几何视觉片头时。
---

# 程序化视频制作专家 (Video Production Expert)

本技能背后是一套**已安装在本地、可直接复用的完整工程模板**（opus-video-skills）。
**核心纪律：永远从模板脚手架出发，禁止从零手写场景代码。**

## 0. 安装位置（绝对路径）

```
SKILLS_ROOT=/home/w/.local/share/free-api-chat/skills/opus-video-skills/skills
├── kinetic-reel/       → 3D 动态排版 / 产品宣传片 / 作品集 / 开场片头
│   ├── SKILL.md        → 完整工作流说明（必读）
│   ├── references/style-guide.md → 色值/字体/坐标/运镜配方（必读全文）
│   ├── references/sound.md       → 配乐合成说明
│   ├── template/       → 可运行的 17s 演示（reel.js 含全部视觉原语）
│   ├── examples/work-reel-26/    → 84s 真实成品实例
│   └── scripts/new_project.sh    → 脚手架脚本
└── painted-animation/  → 水彩手绘插画 / 音乐 MV / 动态歌词短片
    ├── SKILL.md        → 完整工作流说明（必读）
    ├── template/       → p5.js + p5.brush 水彩笔刷工程
    │   └── ANIMATION_GUIDE.md    → 角色动画原语（蓄力/挤压拉伸/惯性）
    └── examples/xiaozhen/        → 31s 成品实例
```

## 1. 风格选择（选错方向=先天废品）

| 需求 | 用哪个 |
|---|---|
| 产品宣传/作品集/数据展示/片头/HUD 风格 | **kinetic-reel** |
| 音乐 MV/歌词动画/水彩插画故事/角色绘本感 | **painted-animation** |
| ⚠️ 角色骨骼级 3D 动画（人物拧魔方/打斗/舞蹈） | **两者都不对口**——kinetic 强项是排版与抽象几何，painted 是 2D 插画风。接此类需求须先向用户说明局限：可做的是"几何化角色示意"而非真实骨骼动画 |

## 2. 强制工作流（缺一步=质量腰斩）

```bash
# ① 脚手架——必须走这里，禁止从零建 HTML
bash $SKILLS_ROOT/kinetic-reel/scripts/new_project.sh <目标目录>

# ② 必读配方——不写就不准动工：
#    kinetic:  SKILL.md + references/style-guide.md + references/sound.md 全文
#    painted:  SKILL.md + template/ANIMATION_GUIDE.md 全文
#    再 skim examples/ 下成品实例的 reel.js / xz_shots.js 看组织方式

# ③ 产出 STORYBOARD.md（时间预算、每镜头 headline/机制/转场/声音行）

# ④ 实现：改 template 里的 reel/cues.js（节拍表）与 reel/reel.js（每镜头一个
#    sName(t) 函数），复用模板既有原语（terrain/liquid/knot/cloud/dropWord/
#    chip/stat/HUD），不自己另造轮子
```

## 3. 渲染与验证

```bash
# 接触表抽帧自检（模型当前无图像输入通道——可生成但你看不见，
# 应在交付时如实披露"画面未经视觉复核"）
node render.mjs --sheet=1,4,7,10 --cols=3 --w=640 --out=out/check/sheet.jpg

# 全量渲染 + 编码
node render.mjs --frames --workers=4
node render.mjs --encode --audio=<score.m4a> --out=out/reel.mp4 --fps=30
ffprobe -v error -show_entries format=duration -i out/reel.mp4  # 核验
```

**Linux 环境已验证的两个硬坑**（实测血泪，直接照做）：
- Chromium `--user-data-dir` 必须用**短路径**（如 `/tmp/p5chrome`），过深触发
  SingletonSocket 108 字符上限 → 启动静默失败/全黑帧
- `renderer.domElement` 必须 `document.body.appendChild` 挂载——否则 canvas
  不存在，Puppeteer 截图必黑
- 确定性渲染：禁用 `millis()`/`requestAnimationFrame` 实时循环，帧态只能是
  `frameIdx`/`t` 的纯函数

## 4. 交付披露义务

- 声明产物路径 + 时长/分辨率/帧数（ffprobe 实测值）
- 声明"未经视觉复核"（图像通道缺失）——让用户先看，反馈后再迭代
- 风格选型若偏离两大模板强项，必须明示近似程度
