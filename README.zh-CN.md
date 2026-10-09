# omaread

[English](README.md) · **中文**

**一个小巧的终端 EPUB 阅读器。** 一个 Rust 二进制文件：书库、能在移动文件或
换机器后保留的阅读进度、正文中的图片与 MathML 公式，以及一条人或 Agent 都能
驱动的命令行。

没有服务、没有守护进程、没有数据库：阅读进度就是一个纯文本、只追加的日志，
可以阅读、diff 和同步。

## 安装

需要 Rust 工具链。

```bash
git clone https://github.com/77-223255/omaread
cd omaread
cargo build --release
install -m755 target/release/omaread ~/.local/bin/
```

GitHub 上的 release 是静态链接的（musl）；本地自行编译则链接系统库。

想双击 EPUB 直接打开，再装桌面项、启动器和图标：

```bash
install -Dm644 contrib/omaread.desktop ~/.local/share/applications/
install -Dm755 contrib/omaread-open ~/.local/bin/
install -Dm644 contrib/omaread.svg ~/.local/share/icons/hicolor/scalable/apps/
update-desktop-database -q ~/.local/share/applications
xdg-mime default omaread.desktop application/epub+zip
```

`omaread-open` 通过 `xdg-terminal-exec` 询问系统该用哪个终端，而不是猜。

## 阅读

先收录一次，再打开书库：

```bash
omaread scan ~/Books   # 记住该目录下所有 EPUB
omaread                # 打开书库
```

用 `j`/`k` 移动，`Enter` 打开；下次会从上次停下的地方继续。
只读单个文件而不入库：`omaread ~/Downloads/book.epub`。

在阅读器里按 `?` 看完整按键表。方向键与 `j`/`k` 等效；`←`/`→` 在阅读时切换
章节，在光标模式下按字符移动。常用的是：

| 按键 | |
| --- | --- |
| `j` `k` `↓` `↑` | 上/下一行 |
| `Space` `Backspace`、`PgDn` `PgUp` | 上/下一页 |
| `Ctrl-d` `Ctrl-u` | 半页 |
| `gg` `G` | 章节开头/末尾 |
| `L` `H`（`]` `[`、`→` `←`）| 下/上一章 |
| `t` `Tab` | 目录 |
| `/`、`n` `N` | 本书内搜索、下/上一个结果 |
| `i` | 文本光标（跟链接、移动）|
| `v`、`y` | vision 模式：选取文字并复制到剪贴板 |
| `h` `l` `←` `→` | 光标按字符移动 |
| `Enter`、`Ctrl-o` | 跟随链接、返回 |
| `Esc` | 后退一步：清除搜索、退出光标、关闭目录 |
| `q` | 回书库 |
| `Q` | 退出 |
| `?` | 这份按键表 |

书库使用同一套移动键——`j`/`k` 或 `↓`/`↑`，`Space`、`PgDn` 翻页——并且分成两层：
先按字母顺序列出作者，再列出某位作者按标题排序的书。`Enter`（或 `l`）进入，
`Esc`（或 `h`）返回，`/` 筛选当前这一层。也可以直接用鼠标：点击作者或书即可
进入或打开，滚轮可以滚动。

## 命令

`BOOK` 指书的 id（或至少 8 个字符的唯一前缀）、书库认识的标题或作者，或磁盘上的
文件。

| 命令 | |
| --- | --- |
| `omaread` | 读书库 |
| `omaread BOOK [--chapter N\|HREF] [--at TEXT]` | 读一本书，并可定位 |
| `omaread list [--json] [--filter TEXT]` | 以表格列出书库 |
| `omaread authors [--json]` | 所有不同的作者名，以及各自出现在几本书上 |
| `omaread scan DIR [--filenames]` | 收录某个目录下的书，并识别移动过的书 |
| `omaread show BOOK [--json]` | 书自称的信息 |
| `omaread set BOOK field=value … [--json]` | 更正一本书的信息，不改动文件 |
| `omaread edit TEXT field=value … [--json]` | 一次更正某个词命中的每一本书 |
| `omaread forget BOOK [--json]` | 移出一本书，或整个书架 |
| `omaread journal status [--json]` | 事件日志里有什么，还有多少仍然有用 |
| `omaread journal compact [--json]` | 把日志折算成仍然有用的事件 |
| `omaread find TEXT` | 全库搜索并阅读命中的位置 |
| `omaread export [DIR] [--force] [--reindex] [--embed]` | 导出 Markdown，每章一个文件 |
| `omaread inspect BOOK` | 书文件里有什么，不经过书库 |
| `omaread images BOOK` | 书文件中的图片，以及各自多大 |
| `omaread blocks N BOOK` | 书文件某一章解析出的块 |
| `omaread help [COMMAND]` | 同样的总览，或某条命令自己的帮助 |
| `omaread --version` | 版本 |

`set` 支持的字段：`title`、`authors`、`series`、`series-index`、`tags`、
`rating`、`publisher`、`year`、`language`。值为空表示清除该字段。`authors` 和
`tags` 接受逗号分隔的列表；当名字自身带逗号时，用 JSON 数组：
`set BOOK 'authors=["Le Guin, Ursula"]'`。更正会作为一条日志事件：优先于
文件内容、重扫后仍在，且绝不写入 EPUB。

### 给脚本和 agent

每条命令都是对书库的一个动词——`scan` 增加、`set` 和 `edit` 更正、`forget`
移除、`list`/`show`/`authors` 读取——凡是程序要解析的都有 `--json`。更正总是
日志事件，所以 agent 不需要什么私有文件格式，只要命令行。

要整理元数据，用 `edit`：它一次作用在某个词命中的整组书上。同一个作者被十几
个文件写成十几种拼法，正是它存在的理由：

```bash
omaread authors                                   # 各种拼法，带计数
omaread list --filter "Murakami" --json           # 预览一个词命中的书
omaread edit "Haruki Murakami" 'authors=["村上春树"]' --json
```

`export` 用于喂给搜索引擎；`--reindex` 把结果交给
[qmd](https://github.com/tobi/qmd)，`--embed` 还会更新 embedding。装了 qmd 时
`find` 用它的索引，没装则直接搜索书本。

## 设置与数据

设置位于 `~/.config/omaread/config.toml`，首次启动时生成并带注释：

```toml
# 日志目录：一个本地文件，是阅读进度的唯一事实来源。它会在原地折叠，
# 所以不打算在机器之间共享。
# journal_dir = "~/.local/share/omaread/journal"

# 图片如何绘制。注释掉则询问终端，在 kitty、sixel、quad 中
# 选最好的。在 tmux 里只有 quad 可用，因为 tmux 自己管理屏幕。
# images = "sixel"
```

日志是唯一事实来源：一个本地 JSONL 日志，一行一个事件，位于
`~/.local/share/omaread/journal/journal.jsonl`，启动时折算出其余状态。由于书按内容
哈希识别，移动或改名都会保留元数据与阅读进度。

### 日志会自行清理

这样的日志只会越长越大，所以会不定期折算一次。折算会把书库真正需要的事件写回去
——每本书实际所在的每个文件一条 `book_seen`、一条 `metadata_set` 装下文件装不下的
字段、一条 `position_set`——并丢掉它们背后的历史：标题是怎么更正的、书在移动前
在哪里、一本被移出又重新读入的书。从折算后的日志重建的书库和原来**逐字节相同**。

日志超过 256 KB 时会自动折算；`omaread journal` 也能随时查看和触发：

```bash
omaread journal status            # 日志里有什么，还有多少是死重量
omaread journal compact           # 立刻折算
omaread journal status --json     # 同样的内容，给程序读
```

日志属于本机，不打算在机器之间共享。旧版本留下的、每台机器一个的
`journal-<主机名>.jsonl` 会在下一次写入时被折进这一个文件，旧文件随之删除。

## 其他

- **图片。** 图片由普通字符格组成：一个字格分成四个象限，每象限是该小块图像的一个
  像素，用两种颜色绘制。封面和固定版式页占满一屏；正文图片最多放大四倍填满；小标记
  （字形大小的图，比如脚注标记）直接隐藏，让句子连起来读。图片居中，放大不超过自身
  格数的四倍。终端支持时用 kitty 或 sixel 画真实像素；象限块是所有终端都会得到的
  回退（tmux 内也是），它随文字一起滚动和重绘。
- **Vision 模式。** `v` 在光标处开始选取，移动键扩展选区，`y` 用终端自己的剪贴板
  转义（OSC 52）复制所选内容，ssh 下同样有效；`Esc` 或 `v` 取消选区。
- **页面。** 一个隐形光标记录阅读位置：它从第一行开始，滚轮/翻页移动它，光标所在
  段落保持全色，页面上其余文字混回背景变暗。页面跟随光标保持在中央，所以章节开头
  和结尾的段落也能像中间的一样依次高亮。阅读位置就是光标的字符位置，重开时你离开
  时的那一行会回到屏幕中央。
- **公式。** MathML 排成一行；存在 Unicode 上下标时用真正的上下标，否则写成
  `_x` / `^(x)`。
- **主题。** 装了 Omarchy 就跟随当前主题，否则用内置配色。
- **语言。** 界面跟随 `LC_ALL` / `LC_MESSAGES` / `LANG`；机器可读的输出
  （`--json`、表格）保持英文。

## 尚未支持

PDF、MOBI 等 Kindle 格式；双栏并排阅读。

## 许可

MIT，见 [LICENSE](LICENSE)。

## 致谢

omaread 最初 fork 自 Alexander Zeitler 的
[**omalibre**](https://github.com/AlexZeitler/omalibre)，之后为个人使用做了
重写——如今的书库、日志与阅读器都是它自己的。原始的 MIT 许可与版权声明保留在
[LICENSE](LICENSE) 中。
