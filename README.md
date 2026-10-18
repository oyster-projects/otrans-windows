# OTrans PC Rust

OTrans 的轻量 Windows 客户端，使用 Rust + Tauri 2。它与飞牛服务端、鸿蒙客户端和 Android 客户端使用同一套 API。Windows 主线只维护本项目，Electron 版已经冻结。

## 当前功能

- 内网地址优先、外网地址自动回退，并校验 `serverId`
- 配对令牌保存在 Windows 凭据管理器，不写入状态 JSON
- WebSocket 实时通知、断线自动重连，并每 60 秒做一次安全同步
- 接收文本、图片和文件；同步服务端的全局删除事件
- 拖入多个文件，和文字一起组成一条中转记录
- 点击按钮读取一次剪切板，支持文字、图片以及资源管理器复制的文件
- 托盘运行、开机启动选项、Windows 通知和手动同步
- 连接和接收目录选项集中在“设置”弹窗；接收目录使用简短的时间与内容名称
- 状态文件采用临时文件 + 备份替换，损坏时会自动读取上一份有效状态

当前 Rust 版尚未实现实验性局域网设备直传和 Outbox 文件夹监听，因此界面不会展示这两个无效选项；所有发送均通过拖放、文件选择或单次读取剪贴板完成。NAS 仍是权威中转与删除同步节点。

## 开发

需要 Rust stable、Node.js、WebView2 和 Windows C++ 构建工具：

```powershell
npm install
npm run tauri dev
```

本机没有完整 Visual Studio Build Tools 时，可以运行 `scripts/build-local.ps1`。该脚本使用仓库下 `.toolchain` 中的便携 LLVM 和 cargo-xwin，生成的 EXE 位于 `dist`。

