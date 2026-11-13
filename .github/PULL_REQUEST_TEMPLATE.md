## 改动内容

<!-- 说明改了什么，以及为什么需要这个改动。 -->

## 验证

- [ ] `cargo fmt --manifest-path app/src-tauri/Cargo.toml --all -- --check`
- [ ] `cargo clippy --manifest-path app/src-tauri/Cargo.toml --all-targets -- -D warnings`
- [ ] `cargo test --manifest-path app/src-tauri/Cargo.toml`
- [ ] `cd app && npm run build`
- [ ] 如有 UI 改动，已在本机实际启动并检查
- [ ] 如有显示器控制改动，已用真实显示器验证

## 安全确认

- [ ] 没有提交构建产物、日志、本机设置或未打码的截图
- [ ] 没有提交完整的显示器设备路径或 EDID 序列号

## 关联 Issue

<!-- 例如：Closes #12 -->
