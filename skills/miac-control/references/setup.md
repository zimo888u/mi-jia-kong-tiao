# 安装与首次登录

1. 在 Windows x64 电脑上解压压缩包，将整个 `miac-control` 文件夹放进 Codex 的 `skills` 目录。默认位置是 `%USERPROFILE%\.codex\skills\miac-control`；若设置了 `CODEX_HOME`，则放在 `%CODEX_HOME%\skills\miac-control`。确认该目录下能看到 `SKILL.md` 和 `scripts` 文件夹。
2. 新开一个 Codex 对话，确认可以调用 `$miac-control`。
3. 首次使用时运行 `scripts\miac-app.exe --login`（也可以双击桌面程序后点击“扫码登录米家账号”）。用米家 App 扫码，在桌面窗口中选择要控制的空调。登录凭据只保存在这台电脑当前 Windows 用户的 `%APPDATA%\米家空调` 中。
4. 在 Codex 中说“用 miac-control 查询室温”，确认能读到设备数据。之后可以说“打开空调”“设置 26 度”等。

压缩包不含任何账号、token 或设备凭据。换电脑或换 Windows 用户后，需要在那边重新扫码。空调型号支持情况与随包桌面端的 `miac-core` 版本一致；未知型号需要联网读取公开 MIoT 规格。
