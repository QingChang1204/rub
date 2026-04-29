# L2级别的隐形防屏蔽与原子表单递交 (Stealth & Form Filling)

在现代高防站点环境中，传统的 DOM 操作（如通过元素方法强行改值）非常容易被各类反作弊探针（如 reCAPTCHA, Google reCAPTCHA v3 或自定义前端校验器）侦测为“非人类交互”导致请求熔断。

对此，`rub` 部署了底层的 L1 防检测与 L2 类人拟真特征（Humanize）。本章节将以安全填充场景为例，示范安全特性。

## 配置 Humanize (拟人化特征)

你可以通过统一的环境变量使得该会话成为一个强拟人配置：

```bash
export RUB_HUMANIZE=true
export RUB_SESSION="stealth_session"
```

这会开启：
* **鼠标曲线扰动**：所有基于目标的 `click` 将会携带合理的移动时长（非瞬移）。
* **光标停顿与退格打字延迟**：模拟了合理的输入顿挫感，避开按键节奏探针扫描。

## 批量防干扰填写 (Atomic Fill)

处理长表单时，反复调度独立 `click` 和 `type` 可能引起中途元素漂移（DOM Context Mutated）错误。`rub fill` 的 `--atomic` 特性将输入动作打包到一次状态检视中锁定。

```bash
rub open https://httpbin.org/forms/post

# 使用 JSON 数组指定输入目标与文本映射
rub fill --atomic '[
  {"selector":"input[name=custname]", "value":"John Doe"},
  {"selector":"input[name=custtel]", "value":"555-1234"},
  {"selector":"input[name=custemail]", "value":"john@example.com"}
]'

# 单选框 / 复选框 建议单独调用 click 而非原子文本写入
rub click --selector 'input[name=size][value=large]'
rub click --selector 'input[name=topping][value=bacon]'

rub click --selector 'button'
```

### 【排错最佳实践】Atomic Fill 限制事项

> [!WARNING]
> 经过最新底层强网验证测试，`fill --atomic v1` **只能对支持可安全回滚** 的组件（如单行 `input`，多行 `textarea` 或部分标准的 `select`）发起原子性写入请求。
>
> 如果你的 `fill` 结构中错误囊括了 `radio` 或者 `checkbox`，底层抛出异常 `unsupported_value_target` 并阻止整个写入链，防止发生不可逆的提交！请对诸如 `radio/checkbox` 的开关类元素单独通过 `rub click` 完成激活。

而且 `{"value": ...}` 属性强制校验为 `string`，杜绝使用 `boolean` 类型以维护 JSON 类型树健康。
