# gpui-global-hotkey

为 [GPUI](https://gpui.rs) 应用提供**系统级全局快捷键**：即使应用处于后台、没有焦点窗口，
组合键也能触发。OS 按键事件被桥接回 GPUI 主线程，最终以 GPUI [`Action`] 或普通回调的形式派发。

本仓库是**独立子仓库**（与 `gpui-autostart` / `gpui-tray` 同模式），通过 git submodule
挂载到 [`gpui-kit`](https://github.com/kkkqkx123/gpui-kit) 的 `crates/global-hotkey`。
它**不是** `gpui-kit` 工作区的成员。

## 工作原理

```
  GPUI 主线程                                 OS 回调线程（非 GPUI）
  ──────────────                             ──────────────────────
  GlobalHotkey::new(cx)
    • 创建 manager（global-hotkey）
    • flume 通道 + bridge task                Windows  WM_HOTKEY
    • 注册表 id → Registration                macOS    Carbon 事件
  register / on_hotkey / unregister           Linux    x11rb XGrabKey
  cx.spawn + AsyncApp::update 派发
    → App::dispatch_action / 回调
```

原生平台线程只负责 `send` 事件；所有 GPUI 状态访问（`dispatch_action`、回调）都发生在主线程的
`flume` 桥接中，与 `gpui-tray` 完全一致。OS 抓键由跨平台
[`global-hotkey`](https://github.com/tauri-apps/global-hotkey) crate 完成，因此本 crate 开箱即支持
Windows / macOS / Linux(X11)（并带 no-op 兜底：当平台无法提供全局快捷键时 `is_available()` 为
`false`，注册退化为空操作，应用照常启动）。

## GPUI flavor 选择

三选一（关闭 default features 可选其他）：

| feature     | GPUI 来源                        |
| ----------- | --------------------------------- |
| `gpui-kit`  | `gpui-pre` 0.3.9（GPUI Kit）— 默认 |
| `gpui`      | 上游 Zed GPUI（git rev）          |
| `gpui-ce`   | GPUI Community Edition 0.3.0      |

所选 flavor 必须与宿主应用编译所用的 GPUI 一致。

## 用法

```rust,ignore
use gpui_global_hotkey::{GlobalHotkey, HotKey};
use gpui_kit::*;

actions!(demo, [ShowWindow, Quit]);

fn main() {
    gpui_kit::application().run(|cx| {
        gpui_kit::init(cx);
        let hk = GlobalHotkey::new(cx).expect("global hotkey");

        // Action 形式：无焦点时也能抵达 App::on_action。
        let _r1 = hk.on_hotkey(HotKey::from_str("ctrl+alt+g").unwrap(), ShowWindow);

        // 回调形式，可感知 Pressed / Released。
        let _r2 = hk.on_hotkey_with(HotKey::from_str("ctrl+alt+q").unwrap(), |cx, ev| {
            if ev.state == gpui_global_hotkey::HotkeyState::Pressed { cx.quit(); }
        });
    });
}
```

`HotKey` 可解析快捷键字符串（`"ctrl+shift+space"`、`"cmdorctrl+g"`），也可由 GPUI `Keystroke`
转换，并支持 `Display`。`hotkey!` 宏可按标志构造：`hotkey!(control | alt => "g")`。

## 平台说明

- **Linux**：仅 X11。无 XWayland 的 Wayland 会话下 `is_available()` 为 `false`，请回退为窗口内快捷键。
  X11 仅产生 `Pressed`。
- **macOS**：Carbon `RegisterEventHotKey` —— 无需辅助功能权限。
- **Windows**：在主线程消息循环上 `RegisterHotKey`。
- 每个进程预期只创建一个 `GlobalHotkey` 实例（底层事件处理器为进程级全局）。

## 许可证

Apache-2.0
