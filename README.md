# FilmCraft for HarmonyOS

把 [FilmCraft](https://github.com/storytold/filmcraft)（纯 Rust + egui 的视频剪辑器）移植到 **HarmonyOS**，产出可在真机安装运行的 DevEco Studio 工程与签名 `.hap`。

- **移植对象**：`storytold/filmcraft` workspace `0.17.0`，上游源码以 path 依赖方式引入，**不 patch 上游**
- **应用标识**：显示名 `FilmCraft`；`versionName` `0.17.0`、`versionCode` `1700`；`bundleName` `io.github.shaliuxin.filmcraft.hmos`
- **目标平台**：HarmonyOS 6 / API 26 / aarch64（真机 HPR-W72，2in1 PC，设备 `46G0225429001073`）
- **交付包**：`entry/build/default/outputs/default/entry-default-signed.hap`（本轮 116,317,280 字节）

![FilmCraft 编辑器真机截图](./docs/filmcraft-editor.jpeg)

---

## 架构（三层）

```
ArkTS (UIAbility + XComponent, libraryname:'filmcraft')
   │  N-API (libfilmcraft.so: init/input/reply/open/drop/attach/resize/detach/frame/active/close/command)
   ▼
C++ N-API 桥 (entry/src/main/cpp/bridge.cpp, napi module "filmcraft")
   │  链接 libfilmcraft_hmos.a
   ▼
Rust 适配层 (native/rust, staticlib "filmcraft_hmos")
   │  path 依赖上游 crates
   ▼
FilmcraftApp (crates/ui-egui) + filmcraft_engine::Session
```

渲染走 **wgpu GLES 后端直绘鸿蒙 `NativeWindow`**，不经 eframe / winit 事件循环。宿主每帧构造 `egui::RawInput`、驱动 `app.logic()` + `app.ui()`，并把 `platform_output` 里的 IME、URL、Viewport 命令翻译成 ArkTS 能力。

界面中文来自上游源码树内嵌的 `crates/ui-egui/src/i18n/zh-cn.tsv`（`ui-egui/build.rs` 读取），不依赖 `CRAFT_I18N_DIR`。

---

## 功能与状态

如实区分「真机已验证」与「本轮未覆盖」。

| 能力 | 状态 | 说明 |
| --- | --- | --- |
| aarch64 Rust 静态库 → CMake 链接 → HAP 打包签名 | ✅ 已验证 | `build_native.sh` + `hvigor assembleHap` BUILD SUCCESSFUL，`SignHap` 通过 |
| 真机安装 / 启动 / 持续渲染 | ✅ 已验证 | `hdc install -r` → `aa start`；GPU 合成器持续出帧，进程稳定无 crash |
| 全中文界面 | ✅ 已验证 | 菜单（导入/编辑/导出/文件/序列/标记/图形与字幕/视图/窗口/帮助）、监视器、面板、时间轴、状态栏，无方块乱码 |
| 中文输入法（IME） | ✅ 已验证 | 透明锚点 `requestFocus` → `attach(true, textConfig)` → attach 成功后绑定 `insertText` 等回调 → `showTextInput`；宿主按 `platform_output.ime` 显隐键盘；无 12800003 / 12800009 / 201 |
| **关闭窗口保存确认** | ✅ 已验证 | 点标题栏 X 弹出「保存项目／关闭前是否保存对"Untitled"的更改？」三按钮（取消／不保存／保存）；「取消」保留窗口与工程；「不保存」正常 `app exit` 退出，无崩溃 |
| 引擎命令通道 | ✅ 已验证 | 22 条命令覆盖项目／选择／剪贴板／标记／播放头／序列／历史／偏好等子系统，17 条成功、5 条返回预期内的参数校验错误，零崩溃 |
| 菜单下拉 / 面板标签交互 | ✅ 已验证 | 9 个顶部菜单全部展开（中文、禁用置灰、快捷键、子菜单箭头正常），9 个面板标签切换生效，全程零崩溃 |
| 保存按钮 → picker → 写盘 | ❌ 已知缺陷 | 关闭确认里点「保存」后对话框不消失、系统 picker 不弹、worker 阻塞。根因：Rust `hooks()` 发出的 `saveproject` 等 10 个 kind 与 `Index.ets` 接收端（`open`／`file`／`save`／`hostsave`）不匹配，`bridge::request` 永不回包；`host_save`／`download` 回调也未接 |
| 导出 / 打开真实媒体文件 | ❌ 已知缺陷（同上） | 所有文件对话框类请求均因宿主缝 kind 不匹配被丢弃，需统一 kinds 并补写盘回调后重编 native |
| x86_64 产物 | ⚠️ 本轮未验证 | 本轮只编译 aarch64（真机所需 ABI） |

![关闭窗口时的保存确认对话框](./docs/close-confirm.jpeg)

---

## 关闭确认的实现

系统关窗会直接销毁窗口、绕过宿主，因此三层协作拦截：

1. **ArkTS**：`EntryAbility.onWindowStageCreate` 注册 `mainWindow.on('windowWillClose', async (): Promise<boolean> => …)`，回调返回 `true` 拦截并调用 `native.close()`。SDK 26 要求 `Promise<boolean>`；写成同步 `(): boolean` 会报 `10505001 No overload matches this call`。
2. **C++ / Rust**：`wc_close()` → `Message::Close`；下一帧向 egui 注入 `ViewportEvent::Close`。
3. **上游**：`FilmcraftApp::logic_control` 检测 `viewport().close_requested()`，脏工程时 `panels::clip_dialogs::intercept_quit` 弹出保存确认并发出 `ViewportCommand::CancelClose`；用户选 Save / Don't Save 后 `leave()` 置 `quit_confirmed` 并发出 `ViewportCommand::Close`。
4. **宿主 `close_action()`** 只看这两个命令：`CancelClose` → 暂缓关闭（继续绘制对话框）；`Close` → 下一帧重新注入关闭事件再验；无 `CancelClose` → `notify("close")` 通知 ArkTS `terminateSelf()`。

这套机制与姊妹工程 `deckcraft-hmos` 的关窗守卫一致；它依赖上游的 `ViewportCommand`，而不是 `WordApp` 那种 `app.close_requested()` 私有方法。

---

## 构建与安装

需要 DevEco Studio（含 OpenHarmony native SDK）、Rust `aarch64-unknown-linux-ohos` target，以及连接好的真机。

```bash
# 1) 还原本机真实签名配置（公开仓库里的是占位符版）
cp signing/build-profile.local.json5 build-profile.json5

# 2) 编译 Rust 适配层（只编真机所需 ABI）
export FC_NATIVE_SDK="/c/Program Files/Huawei/DevEco Studio/sdk/default/openharmony/native"
export FC_NATIVE_TARGETS="aarch64-unknown-linux-ohos"
bash scripts/build_native.sh

# 3) 打包签名 HAP
node "<DevEco>/tools/hvigor/bin/hvigorw.js" assembleHap --mode module \
  -p product=default -p buildMode=debug --no-daemon

# 4) 安装并启动
hdc install -r entry/build/default/outputs/default/entry-default-signed.hap
hdc shell aa start -a EntryAbility -b io.github.shaliuxin.filmcraft.hmos
```

`build_native.sh` 在 Windows 上会用无空格 junction `C:\ohosnative` 绕开 `C:\Program Files` 的路径空格问题。

---

## 签名说明

本仓库的 `build-profile.json5` 是**占位符版**（`signingConfigs: []`），真实证书路径与口令 hex 不入库。本机真实配置保存在 `signing/build-profile.local.json5`（该目录已被 `.gitignore` 屏蔽）。

首次使用请在 DevEco Studio 里用本项目**专属** `bundleName` 做一次自动签名，生成绑定自己设备 UDID 的调试 Profile。**切勿复用其它 craft 工程的 bundleName 或签名 Profile**：Profile 与 bundleName + 设备 UDID 绑定，复用会与该包名已装的旧 App 撞车（真机报 `9568263 版本降级`），且包名上架后永久不可改。

---

## 许可与上游

移植工程沿用上游的 **Apache-2.0 OR MIT** 双许可，许可证全文见 [`LICENSE-APACHE`](./LICENSE-APACHE) 与 [`LICENSE-MIT`](./LICENSE-MIT)。

上游项目：ArtCraft Team and the FilmCraft contributors, *filmcraft*, GitHub, <https://github.com/storytold/filmcraft>, workspace version 0.17.0.
