import native from 'libfilmcraft.so';
import { AbilityConstant, Configuration, UIAbility, Want } from '@kit.AbilityKit';
import { window } from '@kit.ArkUI';
import { BusinessError } from '@kit.BasicServicesKit';
import { publishSystemTheme, systemThemeFromColorMode } from '../SystemTheme';

class SystemThemeInput {
  kind: string = 'system_theme';
  text: string = '';
  constructor(text: string) { this.text = text; }
}

export default class EntryAbility extends UIAbility {
  private syncSystemTheme(colorMode: number | undefined): void {
    const theme = systemThemeFromColorMode(colorMode);
    publishSystemTheme(theme);
    // Push live changes into the native worker; init also carries the first value.
    try {
      native.input(JSON.stringify(new SystemThemeInput(theme)));
    } catch (error) {
      console.info('System theme input unavailable before native init');
    }
  }

  onCreate(want: Want, _launchParam: AbilityConstant.LaunchParam): void {
    this.syncSystemTheme(this.context.config?.colorMode);
    this.maybeEnableTest(want);
    console.info('FilmCraft native ability created');
  }

  onNewWant(want: Want): void {
    // A file opened from the system shares its URI through startAbility; the page drains it on load.
    if (want?.uri) {
      AppStorage.setOrCreate('filmcraftOpenWant', want.uri);
    }
    this.maybeEnableTest(want);
  }

  // 诊断自测入口：`aa start -b <bundle> -a EntryAbility --ps fcTest 1` 传入非空 fcTest 时，
  // 让页面在首帧后自动跑一遍引擎命令序列（结果经 hilog 的 command/command_err 回传）。
  // 正常用户启动不带该参数，行为不变。
  private maybeEnableTest(want: Want): void {
    try {
      const params = want?.parameters;
      const v = params ? params['fcTest'] : undefined;
      if (v !== undefined && v !== null && String(v) !== '') {
        AppStorage.setOrCreate('filmcraftTest', String(v));
        console.info('FilmCraft FC-TEST enabled via want');
      }
    } catch (error) {
      console.info(`fcTest param unavailable: ${(error as Error).message}`);
    }
  }

  onConfigurationUpdate(newConfig: Configuration): void {
    this.syncSystemTheme(newConfig.colorMode);
  }

  onForeground(): void {
    native.active(true);
  }

  onBackground(): void {
    native.active(false);
  }

  onWindowStageCreate(windowStage: window.WindowStage): void {
    if (AppStorage.get<string>('filmcraftSystemTheme') === undefined) {
      this.syncSystemTheme(this.context.config?.colorMode);
    }
    // 系统标题栏的关闭按钮：交给引擎做未保存工程的「保存更改？」确认。
    // SDK 26 声明 on('windowWillClose', Callback<void, Promise<boolean>>)（@since 15）：
    // 回调必须返回 Promise<boolean>，true 拦截本次关闭、false 放行；写成同步 (): boolean
    // 会报 10505001 No overload matches this call（DeckCraft 移植报告已实证）。
    // 用户点 Save / Don't Save 后，Rust 会 notify('close') → Index.ets terminateSelf()。
    try {
      const mainWindow = windowStage.getMainWindowSync();
      mainWindow.on('windowWillClose', async (): Promise<boolean> => {
        try {
          native.close();
          return true;
        } catch (error) {
          // native 未就绪时不能把窗口锁死，放行系统关闭作为兜底。
          console.warn(`FilmCraft native close unavailable: ${(error as Error).message}`);
          return false;
        }
      });
    } catch (error) {
      console.warn(`FilmCraft windowWillClose unavailable: ${(error as Error).message}`);
    }
    windowStage.loadContent('pages/Index', (error: BusinessError) => {
      if (error.code) {
        console.error(`FilmCraft page failed: ${error.code} ${error.message}`);
      }
    });
  }
}
