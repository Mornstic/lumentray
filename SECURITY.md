# 安全策略

## 支持版本

| 版本 | 支持安全更新 |
| --- | --- |
| 0.1.x | 是 |

## 报告漏洞

请使用 GitHub Security 页面的 **Report a vulnerability** 私下报告：

<https://github.com/Mornstic/lumentray/security/advisories/new>

不要在公开 Issue、讨论或截图中披露未修复的漏洞。

报告中请尽量包含：

- 受影响的版本和 Windows 版本
- 复现步骤、影响范围和建议的修复方向
- 是否与显示器控制、注册表、安装包或 WebView 相关

请使用占位符代替真实设备路径。**不要提交完整的显示器设备路径、EDID 序列号或未打码的桌面截图。**

## 安全边界

- LumenTray 不访问网络，没有账号、云端、遥测或自动更新。
- 亮度控制通过本机 DDC/CI 完成；程序不需要管理员权限，也不安装驱动或服务。
- 设置保存在 `%APPDATA%\LumenTray\settings.json`，日志保存在 `%APPDATA%\LumenTray\logs\`。
- 开机启动通过当前用户的 `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` 写入，可随时在设置中关闭。
- 安装包当前未做代码签名，Windows SmartScreen 可能提示「未知发布者」。请从本仓库 Releases 下载，并用 `SHA256SUMS.txt` 校验。

## 处理流程

维护者会尽快确认报告，评估影响并准备修复；在修复发布前，请给维护者合理的处理时间。
修复完成后，会在 Release Notes 或 GitHub Security Advisory 中说明影响范围。
