//! 鸿蒙专属 FFI 边界。视频编辑器与全部解码/合成逻辑保持在安全 Rust 上游（filmcraft 各 crate）。
//!
//! 与 WordCraft 同构：`FilmcraftApp` 已把宿主缝直接暴露为 `new(Session)` / `logic` / `raw_input_hook`
//! / `ui` / `on_exit`（`impl eframe::App`）与 `set_wgpu(RenderState)`。与 WordCraft 的关键差异：
//!   1) **FilmCraft 走 eframe::App trait**（WordCraft 的 WordApp 是固有方法），适配层需 `use eframe::App`
//!      把 trait 方法带进作用域——这正是本地 `eframe` shim 的用途（零 winit）。
//!   2) **GPU 合成器**：`app.set_wgpu(rs)` 把 egui/wgpu 的 RenderState 交给上游 `GpuCompositor`，
//!      视频帧在 GPU 合成后上传为 egui 纹理，egui 再把它画进 UI——渲染管线沿用 WordCraft 的纯
//!      egui+wgpu GL 路径即可。
//!   3) 文件/服务用 `HostHooks`（WordCraft 用 `Services`）；命令通道走 `app.session.execute`。
#![allow(clippy::needless_lifetimes)]
mod appearance;
mod bridge;
mod display_color;
mod file_open;
mod form_factor;
mod input;
mod locale;
mod render;

use eframe::App as _; // raw_input_hook / logic / ui / on_exit 都是 trait 方法
use filmcraft_ui_egui::{FilmcraftApp, HostHooks, install_ime_reset_callbacks};
use serde::Deserialize;
use std::{
    sync::{
        OnceLock,
        mpsc,
        atomic::{AtomicI8, Ordering},
    },
    time::{Duration, Instant},
};

/// 把 `log` crate 的记录转到 hilog：wgpu-hal 的 EGL 诊断（平台选择 / surface 创建 / swap 失败）
/// 默认没有 logger 就会被丢掉，设备上完全看不见。
#[cfg(target_env = "ohos")]
mod glog {
    use std::ffi::{CString, c_char};

    unsafe extern "C" {
        fn wc_hilog(level: i32, message: *const c_char);
    }

    struct Hilog;

    impl log::Log for Hilog {
        fn enabled(&self, _: &log::Metadata<'_>) -> bool {
            true
        }

        fn log(&self, record: &log::Record<'_>) {
            let level = match record.level() {
                log::Level::Error => 2,
                log::Level::Warn => 1,
                _ => 0,
            };
            let line = format!("[{}] {}: {}", record.level(), record.target(), record.args());
            if let Ok(text) = CString::new(line) {
                // SAFETY: 有效的 NUL 结尾 C 字符串；wc_hilog 只读该指针。
                unsafe { wc_hilog(level, text.as_ptr()) };
            }
        }

        fn flush(&self) {}
    }

    static LOGGER: Hilog = Hilog;

    pub fn install() {
        if log::set_logger(&LOGGER).is_ok() {
            log::set_max_level(log::LevelFilter::Info);
        }
    }
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct Config {
    files: String,
    #[serde(default)]
    documents: String,
    #[serde(default = "default_scale")]
    scale: f32,
    #[serde(default)]
    language: String,
    /// 系统 locale（`zh-Hans-CN` 这类 BCP-47 标签），供 `system_languages` hook 供「跟随系统」解析。
    #[serde(default, rename = "systemLocale")]
    system_locale: String,
    #[serde(default, rename = "displayP3")]
    display_p3: bool,
    #[serde(default, rename = "systemTheme")]
    system_theme: String,
    #[serde(default)]
    fullscreen: bool,
    #[serde(default, rename = "deviceType")]
    device_type: String,
    #[serde(default, rename = "widthVp")]
    width_vp: f32,
}
fn default_scale() -> f32 {
    1.
}

#[allow(dead_code)]
enum Message {
    Attach(usize, u32, u32),
    Resize(u32, u32),
    Detach,
    Frame,
    Active(bool),
    Input(input::Input),
    /// 媒体/工程文件：`(路径名, 字节)`。文件已由 ArkTS 落到沙箱 `.inbox`，这里只把沙箱路径交给上游。
    Open(String, Vec<u8>),
    Drop(String, Vec<u8>, f32, f32),
    DropFile(String, std::fs::File, f32, f32),
    Close,
    Wake(Duration),
    /// 命令通道（自动化/控制端）：转成 `app.session.execute(id, params)`。
    Command(String, serde_json::Value),
}

static SEND: OnceLock<mpsc::Sender<Message>> = OnceLock::new();
fn send(message: Message) {
    if let Some(tx) = SEND.get() {
        let _ = tx.send(message);
    }
}

/// 把 ArkTS 上报的系统主题透传给 appearance 模块。
static SYSTEM_THEME_TOKEN: AtomicI8 = AtomicI8::new(0);

/// # Safety
/// `ptr`/`len` 必须在调用期间指向可读内存；调用方保证。
unsafe fn bytes<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if len > isize::MAX as usize || (ptr.is_null() && len != 0) {
        return None;
    }
    if len == 0 {
        return Some(&[]);
    }
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

fn copy_bytes(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(data.len())
        .map_err(|e| format!("Not enough memory to open file: {e}"))?;
    bytes.extend_from_slice(data);
    Ok(bytes)
}

fn write_temp_file(dir: &str, name: &str, bytes: &[u8]) -> std::io::Result<String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let safe: String = name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '_' })
        .collect();
    let path = std::path::Path::new(dir).join(format!("{stamp}_{safe}"));
    std::fs::write(&path, bytes)?;
    Ok(path.to_string_lossy().into_owned())
}

// ===================== FFI 导出（C++ 边界调用） =====================

/// # Safety
/// 指针/长度对必须指向可读内存直到本调用返回；notify 回调在进程生命周期内保持有效。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wc_init(ptr: *const u8, len: usize, notify: bridge::Notify) -> bool {
    let Some(data) = (unsafe { bytes(ptr, len) }) else {
        return false;
    };
    let Ok(config) = serde_json::from_slice::<Config>(data) else {
        return false;
    };
    if !config.scale.is_finite() || config.scale <= 0. || config.scale > 8. {
        return false;
    }
    bridge::init(notify);
    #[cfg(target_env = "ohos")]
    glog::install();
    locale::install(&config.files, &config.system_locale, &config.language);
    if !config.system_theme.is_empty() {
        let dark = config.system_theme.eq_ignore_ascii_case("dark");
        SYSTEM_THEME_TOKEN.store(if dark { 1 } else { -1 }, Ordering::Release);
        appearance::set_dark(dark);
    }
    let (tx, rx) = mpsc::channel();
    if SEND.set(tx).is_err() {
        return true;
    }
    std::thread::Builder::new()
        .name("filmcraft-native".into())
        .spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker(config, rx)))
                .is_err()
            {
                bridge::notify(0, "error", "Native host initialization failed", &[]);
            }
        })
        .is_ok()
}

/// # Safety
/// 指针/长度对必须可读直到返回。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wc_input(ptr: *const u8, len: usize) {
    let Some(data) = (unsafe { bytes(ptr, len) }) else { return; };
    let Ok(value) = serde_json::from_slice(data) else { return; };
    send(Message::Input(value));
}

/// # Safety
/// 同 `wc_init`。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wc_reply(
    id: u64,
    ok: bool,
    text: *const u8,
    text_len: usize,
    data: *const u8,
    len: usize,
) {
    let Some(text) = (unsafe { bytes(text, text_len) }) else {
        bridge::reply(id, Err("Invalid reply text buffer".into()));
        return;
    };
    let Some(data) = (unsafe { bytes(data, len) }) else {
        bridge::reply(id, Err("Invalid reply data buffer".into()));
        return;
    };
    let text = String::from_utf8_lossy(text).into_owned();
    bridge::reply(
        id,
        if ok {
            copy_bytes(data).map(|data| (text, data))
        } else {
            Err(text)
        },
    );
}

/// # Safety
/// 指针/长度对必须可读直到返回。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wc_open(text: *const u8, n: usize, data: *const u8, len: usize) {
    if let (Some(text), Some(data)) = (unsafe { bytes(text, n) }, unsafe { bytes(data, len) }) {
        match copy_bytes(data) {
            Ok(data) => send(Message::Open(String::from_utf8_lossy(text).into_owned(), data)),
            Err(error) => bridge::notify(0, "error", &error, &[]),
        }
    }
}

/// # Safety
/// 指针/长度对必须可读直到返回。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wc_drop(
    text: *const u8,
    n: usize,
    data: *const u8,
    len: usize,
    x: f32,
    y: f32,
) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }
    if let (Some(text), Some(data)) = (unsafe { bytes(text, n) }, unsafe { bytes(data, len) }) {
        send(Message::Drop(
            String::from_utf8_lossy(text).into_owned(),
            data.to_vec(),
            x,
            y,
        ));
    }
}

/// # Safety
/// `text`/`n` 必须可读且 `fd` 打开直到返回。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wc_drop_file(text: *const u8, n: usize, fd: i32, x: f32, y: f32) {
    if !x.is_finite() || !y.is_finite() {
        return;
    }
    let Some(text) = (unsafe { bytes(text, n) }) else {
        return;
    };
    match unsafe { file_open::duplicate(fd) } {
        Ok(file) => send(Message::DropFile(
            String::from_utf8_lossy(text).into_owned(),
            file,
            x,
            y,
        )),
        Err(error) => bridge::notify(0, "error", &error, &[]),
    }
}

/// # Safety
/// `ptr`/`len` 为自由文本命令标识。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wc_command(ptr: *const u8, len: usize) {
    let Some(data) = (unsafe { bytes(ptr, len) }) else { return; };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(data) else { return; };
    let Some(id) = v.get("id").and_then(|v| v.as_str()) else { return; };
    send(Message::Command(
        id.into(),
        v.get("params").cloned().unwrap_or_default(),
    ));
}

/// # Safety
/// `window` 必须是存活的 OHNativeWindow，且已获取一个原生引用；该引用所有权转移给 worker 及其 wgpu surface。
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wc_attach(window: usize, w: u32, h: u32) {
    if window != 0 {
        send(Message::Attach(window, w, h));
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn wc_resize(w: u32, h: u32) {
    send(Message::Resize(w, h));
}

#[unsafe(no_mangle)]
pub extern "C" fn wc_detach() {
    bridge::cancel();
    send(Message::Detach);
}

#[unsafe(no_mangle)]
pub extern "C" fn wc_frame() {
    send(Message::Frame);
}

#[unsafe(no_mangle)]
pub extern "C" fn wc_active(active: bool) {
    send(Message::Active(active));
}

#[unsafe(no_mangle)]
pub extern "C" fn wc_close() {
    send(Message::Close);
}

// ===================== 仅 OHOS 宿主逻辑 =====================

#[cfg(target_env = "ohos")]
unsafe extern "C" {
    fn wc_request_vsync();
}

#[cfg(target_env = "ohos")]
fn worker(config: Config, rx: mpsc::Receiver<Message>) {
    bridge::notify(0, "stage", "Rust worker entered", &[]);
    let ctx = egui::Context::default();
    install_ime_reset_callbacks(&ctx);
    let form = form_factor::Form::new(&config.device_type, config.width_vp);
    bridge::notify(
        0,
        "diagnostic",
        &format!("form factor {:?}, {:.0} vp, {:?}", form.device_type, form.width_vp, form.width_class),
        &[],
    );

    // FilmCraft 的 Session 自带 `default()`（首开空工程）；文件打开走 `session.execute("file.open",…)`。
    // 鸿蒙沙箱外的路径 std::fs 写不了（DocumentViewPicker 只授予 URI 级权限）。
    // 注入 OHOS Services：工程保存 / 导出的字节交给 ArkTS 弹系统保存选择器落盘；
    // 沙箱内部文件（autosave / 预设 / 媒体缓存）仍走原生 std::fs，不打扰用户。
    let session = filmcraft_engine::Session::new(std::sync::Arc::new(OhosServices {
        files: config.files.clone(),
    }));
    let mut app = FilmcraftApp::new(session);
    app.integrated_titlebar = false; // 鸿蒙走 ArkTS 原生标题栏，不用上游自绘标题栏
    app.hooks = hooks(&config.files, &config.system_locale);
    bridge::notify(0, "stage", "editor ready", &[]);

    let mut render = match render::Render::new(config.display_p3) {
        Ok(render) => render,
        Err(error) => {
            bridge::notify(0, "error", &error, &[]);
            return;
        }
    };
    let mut active = true;
    let mut visible = false;
    let mut deadline: Option<Instant> = None;
    let started = Instant::now();
    let mut events: Vec<egui::Event> = Vec::new();
    let mut input_state = input::State::default();
    let mut close = false;
    let mut exiting = false;
    // 软键盘是否已由宿主弹起：避免每帧重复通知，仅在 egui 文本焦点变化时切换。
    let mut ime_shown = false;
    // 每帧复用的最小 eframe::Frame（shim）；logic/ui 只把它当句柄 + set_ui_language 落点。
    let mut frame = eframe::Frame::new();

    ctx.set_request_repaint_callback(|info| send(Message::Wake(info.delay)));
    bridge::notify(0, "ready", "Native host initialized", &[]);

    loop {
        if exiting {
            break;
        }
        let wait = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        let message = match wait {
            Some(wait) => match rx.recv_timeout(wait) {
                Ok(m) => m,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    deadline = None;
                    if active && visible && render.attached() {
                        unsafe { wc_request_vsync() };
                    }
                    continue;
                }
                Err(_) => break,
            },
            None => match rx.recv() {
                Ok(m) => m,
                Err(_) => break,
            },
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<(), String> {
                match message {
                    Message::Attach(ptr, w, h) => {
                        visible = w > 0 && h > 0;
                        render.bind(render::Window(ptr), w, h)?;
                        // GPU 合成器绑定：把已绑定的 surface/device 的 RenderState 交给上游。
                        if let Some(rs) = render.egui_render_state() {
                            app.set_wgpu(rs);
                        }
                        bridge::resume();
                        deadline = Some(Instant::now());
                        // 重连后重置软键盘状态，下一帧按实际焦点重新评估显隐。
                        ime_shown = false;
                        bridge::notify(0, "ready", "Native egui / wgpu GLES + compositor", &[]);
                    }
                    Message::Resize(w, h) => {
                        visible = w > 0 && h > 0;
                        render.resize(w, h)?;
                        deadline = Some(Instant::now());
                    }
                    Message::Detach => {
                        visible = false;
                        deadline = None;
                        render.detach();
                        input_state.cancel(&mut events);
                        // 表面销毁时收起软键盘并重置状态，避免重连后状态错乱。
                        ime_shown = false;
                        bridge::notify(0, "ime", "hide", &[]);
                    }
                    Message::Active(value) => {
                        active = value;
                        if active {
                            bridge::resume();
                            deadline = Some(Instant::now());
                        } else {
                            deadline = None;
                            input_state.cancel(&mut events);
                        }
                    }
                    Message::Input(value) => {
                        if value.kind == "system_theme" {
                            let dark = value.text.eq_ignore_ascii_case("dark");
                            appearance::set_dark(dark);
                            deadline = Some(Instant::now());
                            return Ok(());
                        }
                        input_state.push(value, config.scale, &mut events);
                        deadline = Some(Instant::now());
                    }
                    Message::Open(name, data) => {
                        if let Ok(path) = write_temp_file(&config.files, &name, &data) {
                            open_into_app(&mut app, &path);
                        }
                        deadline = Some(Instant::now());
                    }
                    Message::Drop(name, data, _x, _y) => {
                        if let Ok(path) = write_temp_file(&config.files, &name, &data) {
                            open_into_app(&mut app, &path);
                        }
                        deadline = Some(Instant::now());
                    }
                    Message::DropFile(name, file, _x, _y) => {
                        match file_open::read(file) {
                            Ok(data) => {
                                if let Ok(path) = write_temp_file(&config.files, &name, &data) {
                                    open_into_app(&mut app, &path);
                                }
                            }
                            Err(e) => bridge::notify(0, "error", &e, &[]),
                        }
                        deadline = Some(Instant::now());
                    }
                    Message::Close => {
                        bridge::resume();
                        close = true;
                        deadline = Some(Instant::now());
                    }
                    Message::Wake(delay) => {
                        if active && visible {
                            if let Some(next) = Instant::now().checked_add(delay) {
                                deadline = Some(deadline.map_or(next, |d| d.min(next)));
                            }
                        }
                    }
                    Message::Command(id, params) => {
                        match app.session.execute(&id, params) {
                            Ok(v) => {
                                let preview = v.to_string();
                                let preview = preview.chars().take(120).collect::<String>();
                                bridge::notify(0, "command", &format!("{id}: {preview}"), &[]);
                            }
                            Err(e) => bridge::notify(0, "command_err", &format!("{id}: {e}"), &[]),
                        }
                        deadline = Some(Instant::now());
                    }
                    Message::Frame => {
                        if !active || !visible {
                            return Ok(());
                        }
                        if !render.attached() {
                            return Ok(());
                        }
                        let size = egui::vec2(
                            render.width as f32 / config.scale,
                            render.height as f32 / config.scale,
                        );
                        let mut raw = egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, size)),
                            time: Some(started.elapsed().as_secs_f64()),
                            events: std::mem::take(&mut events),
                            system_theme: appearance::current(),
                            ..Default::default()
                        };
                        let viewport = raw.viewports.entry(egui::ViewportId::ROOT).or_default();
                        viewport.native_pixels_per_point = Some(config.scale);
                        viewport.inner_rect = raw.screen_rect;
                        viewport.focused = Some(true);
                        let validating_close = close;
                        if close {
                            viewport.events.push(egui::ViewportEvent::Close);
                            close = false;
                        }
                        // 宿主缝：raw_input_hook（trait）→ logic/ui（trait，经最小 Frame）。
                        app.raw_input_hook(&ctx, &mut raw);
                        let output = editor_frame(&ctx, &mut app, raw, &mut frame);
                        for command in &output.platform_output.commands {
                            if let egui::OutputCommand::OpenUrl(url) = command {
                                bridge::notify(0, "url", &url.url, &[]);
                            }
                        }
                        // 软键盘随文本焦点自动弹起/收起：egui 在有文本字段获得焦点时把
                        // `platform_output.ime` 置为 Some，宿主据此显隐系统输入法键盘。
                        let ime_wanted = output.platform_output.ime.is_some();
                        if ime_wanted != ime_shown {
                            ime_shown = ime_wanted;
                            bridge::notify(0, "ime", if ime_wanted { "show" } else { "hide" }, &[]);
                        }
                        let mut commands = Vec::new();
                        let mut repaint_delay: Option<Duration> = None;
                        if let Some(v) = output.viewport_output.get(&egui::ViewportId::ROOT) {
                            commands.extend(v.commands.clone());
                            repaint_delay = Some(v.repaint_delay);
                        }
                        // 关窗门控（与 DeckCraft 同一套）：本帧向 egui 推了 ViewportEvent::Close
                        // 后，若上游报 CancelClose 说明有未保存工程、已弹出「保存更改？」对话框，
                        // 必须继续正常绘制而不能退出；用户选 Save / Don't Save 后上游 leave()
                        // 置 quit_confirmed 并发 ViewportCommand::Close，下一帧重新注入 Close 事件
                        // 再验一次，此时无 CancelClose 才真正放行。
                        match close_action(validating_close, &commands) {
                            CloseAction::Confirm => {
                                // 上游确认关闭：flush session 并通知宿主 terminateSelf()。
                                app.on_exit();
                                bridge::notify(0, "close", "", &[]);
                                exiting = true;
                                return Ok(());
                            }
                            CloseAction::Request => {
                                close = true;
                                deadline = Some(Instant::now());
                            }
                            CloseAction::Cancel | CloseAction::None => {}
                        }
                        let painted = render.paint(&ctx, output)?;
                        if first_frame_marker(painted) {
                            bridge::notify(0, "first_frame", "Native UI presented", &[]);
                        }
                        // 按 egui 的 `repaint_delay` 排下一帧：时间线在动时它是 0（连续重绘），
                        // 静止时是一个很大的值或 MAX（等输入/vsync 唤醒），避免空转耗电。
                        if painted && active && visible && render.attached() {
                            deadline = match repaint_delay {
                                Some(delay) if delay.is_zero() => Some(Instant::now()),
                                Some(delay) if delay < Duration::from_secs(3600) => {
                                    Some(Instant::now() + delay)
                                }
                                _ => None,
                            };
                        }
                    }
                }
                Ok(())
            },
        ));
        let _ = result;
    }

    // 正常退出路径（Detach/断连）也要 flush session，确保恢复日志落盘。
    if !exiting {
        let _ = app.on_exit();
    }
}

#[cfg(target_env = "ohos")]
#[allow(dead_code)]
fn first_frame_marker(painted: bool) -> bool {
    use std::sync::atomic::{AtomicBool, Ordering};
    static FIRST: AtomicBool = AtomicBool::new(false);
    if painted && !FIRST.swap(true, Ordering::AcqRel) {
        true
    } else {
        false
    }
}

#[cfg(target_env = "ohos")]
fn editor_frame(
    ctx: &egui::Context,
    app: &mut FilmcraftApp,
    raw: egui::RawInput,
    frame: &mut eframe::Frame,
) -> egui::FullOutput {
    // 与上游 eframe 宿主一致：每帧的逻辑拍（`logic`，含逐帧 tick/时间线推进）先于 UI 拍。
    // `run_ui` 会跑 logic-pass(pass 0) + 完整渲染 pass(pass 1)，这里在 pass 0 跑 logic、各 pass 跑 ui。
    ctx.run_ui(raw, |ui| {
        if ui.ctx().current_pass_index() == 0 {
            app.logic(ctx, frame);
        }
        app.ui(ui, frame);
    })
}

#[cfg(target_env = "ohos")]
#[derive(Debug, PartialEq, Eq)]
enum CloseAction {
    None,
    Request,
    Cancel,
    Confirm,
}

/// FilmCraft 自管未保存工程：`FilmcraftApp::logic_control` 检测到
/// `viewport().close_requested()` 后调 `panels::clip_dialogs::intercept_quit`，脏工程会弹
/// 「保存更改？」对话框并发 `ViewportCommand::CancelClose`；用户选 Save / Don't Save 后
/// `leave()` 置 `quit_confirmed` 并发 `ViewportCommand::Close`。宿主只看这两个命令，
/// 不依赖上游的 `quit_confirmed` 私有状态（与 DeckCraft 的关窗守卫同一套机制）。
#[cfg(target_env = "ohos")]
fn close_action(validating: bool, commands: &[egui::ViewportCommand]) -> CloseAction {
    if commands.contains(&egui::ViewportCommand::CancelClose) {
        CloseAction::Cancel
    } else if validating {
        CloseAction::Confirm
    } else if commands.contains(&egui::ViewportCommand::Close) {
        CloseAction::Request
    } else {
        CloseAction::None
    }
}

/// 鸿蒙文件服务（注入 `filmcraft_engine::Session`）。
///
/// 上游 `write_project` / 导出都经 `Services::write_file(path, bytes)`。鸿蒙的
/// `DocumentViewPicker` 只给 URI 级临时授权，Rust 的 `std::fs` 写不了沙箱外的用户文档，
/// 因此这里按路径分流：
/// - `config.files` 沙箱目录内（autosave、预设、媒体缓存）→ 原生 `FsServices`，静默写入；
/// - 用户文档路径 → `bridge::request("savebytes", …)`，由 ArkTS 弹系统保存选择器写盘。
///
/// `bridge::request` 会阻塞 worker 等 `native.reply`；ArkTS 侧对成功／取消／异常三条路径
/// 都必须回包，否则帧循环会永久卡住。
#[cfg(target_env = "ohos")]
struct OhosServices {
    files: String,
}

#[cfg(target_env = "ohos")]
impl OhosServices {
    fn internal(&self, path: &str) -> bool {
        !self.files.is_empty() && path.starts_with(&self.files)
    }
}

#[cfg(target_env = "ohos")]
impl filmcraft_engine::Services for OhosServices {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        std::fs::read(path)
    }

    fn write_file(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        if self.internal(path) {
            return filmcraft_engine::Services::write_file(&filmcraft_engine::FsServices, path, data);
        }
        bridge::request("savebytes", path, data)
            .map(|_| ())
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))
    }

    fn write_file_new(&self, path: &str, data: &[u8]) -> std::io::Result<()> {
        if self.internal(path) {
            return filmcraft_engine::Services::write_file_new(&filmcraft_engine::FsServices, path, data);
        }
        self.write_file(path, data)
    }

    fn file_size(&self, path: &str) -> std::io::Result<u64> {
        let m = std::fs::metadata(path)?;
        if m.is_file() {
            Ok(m.len())
        } else {
            Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{path} is not a file")))
        }
    }

    fn list_dir(&self, dir: &str) -> Option<std::io::Result<Vec<String>>> {
        let dir = if dir.is_empty() { "." } else { dir };
        Some(std::fs::read_dir(dir).map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        }))
    }

    /// 导出先在内存编码再交给 `write_file`，才能走系统保存选择器（沙箱外无直接写权限）。
    fn export_in_memory(&self) -> bool {
        true
    }
}

/// 宿主 hook（OHOS）：文件/目录选择器走 ArkTS 系统选择器，命令/链接走宿主能力。
/// 单值选择器回传沙箱路径（reply 的 name 字段）；多选媒体回传 JSON 数组。
#[cfg(target_env = "ohos")]
fn hooks(files: &str, system_locale: &str) -> HostHooks {
    let mut h = HostHooks::default();
    // 导入媒体（多选）：ArkTS 把授权文件落沙箱 `.inbox`，回传 JSON 数组路径。
    h.pick_files = Some(Box::new({
        let files = files.to_string();
        move |exts: &[&str]| {
            let req = serde_json::json!({ "exts": exts.to_vec() }).to_string();
            pick_list("openmedia", &req, &files)
        }
    }));
    // 打开工程。
    h.pick_open_project = Some(Box::new({
        let files = files.to_string();
        move || pick_one("openproject", "FilmCraft Project", &files)
    }));
    // 保存/另存：上游只把返回值当路径字符串用，字节写入走上面注入的 OhosServices::write_file
    // （→ ArkTS "savebytes" 弹系统保存选择器）。这里不弹 picker，避免一次保存弹两次选择器；
    // ArkTS 直接把建议文件名回包即可。
    h.pick_save = Some(Box::new(move |name: &str| pick_one("saveproject", name, "")));
    // 导出/另存（含 filter）。
    h.pick_save_as = Some(Box::new(move |filter: &str, exts: &[&str], name: &str| {
        let req = serde_json::json!({ "filter": filter, "exts": exts.to_vec(), "name": name }).to_string();
        pick_one("saveas", &req, "")
    }));
    // JSON 预设导入。
    h.pick_open_file = Some(Box::new(move |filter: &str, exts: &[&str]| {
        let req = serde_json::json!({ "filter": filter, "exts": exts.to_vec() }).to_string();
        pick_one("openfile", &req, "")
    }));
    // 文件夹（Link Media / proxy / 导出帧）。
    h.pick_folder = Some(Box::new(|| pick_one("pickfolder", "", "")));
    h.pick_folder_at = Some(Box::new(move |dir: &str| {
        let req = serde_json::json!({ "dir": dir }).to_string();
        pick_one("pickfolderat", &req, "")
    }));
    // 重连媒体（Locate/Attach Proxies/Reconnect）。
    h.pick_file_for_relink = Some(Box::new(move |exts: &[&str], _hint| {
        let req = serde_json::json!({ "exts": exts.to_vec() }).to_string();
        pick_one("relink", &req, "")
    }));
    // 打开 URL（Help/About/Import 链接）。
    h.open_url = Some(Box::new(|url: &str| match bridge::request("openurl", url, &[]) {
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }));
    // 打开文件 / 在文件管理器显示。
    h.open_path = Some(Box::new(move |path: &str, reveal: bool| {
        let req = serde_json::json!({ "reveal": reveal }).to_string();
        match bridge::request("openpath", path, req.as_bytes()) {
            Ok(_) => Ok(()),
            Err(e) => Err(e),
        }
    }));
    // 系统语言：给「Interface Language ▸ System Language」解析用。
    // 闭包必须 'static：先把 &str 克隆成 owned String 再移入，Fn 可多次调用故每次 clone。
    h.system_languages = Some(Box::new({
        let locale = system_locale.to_string();
        move || {
            if locale.is_empty() {
                Vec::new()
            } else {
                vec![locale.clone()]
            }
        }
    }));
    // 系统主题（跟随系统）：appearance 模块已知当前深浅。
    h.system_theme = Some(Box::new(|_ctx: &egui::Context| appearance::current()));
    // 音频输出（cpal/OHOS 音频）首版不接：预览静音，编辑/合成不受影响。
    // run_dialog / clipboard_media / menu_changed / raise_without_focus / cursor_screen_position / pick_color 留空。
    h
}

/// 单选/单路径：阻塞等待 ArkTS 回包，返回 name（沙箱路径或 URI）；取消返回 None。
#[cfg(target_env = "ohos")]
fn pick_one(kind: &str, text: &str, _files: &str) -> Option<String> {
    match bridge::request(kind, text, &[]) {
        Ok((name, _bytes)) if !name.is_empty() => Some(name),
        Ok((_name, _bytes)) => {
            log::warn!("[fc] pick {kind} empty name");
            None
        }
        Err(e) => {
            if e.to_ascii_lowercase().contains("cancel") {
                log::info!("[fc] pick {kind} cancelled");
            } else {
                log::warn!("[fc] pick {kind} failed: {e}");
                bridge::notify(0, "error", &e, &[]);
            }
            None
        }
    }
}

/// 多选：回包 name 为 JSON 数组（路径列表）。
#[cfg(target_env = "ohos")]
fn pick_list(kind: &str, text: &str, files: &str) -> Vec<String> {
    let _ = files;
    match bridge::request(kind, text, &[]) {
        Ok((name, _bytes)) => {
            let name = name.trim();
            if name.is_empty() {
                return Vec::new();
            }
            if let Ok(arr) = serde_json::from_str::<Vec<String>>(name) {
                return arr;
            }
            // 单路径回退（无 JSON 数组时）。
            vec![name.to_string()]
        }
        Err(e) => {
            if e.to_ascii_lowercase().contains("cancel") {
                log::info!("[fc] pick_list {kind} cancelled");
            } else {
                log::warn!("[fc] pick_list {kind} failed: {e}");
            }
            Vec::new()
        }
    }
}

/// 把落沙箱的文件路径交给上游 `file.import` / `file.open`。失败只记日志、不中断帧循环。
#[cfg(target_env = "ohos")]
fn open_into_app(app: &mut FilmcraftApp, path: &str) {
    // 优先按媒体导入（大多数拖入/选择的是素材）；若上游识别为工程则其内部走 file.open。
    match app.session.execute("file.import", serde_json::json!({ "paths": [path] })) {
        Ok(v) => {
            let s = v.to_string();
            let s = s.chars().take(120).collect::<String>();
            log::info!("[fc] import {path}: {s}");
        }
        Err(e) => {
            log::warn!("[fc] import failed for {path}: {e}");
            bridge::notify(0, "error", &format!("import: {e}"), &[]);
        }
    }
}

#[cfg(not(target_env = "ohos"))]
fn worker(_config: Config, _rx: mpsc::Receiver<Message>) {
    bridge::notify(0, "error", "Native host requires an OHOS target", &[]);
}
