# 架构

本文描述 LumenTray 0.1.0 的实现结构：进程与线程、显示器枚举、DDC/CI 读写、事件驱动、窗口生命周期、设置持久化和已知取舍。

相关文档：[设计语言](design-language.md) · [更新记录](../CHANGELOG.md) · [贡献指南](../CONTRIBUTING.md)

## 1. 目标与边界

LumenTray 的目标只有一条：**让多显示器亮度的日常调整快速、稳定、可解释。**

由此产生的约束：

- 单块显示器失败不能影响其他显示器，也不能清空整个列表
- 空闲时不轮询、不创建 WebView2、不做 DDC 访问
- 每次写入以显示器回读值为准，失败要回滚并说明原因
- 所有依赖都在本机，不需要账号、网络和管理员权限

v1 不做对比度、色温、输入源、HDR、日程、ICC 校色或云同步。范围收窄是为了把枚举与 DDC/CI 容错做扎实。

## 2. 技术栈

| 层 | 选择 | 说明 |
|---|---|---|
| 桌面外壳 | Tauri 2 | 无窗口托盘常驻，按需创建 WebView2 |
| 后端 | Rust 2024 + `windows` 0.62 | 直接调用 Win32、Dxva2、注册表与消息循环 |
| 前端 | React 19 + TypeScript + Vite | 自绘玻璃界面，不依赖组件库 |
| DDC/CI | `crates/ddc-core` | 独立库，同时被应用和 `ddcctl` 使用 |
| 存储 | `serde_json` + 原子写 | 单文件配置，可读、可迁移 |
| 日志 | 自写滚动文件 | 2 MB × 3，进程内 400 行环形缓冲 |
| 单实例 | `tauri-plugin-single-instance` | 第二次启动唤出已有飞窗 |

## 3. 进程与线程

```text
lumentray.exe
├─ 主线程            Tauri 事件循环、托盘、窗口生命周期、命令分发
├─ 隐藏事件窗口线程   Win32 消息循环：热键、显示/设备/电源广播
├─ 快捷键工作线程     80 ms 合并连续按键，顺序写入各显示器
├─ 设置保存线程       700 ms 防抖，合并拖动产生的连续修改
├─ 事件防抖线程       600 / 1200 ms 延迟后触发刷新
├─ Tauri async runtime  spawn_blocking 执行枚举与 DDC 读写
└─ WebView2 子进程    仅飞窗、设置窗、OSD 存在时运行
```

关键约束：

- **主线程永远不执行 DDC 调用。** `list_monitors`、`set_brightness` 和恢复流程都进入 `spawn_blocking`；Dxva2 调用可能阻塞数百毫秒甚至更久。
- **创建窗口的命令必须是 `async fn`。** Tauri 2 的同步命令在主线程执行，创建窗口会等待事件循环，形成自锁。`open_settings` 因此是异步命令。
- **DDC 访问在进程内串行化。** `ddc-core` 用一把进程级互斥锁包住枚举、读取和写入；UI、快捷键、恢复流程即使在不同线程也不会交叉挤占 I2C 通道。不为每块屏起常驻 worker，并发读不会更快，只会增加抖动。
- **关闭窗口即销毁 WebView2。** 托盘进程不保留隐藏的 UI 进程。

## 4. 模块

```text
app/src-tauri/src/
├─ main.rs          进程入口
├─ lib.rs           Tauri builder、命令注册、托盘、窗口、事件窗口
├─ cache.rs         进程内亮度快照与已记录值
├─ hotkeys.rs       RegisterHotKey、组合键解析、录制、合并写入
├─ osd.rs           无焦点 OSD 窗口与定位
├─ settings.rs      设置模型、校验、原子保存、亮度记录
├─ autostart.rs     HKCU Run 与 StartupApproved
├─ diagnostics.rs   诊断信息、日志尾部、剪贴板
└─ logging.rs       滚动日志与环形缓冲

crates/ddc-core/src/lib.rs
├─ 显示器枚举与物理句柄匹配
├─ 高层亮度 API 与 VCP 0x10 回退
├─ 错误分类、瞬态重试、范围缓存
└─ 写入后回读

app/src/
├─ App.tsx          飞窗
├─ Settings.tsx     五页设置
├─ Osd.tsx          OSD 胶囊
├─ theme.css        设计令牌
├─ App.css         飞窗样式
├─ Settings.css    设置窗样式
└─ Osd.css         OSD 样式
```

## 5. 显示器枚举与身份

每次枚举按以下顺序进行：

1. `QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS)` 获取活动显示路径：
   - source：GDI 名（如 `\\.\DISPLAY1`）
   - target：友好名、`monitorDevicePath`、输出技术（DisplayPort / HDMI / DVI 等）
2. `EnumDisplayMonitors` + `GetMonitorInfoW` 获取 `HMONITOR`、桌面矩形和主屏标记。
3. 用 GDI 名把路径信息与 `HMONITOR` 配对。
4. `GetNumberOfPhysicalMonitorsFromHMONITOR`：
   - 返回 0 → 视为虚拟 / 远程 / 内部显示，记录日志并跳过，不报错。
   - 调用失败 → 记录并跳过该 `HMONITOR`，继续处理其他显示器。
5. `GetPhysicalMonitorsFromHMONITOR` 获取物理句柄；一个 `HMONITOR` 可能对应多个物理显示器（MST），逐个保留。
6. 生成稳定 ID：优先 `monitorDevicePath#index`，路径为空时退回 `GDI 名#index`。亮度记录按该 ID 持久化。

枚举结果不会因为单屏失败而清空其他项；失败信息写在 `notes` 与每屏的 `error` 字段中。前端按 `x`、`y` 排序编号，并在光图中按真实分辨率与相对位置排布。

## 6. DDC/CI 读写

### 6.1 读取

对每块物理显示器：

1. 尝试高层 API `GetMonitorBrightness`，返回 min / current / max。
2. 高层失败时尝试 VCP `0x10`：`GetVCPFeatureAndVCPFeatureReply`，min 按 0 处理。
3. 两者都失败时返回高层调用的错误；卡片保留，前端显示「上次 N%」与原因。

范围会归一化：min 与 max 相等、max 为 0 或范围倒置时回退到 0–100，避免除零和异常百分比。

### 6.2 写入

1. 使用该显示器缓存的范围；没有缓存时先读一次范围。
2. 百分比换算为原始值，尝试 `SetMonitorBrightness`。
3. 高层失败时读取 VCP 的 max，换算后调用 `SetVCPFeature(0x10)`。
4. 写入失败后等待 80 ms 再回读一次，把显示器的真实值返回给前端。
5. 返回 `WriteOutcome { brightness, error }`；成功时清空错误，失败时保留错误并让前端回滚。

范围按显示器 ID 缓存 30 分钟，减少每次写入前的额外 DDC 往返；写入失败会清除该屏缓存。

### 6.3 错误分类与重试

| 类别 | 错误码 | 行为 |
|---|---|---|
| 瞬态 I2C / 协议 | `0xC0262582` `0xC0262583` `0xC0262586` `0xC0262589` `0xC026258A` `0xC026258B` | 最多 3 次，间隔 50 / 100 ms |
| 不支持 | `0xC0262580` `0xC0262581` `0xC0262584` | 不重试，切换下一方法 |
| 显示器消失 | `0xC026258D` | 标记不可达，等待事件重新枚举 |
| 未知 | 其他 | 记录原始码，不盲目重试 |

重试只针对瞬态错误。像本机 Dell U2412M 的持续 `0xC0262582`，重试 20 次也不会恢复，因此产品选择快速失败、保留卡片并提示断电重启。

飞窗在出现瞬态错误后还会做一次免费的静默重读：等待 3 秒后重新枚举，仍失败才停留在错误卡片。显示器刚上电或刚接回时 DDC 通道可能短暂无响应，这一次重读通常就能恢复；同一次故障不会反复轮询，重新成功后才重新武装。

### 6.4 进程级串行化

Dxva2 最终读写的是 GPU 的 I2C 总线。飞窗滑杆、全局快捷键、启动/唤醒恢复和事件刷新可能同时发起 DDC 请求，两个线程在一条通道上交叉收发字节会触发本可避免的 I2C 错误（如 `0xC0262582`）。

`ddc-core` 因此持有一把进程级互斥锁 `DDC_LOCK`：

- `enumerate()`、`get_brightness()`、`set_brightness()` 入口先取锁，整个枚举或读写事务完成后再释放。
- 热键工作线程、`spawn_blocking` 任务与恢复流程共用同一把锁，锁顺序由排队决定，不依赖 Windows 是否代为串行化。
- 锁只覆盖 DDC 事务，不覆盖等待写入回读的 80 ms 或前端逻辑；单次事务失败不会永久持锁。
- `ddcctl` 是独立进程，不受应用锁影响。

### 6.5 为什么每次写入都重新枚举

`with_monitor(id, ...)` 会重新查询显示路径与物理句柄，按稳定 ID 找到目标显示器。这样不需要长期持有可能失效的物理句柄，代价是每次写入多一次枚举开销；实测稳态写入约 60–100 ms，仍在可接受范围。

## 7. 亮度数据流

### 7.1 滑杆写入

```text
拖动滑杆
  → 前端立即更新数值（乐观 UI）
  → 同一屏上一笔未返回时只保留最新目标值
  → invoke("set_brightness", { id, percent })
  → spawn_blocking 执行 ddc-core 写入并回读
  → WriteOutcome 返回
  → 成功：更新缓存、设置中的上次亮度与飞窗数值
  → 失败：显示错误，回滚到回读值或最后已知值
```

前端用 `pending` 保存最新目标值、`inFlight` 标记正在写入的显示器。上一笔返回后如果还有新值，立即续写，避免 DDC 队列堆积。

### 7.2 全部联动

总控只选取 `brightness != null && error == null` 的显示器，逐屏调用同一写入路径。不可控屏幕保持原状；数值不一致时总控显示平均值和「混合」标记。

### 7.3 快捷键

```text
WM_HOTKEY
  → 80 ms 合并连续按键，累计本次调节方向
  → 读取可达屏的缓存值（缓存过期才重新枚举）
  → 逐屏写入并回读
  → 更新缓存、上次亮度
  → emit("brightness-changed") 回填已打开的飞窗
  → 鼠标所在屏显示 OSD
```

热键工作线程顺序写屏；即使飞窗同时在写入，`ddc-core` 的进程级锁也会让两边的 DDC 事务排队，不会交叉挤占通道。达到 0% 或 100% 的屏幕计入「已同步」，但不重复写入。

### 7.4 启动与唤醒恢复

开启「恢复上次亮度」后：

- 启动时后台执行一次恢复；唤醒事件延迟 1200 ms 后执行。
- 当前值与记录值相同则跳过写入。
- 用户在此期间手动调节任意屏幕，会中止剩余恢复，避免覆盖用户操作。
- 单屏恢复失败只记录日志，不影响其他屏幕。

## 8. 事件驱动

启动时在独立线程创建一个**顶层隐藏窗口**（`WS_POPUP` + `WS_EX_TOOLWINDOW`）。不能使用 message-only 窗口，因为收不到广播消息。

| 消息 | 处理 |
|---|---|
| `WM_DISPLAYCHANGE` | 防抖 600 ms 后重新枚举 |
| `WM_DEVICECHANGE` | 防抖 600 ms 后重新枚举 |
| `WM_POWERBROADCAST`（恢复） | 延迟 1200 ms，执行恢复并按需刷新 |
| `WM_HOTKEY` | 交给快捷键模块合并处理 |

每次事件递增序号；防抖线程醒来后序号不匹配就直接退出，避免短时间多次事件造成重复枚举。没有常驻轮询定时器。飞窗打开时先用 `get_monitor_snapshot` 水合进程内缓存，再后台调用 `list_monitors` 更新。

## 9. 窗口与交互

| 窗口 | 标签 | 创建方式 | 关键属性 |
|---|---|---|---|
| 飞窗 | `flyout` | 点击托盘时创建 | 380 宽、高度随内容、无边框、透明、置顶、跳过任务栏、关闭即销毁 |
| 设置 | `settings` | 托盘菜单或飞窗齿轮 | 720 × 560、无边框、透明、不置顶、居中 |
| OSD | `osd` | 快捷键调节时按需创建 | 400 × 128、置顶、不聚焦、跳过任务栏、鼠标穿透、30 s 空闲销毁 |
| 事件窗口 | 无 | 启动时创建 | 0 × 0 顶层隐藏窗口，仅接收消息 |

飞窗定位：

- 以托盘图标点击位置为锚点，找到所在显示器与工作区。
- 任务栏在底部时贴工作区底部；在顶部、左侧、右侧时按方向翻转。
- 最终位置夹紧在工作区内，先定位再显示，避免从屏幕角落飞入。

飞窗关闭：

- 失焦关闭要求窗口至少获得过一次焦点且打开超过 250 ms，避免启动瞬间误关。
- 托盘点击切换时记录关闭时间，300 ms 内的二次点击忽略，避免双击导致重开。
- 打开设置窗时先创建设置窗，再关闭飞窗，避免焦点转移产生闪回。

OSD：

- 不使用 `WS_EX_APPWINDOW`，强制 `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE`，用 `SW_SHOWNOACTIVATE` 显示。
- 鼠标穿透，不抢焦点，不进任务栏与 `Alt` + `Tab`。
- 1.6 s 后淡出 240 ms，30 s 无操作销毁；下次热键重新创建。

## 10. 设置与持久化

位置：`%APPDATA%\LumenTray\settings.json`。

```json
{
  "version": 2,
  "theme": "system",
  "glassOpacity": 95,
  "launchAtLogin": false,
  "restoreOnWake": false,
  "step": 5,
  "hotkeysEnabled": false,
  "hotkeyIncrease": "Alt+F1",
  "hotkeyDecrease": "Alt+F2",
  "hotkeyStep": 10,
  "monitors": {
    "\\\\?\\DISPLAY#...\\#0": { "lastBrightness": 90 }
  }
}
```

- 缺字段使用默认值；非法步长、玻璃不透明度或热键在加载时回退。
- 解析失败的文件改名保留为 `settings.json.corrupt-<时间戳>`，应用使用默认设置启动。
- 保存为临时文件 → `sync_all` → 复制旧文件为 `settings.json.bak` → `rename` 替换。
- 保存线程 700 ms 防抖，合并拖动产生的连续修改。
- 「开机启动」在加载时以注册表为准，而不是以 JSON 为准；用户在任务管理器里禁用启动项后，设置页会正确显示为关闭。

## 11. 日志与诊断

- 日志：`%APPDATA%\LumenTray\logs\lumentray.log`，2 MB × 3 滚动。
- 进程内保留最近 400 行，日志文件不可读时诊断页仍有内容。
- 记录枚举耗时、每屏读写结果与错误码、事件与防抖、热键注册与调节耗时、设置保存失败。
- 诊断页显示程序 / 配置 / 日志路径、热键状态、每屏设备路径、连接方式、DDC 方法、原始范围、错误和日志尾部；可复制为纯文本报告。
- 日志不记录账号、密钥或屏幕内容。

## 12. 安全边界

- 无网络代码，不访问云端，无遥测。
- 不需要管理员权限。
- 只有开启「开机启动」时写入 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`；关闭时同时删除 `StartupApproved\Run` 中的禁用标记。
- 不修改系统主题，不注入其他进程，不读取屏幕内容。
- 安装包未签名，SmartScreen 可能提示未知发布者。

## 13. 性能与已知限制

2026-11-13 在开发机（Windows 11，三台显示器）release 0.1.0 实测：

| 场景 | 实测 |
|---|---:|
| 空闲托盘进程 | 约 14.9 MB 工作集 / 2.4 MB Private |
| 飞窗首次打开 | 235–271 ms |
| 设置窗打开 | 约 68 ms |
| 稳态单屏写入 | 60–100 ms |
| 完整枚举（三屏） | 约 180–200 ms |
| 安装包 | 1.8 MB |
| 主程序 | 约 4.95 MiB |

已知限制：

- **WebView2 内存基线高。** 飞窗打开时包含 WebView2 子进程的 Private 内存约 192 MB，高于最初 150 MB 的预算。关闭窗口后 WebView2 进程退出，托盘进程回到基线。保持玻璃视觉质量的前提下暂无更低方案，记录为已知偏差。
- **DDC 访问串行执行。** 进程内由 `DDC_LOCK` 排队，同一条 I2C 通道上的枚举与写入不会交叉；总耗时无法通过并发显著缩短。
- **DDC 调用不可取消。** 卡住的调用可能让单次操作等待较久；应用不 join 卡住的调用，退出不被单屏拖死。
- **设备路径变化会重新记录亮度。** 换接口、扩展坞或部分 KVM 后，显示器 ID 可能变化，历史亮度记录不会自动跟随。

## 14. 验证

- `crates/ddc-core`：百分比换算、范围归一化、瞬态错误分类、错误码名称。
- `app/src-tauri`：组合键解析与校验、日志轮转、设置迁移 / 损坏文件 / 备份。
- 真实三屏：枚举、读写、回读、不可达卡片、全部联动、快捷键、OSD、恢复亮度、深浅色与 DPR 1/2。
- WebView2 调试辅助脚本位于 `tools/cdp-*.mjs`，可在不产生真实鼠标事件的情况下读取 DOM、截图和执行表达式。
- CI 在 Windows runner 上执行 `cargo fmt --check`、`cargo clippy -D warnings`、`cargo test` 与前端构建。
