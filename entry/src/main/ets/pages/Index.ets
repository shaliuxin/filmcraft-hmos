import native from 'libfilmcraft.so';
import { common } from '@kit.AbilityKit';
import { fileIo, fileUri, picker } from '@kit.CoreFileKit';
import { i18n } from '@kit.LocalizationKit';
import { BusinessError, deviceInfo } from '@kit.BasicServicesKit';
import { display } from '@kit.ArkUI';
import { inputMethod } from '@kit.IMEKit';
import { SYSTEM_THEME_DARK, SYSTEM_THEME_LIGHT } from '../SystemTheme';

// Mirrors the Rust `Config` deserialized in native/rust/src/lib.rs.
class NativeConfig {
  files: string;
  documents: string;
  scale: number;
  language: string;
  systemLocale: string;
  displayP3: boolean;
  systemTheme: string;
  fullscreen: boolean;
  deviceType: string;
  widthVp: number;
  constructor(files: string, documents: string, scale: number, language: string, systemLocale: string,
              displayP3: boolean, systemTheme: string, fullscreen: boolean, deviceType: string, widthVp: number) {
    this.files = files;
    this.documents = documents;
    this.scale = scale;
    this.language = language;
    this.systemLocale = systemLocale;
    this.displayP3 = displayP3;
    this.systemTheme = systemTheme;
    this.fullscreen = fullscreen;
    this.deviceType = deviceType;
    this.widthVp = widthVp;
  }
}

/**
 * 系统语言的完整 BCP-47 标签（如 `zh-Hans-CN`）：Rust 侧用它决定界面语言，也据此挑中文字体。
 * getSystemLocale 在个别设备上可能不可用，退回只带语言的 getSystemLanguage。
 */
function systemLocaleTag(): string {
  try {
    const locale = i18n.System.getSystemLocale();
    if (locale && locale.length > 0) {
      return locale;
    }
  } catch (error) {
    console.info(`System locale unavailable: ${(error as Error).message}`);
  }
  return i18n.System.getSystemLanguage();
}

/**
 * 对文件名或 file:// URI 里的百分号（%xx）解码，得到真实可读名（如 文档1）。
 * filmcraft 用回传路径的末段（file_stem）作文档标题，若不先解码，标题/信息页/最近列表
 * 会显示原始百分号串（%E6%96%87…）。
 * - 含 `://`（完整 URI）：只解码各路径段，保留 scheme 与分隔符。
 * - 其余（裸文件名）：整体解码。
 * 解码失败则原样返回，绝不影响原值。
 */
function percentDecodeName(s: string): string {
  if (!s || s.length === 0) { return s; }
  try {
    if (s.includes('://')) {
      const parts = s.split('/');
      for (let i = 1; i < parts.length; i++) { parts[i] = decodeURIComponent(parts[i]); }
      return parts.join('/');
    }
    return decodeURIComponent(s);
  } catch (error) {
    return s;
  }
}

/**
 * 把 file:// URI 各路径段重新百分号编码（保留 scheme 与分隔符），用于把「解码后的名」
 * 还原成 picker 原始 URI 形式，供 fileIo.openSync 打开写入（兜底：若解码形式打不开则用此）。
 */
function percentEncodePath(uri: string): string {
  try {
    const parts = uri.split('/');
    for (let i = 1; i < parts.length; i++) { parts[i] = encodeURIComponent(parts[i]); }
    return parts.join('/');
  } catch (error) {
    return uri;
  }
}

class NativeMessage { kind: string = ''; text: string = ''; }

@Entry
@Component
struct Index {
  @State private failure: string = '';
  @State private initialized: boolean = false;
  @State private startupVisible: boolean = true;
  @State private startupOpacity: number = 1;
  private alive: boolean = false;
  private generation: number = 0;
  private context: common.UIAbilityContext = this.getUIContext().getHostContext() as common.UIAbilityContext;
  @State private deviceScale: number = 1;
  // 中文输入法（与 DeckCraft / GridCraft 同一修法）：官方「自定义编辑框」方案要求
  // attach 前先有一个获焦的 ArkUI 编辑组件（XComponent 原生表面不算），
  // 且监听器必须在 attach 成功之后注册（否则 12800009）。
  private imeController: inputMethod.InputMethodController | undefined = undefined;
  private imeAttached: boolean = false;
  private imeShown: boolean = false;
  private imeInComposition: boolean = false;
  private imeBound: boolean = false;
  private imeWanted: boolean = false;
  // 引擎命令自测（诊断）：由 EntryAbility 经 want 参数 fcTest 触发，首帧后逐条 native.command()。
  private fcTestRunning: boolean = false;

  aboutToAppear(): void {
    console.info('FilmCraft native page appearing');
    this.alive = true;
    this.generation++;
    this.initializeHost(this.generation);
    // 启动兜底：遮罩盖在 XComponent 之上，若首帧通知因任何原因没到，它会永久遮住原生画面
    // （表现为满屏漆黑）。到点必须揭开。
    setTimeout((): void => { this.hideStartup(); }, 4000);
  }

  aboutToDisappear(): void {
    this.alive = false;
    this.generation++;
    this.initialized = false;
    native.active(false);
  }

  private async initializeHost(generation: number): Promise<void> {
    try {
      let scale: number = this.getUIContext().vp2px(1);
      this.deviceScale = scale;
      let systemTheme = AppStorage.get<string>('filmcraftSystemTheme') ?? '';
      if (systemTheme !== SYSTEM_THEME_DARK && systemTheme !== SYSTEM_THEME_LIGHT) {
        systemTheme = '';
      }
      const filesDir = this.context.filesDir;
      const documents = filesDir + '/Documents';
      try { fileIo.mkdirSync(documents, true); } catch (e) { /* exists */ }
      native.init(JSON.stringify(new NativeConfig(
          filesDir, documents, scale, i18n.System.getSystemLanguage(), systemLocaleTag(), false, systemTheme,
          false, deviceInfo.deviceType, this.getUIContext().px2vp(display.getDefaultDisplaySync().width))),
        (id: number, message: string, data: ArrayBuffer): void => { this.receive(id, message, data); });
      this.initialized = true;
      this.failure = '';
      // A file launched from the system shares its URI through onNewWant.
      const want = AppStorage.get<string>('filmcraftOpenWant');
      if (want && want.length > 0) {
        AppStorage.setOrCreate('filmcraftOpenWant', '');
        this.openWant(want);
      }
    } catch (error) {
      if (this.alive && generation === this.generation) { this.failure = (error as Error).message; }
    }
  }

  private openWant(uri: string): void {
    try {
      const path = new fileUri.FileUri(uri).path;
      const name = path.length > 0 ? path.substring(path.lastIndexOf('/') + 1) : 'document';
      const file = fileIo.openSync(path, fileIo.OpenMode.READ_ONLY);
      const bytes = this.readAll(file.fd);
      fileIo.closeSync(file);
      native.open(name, bytes);
    } catch (error) {
      console.error(`FilmCraft open want failed: ${(error as Error).message}`);
    }
  }

  private readAll(fd: number): ArrayBuffer {
    const chunks: ArrayBuffer[] = [];
    let total = 0;
    const bufSize = 1024 * 1024;
    while (true) {
      const buf = new ArrayBuffer(bufSize);
      const n = fileIo.readSync(fd, buf);
      if (n <= 0) { break; }
      chunks.push(n === bufSize ? buf : buf.slice(0, n));
      total += n;
    }
    const out = new Uint8Array(total);
    let offset = 0;
    for (const c of chunks) {
      out.set(new Uint8Array(c), offset);
      offset += c.byteLength;
    }
    return out.buffer;
  }

  private receive(id: number, message: string, data: ArrayBuffer): void {
    const request = JSON.parse(message) as NativeMessage;
    console.info(`FilmCraft native ${request.kind}: ${request.text}`);
    if (!this.alive) {
      if (id > 0) { native.reply(id, false, 'cancelled', new ArrayBuffer(0)); }
      return;
    }
    if (request.kind === 'error') {
      this.failure = request.text;
    } else if (request.kind === 'ready' || request.kind === 'first_frame') {
      this.failure = '';
      if (request.kind === 'first_frame') {
        this.revealEditor();
        const testFlag = AppStorage.get<string>('filmcraftTest');
        if (testFlag && testFlag.length > 0) {
          AppStorage.setOrCreate('filmcraftTest', '');
          setTimeout((): void => this.startFCTest(), 1500);
        }
      }
    } else if (request.kind === 'close') {
      this.context.terminateSelf();
    } else if (request.kind === 'url') {
      this.context.startAbility({ action: 'ohos.want.action.viewData', uri: request.text })
        .catch((error: BusinessError) => { console.error(`External link: ${error.message}`); });
    } else if (request.kind === 'open' || request.kind === 'file') {
      this.pickDocument(id, false);
    } else if (request.kind === 'save') {
      this.pickSave(id, request.text);
    } else if (request.kind === 'hostsave') {
      // host_save：把字节写到 pickSave 选出的 URI（request.text），data 即文档字节。
      this.writeSave(id, request.text, data);
    } else if (request.kind === 'ime') {
      // Rust 通知：egui 文本字段获得/失去焦点 → 弹起/收起软键盘。
      this.setImeKeyboard(request.text === 'show');
    }
  }

  // ===================== 中文输入法（IME） =====================
  // 鸿蒙软键盘中文输入法走 InputMethod 框架，不经 XComponent 原始按键事件。
  // 顺序（实测必须）：锚点获焦 → attach(showKeyboard=true) → attach 成功后注册监听。
  // 否则 attach 报 12800003（编辑框未获焦）或监听注册报 12800009（未附加）。

  private imeControllerOrGet(): inputMethod.InputMethodController | undefined {
    if (this.imeController) {
      return this.imeController;
    }
    try {
      const controller: inputMethod.InputMethodController = inputMethod.getController();
      this.imeController = controller;
      return controller;
    } catch (error) {
      console.error(`FilmCraft IME controller failed: ${(error as Error).message}`);
      return undefined;
    }
  }

  private bindImeListeners(controller: inputMethod.InputMethodController): void {
    if (this.imeBound) {
      return;
    }
    try {
      // insertText = 已确认上屏文本；setPreviewText = 拼音组合态；finishTextPreview = 组合结束；
      // deleteLeft = 退格（组合态内由键盘自行处理）；sendFunctionKey = 回车；sendKeyboardStatus = 显隐。
      controller.on('insertText', (text: string): void => { this.sendIme('commit', text); });
      controller.on('setPreviewText', (text: string, _range: inputMethod.Range): void => {
        this.imeInComposition = (text.length > 0);
        this.sendIme('preedit', text);
      });
      controller.on('finishTextPreview', (): void => { this.imeInComposition = false; this.sendIme('preedit', ''); });
      controller.on('deleteLeft', (length: number): void => {
        if (!this.imeInComposition) { this.sendIme('delete', String(length)); }
      });
      controller.on('sendFunctionKey', (_key: inputMethod.FunctionKey): void => { this.sendIme('enter', ''); });
      controller.on('sendKeyboardStatus', (status: inputMethod.KeyboardStatus): void => {
        this.imeShown = (status === inputMethod.KeyboardStatus.SHOW);
      });
      this.imeBound = true;
      console.info('FilmCraft IME listeners bound');
    } catch (error) {
      console.warn(`FilmCraft IME bind failed: ${(error as Error).message}`);
    }
  }

  private async setImeKeyboard(show: boolean): Promise<void> {
    if (!this.alive) {
      return;
    }
    this.imeWanted = show;
    const controller = this.imeControllerOrGet();
    if (!controller) {
      return;
    }
    if (show) {
      try {
        this.getUIContext().getFocusController().requestFocus('filmcraft-ime-anchor');
      } catch (error) {
        console.warn(`FilmCraft IME anchor focus failed: ${(error as Error).message}`);
      }
      try {
        await controller.attach(true, this.imeTextConfig());
        this.imeAttached = true;
        this.imeShown = true;
        console.info('FilmCraft IME attached');
      } catch (error) {
        console.warn(`FilmCraft IME attach failed: ${(error as Error).message}`);
        try {
          await controller.showTextInput(inputMethod.RequestKeyboardReason.OTHER);
          this.imeAttached = true;
          this.imeShown = true;
        } catch (e2) {
          console.warn(`FilmCraft IME show failed: ${(e2 as Error).message}`);
        }
      }
      if (this.imeAttached) {
        this.bindImeListeners(controller);
        try {
          // PC 模式/外接键盘下 attach(true) 可能只出现悬浮工具栏，TOUCH 理由尝试强制展开面板。
          await controller.showTextInput(inputMethod.RequestKeyboardReason.TOUCH);
        } catch (error) {
          console.warn(`FilmCraft IME touch-show failed: ${(error as Error).message}`);
        }
      }
    } else {
      this.imeInComposition = false;
      try {
        if (this.imeAttached) { await controller.hideTextInput(); }
      } catch (error) {
        console.warn(`FilmCraft IME hide failed: ${(error as Error).message}`);
      }
      try {
        if (this.imeAttached) { await controller.detach(); }
      } catch (error) {
        // 未绑定 / 重复 detach：忽略。
      }
      this.imeAttached = false;
      this.imeShown = false;
    }
  }

  private imeTextConfig(): inputMethod.TextConfig {
    return {
      inputAttribute: {
        textInputType: inputMethod.TextInputType.TEXT,
        enterKeyType: inputMethod.EnterKeyType.NEWLINE,
        consumeKeyEvents: true,
      },
    };
  }

  private sendIme(op: string, text: string): void {
    if (!this.alive) {
      return;
    }
    native.input(JSON.stringify({ kind: 'ime', op, text }));
  }

  // ===================== 引擎命令自测（诊断） =====================
  // 直接驱动上游 `Session::execute`（经 native.command → wc_command），绕过 UI 点击坐标的不确定性，
  // 覆盖 项目/选择/剪贴板/标记/播放头/序列/修剪/效果/偏好 等子系统。刻意避开会阻塞等 picker 回包的
  // 命令（file.open / file.import / file.save / file.saveAs / file.export*），以免 worker 卡在 reply。
  // 每条结果由 lib.rs 经 `command`(成功)/`command_err`(失败) 通知回传 hilog，便于核对。
  private fcTestPayloads(): string[] {
    return [
      '{"id":"file.openDemoProject","params":{}}',
      '{"id":"edit.selectAll","params":{}}',
      '{"id":"edit.copy","params":{}}',
      '{"id":"edit.paste","params":{}}',
      '{"id":"markers.markSelection","params":{}}',
      '{"id":"markers.clearAll","params":{}}',
      '{"id":"playhead.start","params":{}}',
      '{"id":"playhead.set","params":{"frame":5}}',
      '{"id":"playhead.stepForward","params":{}}',
      '{"id":"playhead.stepBack","params":{}}',
      '{"id":"sequence.setWorkArea","params":{"frame":0,"frameEnd":5}}',
      '{"id":"sequence.snap","params":{}}',
      '{"id":"trim.forward","params":{}}',
      '{"id":"trim.backward","params":{}}',
      '{"id":"effects.reset","params":{}}',
      '{"id":"effects.toggleEnabled","params":{}}',
      '{"id":"edit.deselectAll","params":{}}',
      '{"id":"edit.undo","params":{}}',
      '{"id":"edit.redo","params":{}}',
      '{"id":"prefs.reset","params":{}}',
      '{"id":"file.newProject","params":{}}',
      '{"id":"file.newSequence","params":{"name":"FC-Test","width":1280,"height":720,"fps":24}}',
    ];
  }

  private startFCTest(): void {
    if (this.fcTestRunning) {
      return;
    }
    this.fcTestRunning = true;
    const payloads = this.fcTestPayloads();
    console.info(`FC-TEST begin (${payloads.length} engine commands)`);
    this.fireFcTest(payloads, 0);
  }

  private fireFcTest(payloads: string[], i: number): void {
    if (!this.alive) {
      this.fcTestRunning = false;
      console.info(`FC-TEST aborted (page torn down) at ${i}/${payloads.length}`);
      return;
    }
    if (i >= payloads.length) {
      console.info(`FC-TEST done: ${payloads.length} commands dispatched`);
      this.fcTestRunning = false;
      return;
    }
    console.info(`FC-TEST [${i + 1}/${payloads.length}] ${payloads[i]}`);
    try {
      native.command(payloads[i]);
    } catch (error) {
      console.error(`FC-TEST fire failed: ${(error as Error).message}`);
    }
    setTimeout((): void => { this.fireFcTest(payloads, i + 1); }, 800);
  }

  private async pickDocument(id: number, _gallery: boolean): Promise<void> {
    const generation = this.generation;
    try {
      const uris = await new picker.DocumentViewPicker(this.context).select(new picker.DocumentSelectOptions());
      if (!this.alive || generation !== this.generation || uris.length === 0) {
        console.info(`FilmCraft pick open: cancelled (alive=${this.alive} gen ${generation}!=${this.generation} n=${uris.length})`);
        native.reply(id, false, 'cancelled', new ArrayBuffer(0));
        return;
      }
      const uriStr = uris[0];
      const info = new fileUri.FileUri(uriStr);
      // 文件名先解码，落沙箱的临时名与 filmcraft 显示的文档名都应是真实可读名（而非 %xx 串）。
      const baseName = percentDecodeName(info.name || uriStr.substring(uriStr.lastIndexOf('/') + 1) || 'document');
      console.info(`FilmCraft pick open: uri=${uriStr} path=${info.path} name=${baseName}`);

      // 把 picker 授权的文件复制进沙箱 .inbox，Rust 侧再用 std::fs 按路径读取。
      // 关键：fileIo.copyFileSync 原生支持 picker 返回的 file:// URI（含非 ASCII 名），
      // 直接完成 URI→沙箱复制；不要自己 open+writeSync 循环——writeSync 的 buffer 参数
      // 必须是 ArrayBuffer/string，传 Uint8Array 视图会触发 "Invalid arraybuffer or options"。
      const inbox = this.context.filesDir + '/.inbox';
      try {
        fileIo.mkdirSync(inbox, true);
      } catch (e) {
        // 目录已存在。
      }
      const dst = inbox + '/' + Date.now() + '_' + baseName;
      try {
        fileIo.copyFileSync(uriStr, dst);
      } catch (uriErr) {
        // 极少数情况下按 URI 复制失败，退回用解码后的真实路径复制。
        console.warn(`FilmCraft pick open: copySync by URI failed (${(uriErr as Error).message}); trying decoded path`);
        fileIo.copyFileSync(info.path, dst);
      }
      const size = fileIo.statSync(dst).size;
      console.info(`FilmCraft pick open: copied ${size} bytes -> ${dst}`);
      // 把沙箱路径回传给 Rust（open/file 读此路径，save 写此路径）。
      native.reply(id, true, dst, new ArrayBuffer(0));
    } catch (error) {
      const err = error as Error;
      console.error(`FilmCraft pick open FAILED: ${err.message}\n${err.stack ?? ''}`);
      native.reply(id, false, String(err.message ?? error), new ArrayBuffer(0));
    }
  }

  // 另存为：弹系统保存位置选择器（DocumentViewPicker.save），把用户选的 file:// URI 回传上游作为
  // 「保存路径」；真正写盘由 hostsave（上游 host_save）经该 URI + fd 完成——OHOS 沙箱外位置只能经
  // picker 授权 URI 访问，Rust std::fs 写不了，故走 host_save。
  private async pickSave(id: number, suggested: string): Promise<void> {
    const generation = this.generation;
    try {
      if (!this.alive || generation !== this.generation) {
        native.reply(id, false, 'cancelled', new ArrayBuffer(0));
        return;
      }
      const suggestedName = suggested.length > 0 ? suggested : 'document.docx';
      const options = new picker.DocumentSaveOptions();
      options.newFileNames = [suggestedName];
      const uris = await new picker.DocumentViewPicker(this.context).save(options);
      if (!this.alive || generation !== this.generation || !uris || uris.length === 0) {
        console.info(`FilmCraft save: picker cancelled (alive=${this.alive} n=${uris ? uris.length : 0})`);
        native.reply(id, false, 'cancelled', new ArrayBuffer(0));
        return;
      }
      const uri = uris[0];
      // 回传解码后的名：filmcraft 用末段作标题/落盘名，解码后即为真实文件名（文档1 而非 %E6%96%87…）。
      // 真正的写入由 hostsave 完成，writeSave 会先按此 URI 打开、打不开再回退到编码形式。
      const name = percentDecodeName(uri);
      console.info(`FilmCraft save: picker -> ${uri} (name=${name})`);
      native.reply(id, true, name, new ArrayBuffer(0));
    } catch (error) {
      const err = error as Error;
      console.error(`FilmCraft save picker FAILED: ${err.message}`);
      native.reply(id, false, String(err.message ?? error), new ArrayBuffer(0));
    }
  }

  // host_save 落盘：把字节写到 picker 授权的 URI（沙箱外位置）。ArrayBuffer 恰为字节长度（见 C++ CallJS）。
  private writeSave(id: number, uri: string, bytes: ArrayBuffer): void {
    if (!uri || uri.length === 0) {
      native.reply(id, false, 'no save target uri', new ArrayBuffer(0));
      return;
    }
    // pickSave 回传的是「解码后的名」（便于 filmcraft 正确显示标题/落盘名）。写入时优先按此 URI 打开；
    // 若系统 fileIo 要求 picker 原始的百分号形式，则回退到编码形式重试——两条都试，保证写盘成功。
    const targets: string[] = [uri];
    const enc = percentEncodePath(uri);
    if (enc !== uri) { targets.push(enc); }
    let lastErr = '';
    for (const target of targets) {
      try {
        const file = fileIo.openSync(target, fileIo.OpenMode.READ_WRITE | fileIo.OpenMode.CREATE | fileIo.OpenMode.TRUNC);
        try {
          fileIo.writeSync(file.fd, bytes);
        } finally {
          fileIo.closeSync(file);
        }
        console.info(`FilmCraft save: wrote ${bytes.byteLength} bytes -> ${target}`);
        native.reply(id, true, target, new ArrayBuffer(0));
        return;
      } catch (error) {
        lastErr = String((error as Error).message ?? error);
        console.warn(`FilmCraft save write: openSync failed for ${target}: ${lastErr}`);
      }
    }
    console.error(`FilmCraft save write FAILED: ${lastErr}`);
    native.reply(id, false, lastErr, new ArrayBuffer(0));
  }

  private hideStartup(): void {
    if (!this.alive || !this.startupVisible) { return; }
    this.startupOpacity = 0;
    this.startupVisible = false;
  }

  private revealEditor(): void {
    if (!this.startupVisible) { return; }
    // 遮罩必须无条件消失：动画在任何环节失败都不能把原生画面永久盖住。
    try {
      this.getUIContext().animateTo({ duration: 320, curve: Curve.EaseOut, onFinish: (): void => this.hideStartup() },
        (): void => { this.startupOpacity = 0; });
    } catch (error) {
      console.warn(`Reveal animation failed: ${(error as Error).message}`);
      this.hideStartup();
    }
    // 兜底：动画若被系统丢弃（回调线程/丢帧），首帧之后必须让原生画面露出来。
    setTimeout((): void => this.hideStartup(), 600);
  }

  build() {
    Stack() {
      if (this.initialized) {
        XComponent({ id: 'filmcraft-native', type: XComponentType.SURFACE, libraryname: 'filmcraft' })
          .width('100%').height('100%')
          .onLoad((): void => {
            try { this.getUIContext().getFocusController().requestFocus('filmcraft-editor'); } catch (e) {}
          })
          // 不设 focusOnTouch：打字期间点击画布若把焦点抢回 XComponent（非编辑代理），
          // 会断开输入法绑定导致键盘收起；触摸/指针事件不经 ArkUI 焦点，不受影响。
          .id('filmcraft-editor').focusable(true).defaultFocus(true)
      }
      // 隐藏的 IME 锚点：输入法框架只认「获得焦点的编辑组件」为可输入位置，
      // attach 前必须先聚焦它（官方自定义编辑框方案）。
      Text('')
        .id('filmcraft-ime-anchor')
        .focusable(true)
        .width(1).height(1)
        .opacity(0)
        .hitTestBehavior(HitTestMode.None)
        .onBlur((): void => {
          if (this.imeAttached) {
            this.imeAttached = false;
            this.imeShown = false;
            console.info('FilmCraft IME anchor blurred, binding reset');
          }
        })
      if (this.startupVisible) {
        Column() {
          Column({ space: 18 }) {
            Image($r('app.media.app_icon')).width(76).height(76).borderRadius(18)
            Text('FilmCraft').fontSize(30).fontWeight(FontWeight.Medium).fontColor('#F4F4F5')
            Row({ space: 10 }) {
              LoadingProgress().width(18).height(18).color('#A5BFFF')
              Text(this.initialized ? '正在启动编辑器…' : '正在准备文档目录…')
                .fontSize(14).fontColor('#B8BAC2')
            }
          }
        }
        .width('100%').height('100%').justifyContent(FlexAlign.Center)
        .backgroundColor('#10203F').opacity(this.startupOpacity)
        .hitTestBehavior(HitTestMode.Block)
      }
      if (this.failure.length > 0) {
        Column() {
          Text('FilmCraft 原生运行错误').fontSize(20).fontColor('#FFFFFF')
          Text(this.failure).fontSize(14).fontColor('#FFFFFF').margin({ top: 12 })
          if (!this.initialized) {
            Button('重试').onClick((): void => { this.failure = ''; this.initializeHost(this.generation); })
          } else {
            Button('关闭提示').onClick((): void => { this.failure = ''; })
          }
        }.padding(24).backgroundColor('#DD262626').borderRadius(8).width('80%')
      }
    }.width('100%').height('100%').backgroundColor('#262626')
  }
}
