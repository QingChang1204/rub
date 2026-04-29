# Rub CLI 实战手册：核心理念与最佳实践指南

欢迎查阅 Rub CLI 进阶使用手册。本手册专为需要在防自动化屏蔽环境下执行高稳定性浏览器操作的 AI Agent 和高级开发者设计。

## 核心设计哲学

`rub` 不仅仅是一个无头浏览器的包裹器。它的设计初衷是**告别面向不可靠 DOM 的 `exec` (JavaScript 执行) 拼凑**，转而提供一组具备**规范状态机、严格隔离屏障、以及防抖验证**的产品级 CLI 表层命令。

当你需要在某个页面读取或写入内容时，请优先使用符合本手册推荐组合的规范指令集，而非直接通过 `rub exec` 发送未经安全包裹的 JavaScript。

### 我们强调的基线法则

1. **先观测，后修改**：任何修改动作前，应通过 `rub observe`, `rub inspect page` 或 `rub state` 确认当前实际上下文。
2. **规范化提取优先**：获取数据请使用 `rub inspect list` 或 `rub extract`，它们内置了基于 CSS/ARIA 描述的结构化抓取映射。
3. **隔离会话环境**：对于所有的稳定工作流流水线，请为它指派一个独立的 `RUB_HOME` 目录：
   ```bash
   export RUB_HOME=/tmp/rub-workflow-demo
   ```
4. **统一隐形特征参数**：切勿在一个会话中临时切换隐形策略。如果启用了 `--humanize`，请跨整个会话生命周期保持开启。

## 目录结构

* [01. 深度抓取与表格遍历映射 (Advanced Extraction)](01-advanced-extraction-and-scanning.md)
* [02. L2级别的隐形防屏蔽与原子表单递交 (Stealth & Atomic Fill)](02-atomic-form-filling-and-stealth.md)
* [03. 管道拼接与无缝状态网络拦截 (Pipelining Orchestration)](03-pipeline-and-wait-orchestration.md)
