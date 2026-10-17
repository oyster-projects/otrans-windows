# OTrans PC Rust

OTrans 的轻量 Windows 客户端，使用 Rust + Tauri 2。它与现有飞牛服务端、鸿蒙客户端使用同一套 API；原 Electron 客户端可以继续保留，二者的数据目录和设备令牌互不覆盖。

## 当前功能

- 内网地址优先、外网地址自动回退，并校验 `serverId`
- 配对令牌保存在 Windows 凭据管理器，不写入状态 JSON
- WebSocket 实时通知、断线自动重连，并每 60 秒做一次安全同步
- 接收文本、图片和文件；同步服务端的全局删除事件
- 拖入多个文件，和文字一起组成一条中转记录
- 点击按钮读取一次剪切板，支持文字、图片以及资源管理器复制的文件
- 托盘运行、开机启动选项、Windows 通知和手动同步

实验性局域网文件直传和 Outbox 文件夹监听仍由 Electron 版提供，后续再迁移到 Rust 版。

## 开发

需要 Rust stable、Node.js、WebView2 和 Windows C++ 构建工具：

```powershell
npm install
npm run tauri dev
```

本机没有完整 Visual Studio Build Tools 时，可以运行 `scripts/build-local.ps1`。该脚本使用仓库下 `.toolchain` 中的便携 LLVM 和 cargo-xwin，生成的 EXE 位于 `dist`。

