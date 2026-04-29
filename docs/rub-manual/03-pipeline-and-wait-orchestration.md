# 管道拼接与无缝网络断言 (Pipelining & Wait Orchestration)

频繁跨 CLI 进程启动会使得环境准备变得拖沓。针对多阶段事务，`rub` 内置了强大的 `pipe` 进行操作编排。

## 使用 `rub pipe` 的威力

由于 `rub pipe` 接驳的是标准 CLI 流，你可以利用它单次进入浏览器环境完成完整的：登录、等待验证码、加载新界面、收集有效数据链。

以下示范如何连贯通过一段 JSON 数组让内核无缝执行命令梯次：

```bash
export RUB_HOME=/tmp/rub-orchestrator
# 示例：打开站点 -> 断言元素存在 -> 将标题一并购出
rub pipe '[
  {
    "command":"open",
    "args":{"url":"https://example.com"}
  },
  {
    "command":"wait",
    "args":{"selector":"h1"}
  },
  {
    "command":"extract",
    "args":{"spec":{"title":"h1"}}
  }
]' --rub-home $RUB_HOME
```

*最终的标准输出中将会打包呈现 `steps[2].result`，便于你的主控调度程序快速消费。*

## 利用 `rub wait` 压制异步幽灵

现代 SPA （单页面应用）或基于 React/SSR 的动态水化组件会导致刚加载页面时数据不完全可用。传统的设定 `Sleep` 是脆弱的。

如果在独立的步骤栈中，请积极使用等值守校验指令：

```bash
rub wait --selector 'pre' 

# 或监听特定文案显现
rub wait --text 'Order Success' 
```

### 网络层验证 (Wait State)
配合后端的执行结果（例如前面测试中的 `/post` API 回调）：
即使 DOM 没有大面积刷变，仅渲染了一个载荷 `<pre>`，可以通过 `rub inspect text --selector pre` 配合等待确认，准确将请求提交的结果反馈给调用者。

---
> 熟练驾驭 Pipeline、精确的提取策略、稳固隐蔽的操作手法，将是跨接新一代 Web 自动化工程链的不二法门。
