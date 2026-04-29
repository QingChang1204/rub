# 深度抓取与表格遍历映射 (Advanced Extraction)

`rub` 最强大的特征之一，在于其开箱即用的结构化数据抓取能力，且底层封装了复杂的异步等待、滚动加载 (Infinite Scroll) 以及防抖去重机制。

## 基础列表抓取

传统流程里我们需要循环获取 `document.querySelectorAll`。在 `rub` 中，你可以运用 `inspect list` 生成一维或多维抓取对象。

### Hacker News 多页连续抓取实战

以获取 Hacker News 首页新闻为主，我们可以连续滚动多页获取 `30` 条精准新闻标题，而不会被重复元素污染。

```bash
export RUB_HOME=/tmp/rub-hn-scraper
rub open https://news.ycombinator.com/news --rub-home $RUB_HOME

rub inspect list \
  --collection "tr.athing" \
  --field "title=text:.titleline > a" \
  --scan-until 30 \
  --rub-home $RUB_HOME \
  --json-pretty

rub teardown --rub-home $RUB_HOME
```

**解析：**
* `--collection "tr.athing"`：界定每个新闻行元素作为一个迭代映射单元 (Row Root)。
* `--field "title=text:.titleline > a"`：在映射单元内部建立一个名叫 `title` 的域，值来源于子元素的文本节点。
* `--scan-until 30`：告知 `rub`，一旦提取不到 30 个就一直往下滚动。`rub` 会在滚动时加入防抖间隔 (`--settle-ms` 和 `--stall-limit`)，并在获取满后立刻终止拦截请求。

## 高级去重防御的必要性

在大多数现代前端瀑布流视图中，元素的挂载点或 DOM 地址在滚动时极易重用或重排。为了确保数据完整，你可以增加一个作为 Key 的唯一锚点字段：

```bash
rub inspect list \
  --collection "tr.athing" \
  --field "rank=.rank" \
  --field "title=text:.titleline > a" \
  --scan-until 100 \
  --scan-key rank \
  --rub-home $RUB_HOME
```
*引入 `--scan-key rank` 会从底层状态机中丢弃可能存在的重复提取结果，保持 `items` 数组的干净程度。*
