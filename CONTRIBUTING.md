# 贡献指南

感谢你愿意改进 LumenTray。这个项目由个人维护，欢迎高质量的 Issue 和 Pull Request。

## 报告问题

- 先阅读 README 的「常见问题」，并搜索已有 Issue。
- 使用 Bug 报告模板，附上 Windows 版本、程序版本、安装方式和显示器型号。
- 涉及 DDC/CI 行为时，说明显示器连接方式（DisplayPort / HDMI / USB-C）以及显示器 OSD 中是否启用 DDC/CI。
- **不要上传完整的显示器设备路径、EDID 序列号或未打码的桌面截图。**
- 安全漏洞请不要开公开 Issue，按 [SECURITY.md](SECURITY.md) 私下报告。

## 开发环境

- Windows 10 / 11
- Rust stable（MSVC 工具链）
- Visual Studio C++ 生成工具
- Node.js 20+
- WebView2 运行时（Windows 11 已自带）

```powershell
cd app
npm install
npm run tauri dev
```

命令行调试工具：

```powershell
cargo run --manifest-path tools/ddcctl/Cargo.toml -- list
cargo run --manifest-path tools/ddcctl/Cargo.toml -- get DISPLAY1
```

## 代码约定

- Rust 使用 `cargo fmt` 格式化，并通过 `cargo clippy -- -D warnings` 与 `cargo test`。
- 前端使用 TypeScript；提交前运行 `npm run build` 确认类型检查与构建通过。
- DDC/CI 枚举、方法选择与错误分类以 [docs/architecture.md](docs/architecture.md) 为准；修改控制层时请补充测试。
- 项目不引入网络、云服务或遥测。所有显示器控制必须在本机完成。
- 新增依赖需要说明用途，优先使用标准库或已有依赖。
- 视觉改动请遵循 [docs/design-language.md](docs/design-language.md) 的字号、颜色与数字规则。

## 提交 Pull Request

1. 从 `main` 切出主题分支。
2. 一个 PR 只处理一件事，提交信息使用 `feat:`、`fix:`、`docs:`、`refactor:` 或 `chore:` 前缀。
3. 在 PR 中写清改动原因、验证方式和影响的显示器/系统范围。
4. 涉及 UI 的改动请附截图；涉及设备行为的改动请说明真实显示器验证结果。
5. 确认没有提交构建产物、日志、本机设置或未打码的截图。

```powershell
cargo fmt --manifest-path app/src-tauri/Cargo.toml --all -- --check
cargo clippy --manifest-path app/src-tauri/Cargo.toml --all-targets -- -D warnings
cargo test --manifest-path app/src-tauri/Cargo.toml
cd app
npm run build
```

## 发布

由维护者操作，普通 PR 不需要改动版本号。

1. 同步更新 `app/package.json`、`app/src-tauri/tauri.conf.json` 的 `version` 与 `CHANGELOG.md`。
2. 撰写 `.github/release-notes/v<version>.md`：亮点、下载与校验、升级说明、已知限制、完整变更日志链接。发布工作流会检查该文件，缺失会直接失败，避免发布只有一行自动链接的空日志。
3. 提交后打标签并推送：

```powershell
git tag v0.1.0
git push origin v0.1.0
```

4. `.github/workflows/release.yml` 在 Windows runner 上构建，先校验标签与版本号一致、发布日志齐全，再以该日志作为 Release 正文发布安装包、免安装包与 `SHA256SUMS.txt`。
5. 本地演练可执行 `powershell -ExecutionPolicy Bypass -File tools\package_release.ps1`，产物在 `app/src-tauri/target/release/release-assets/`。

发布构建必须使用 `npm run tauri build`；直接 `cargo build --release` 不会启用 `tauri/custom-protocol`。
