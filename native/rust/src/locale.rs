//! 系统语言与系统字体：把鸿蒙的 locale / 字体接进 wordcraft 上游。
//!
//! 语言（已核实上游 API）：wordcraft 的 `i18n::detect_system_lang()` 先查 `WORDCRAFT_LANGUAGE`
//! 环境变量（`lang_from_tag`），再退回 `sys_locale::get_locales()`。`sys-locale` 在 Linux 上只读
//! `LANGUAGE`/`LC_ALL`/`LC_MESSAGES`/`LANG`，而鸿蒙应用进程不带这些变量，于是「跟随系统」永远
//! 解析成英文。ArkTS 侧本来就能拿到真实 locale（`i18n.System.getSystemLocale()`），这里把
//! BCP-47 标签同时写进 `WORDCRAFT_LANGUAGE`（界面语言）与 `LANG`（区域/纸张/小数点，
//! `i18n::system_region()` 经 `Region::from_locale_vars` 读它），上游自己的「跟随系统」便完整生效。
//!
//! 字体（与 solidcraft 不同，须注意）：wordcraft **没有** 运行时的字体目录环境变量。它的界面 CJK
//! 面来自编译期 `CRAFT_FONTS_DIR`（`crates/fonts/build.rs` 读它内嵌 craft-fonts），运行时
//! `system_font_dirs()` 在 `target_os="linux"`（含 OHOS）分支只扫 `/usr/share/fonts`、
//! `$HOME/.fonts`、`$HOME/.local/share/fonts` 等固定目录，**不读任何环境变量**。所以 solidcraft
//! 那套 `set_var("SOLIDCRAFT_FONT_DIRS")` 对 wordcraft 无效。这里做两件 best-effort 的事：
//!   1) 编译期由 `build_native.sh` 传 `CRAFT_FONTS_DIR`（craft-fonts checkout）内嵌全套面——主路径；
//!   2) 运行时把鸿蒙系统 CJK/阿拉伯面按上游候选名挂到 `$HOME/.fonts/`（OHOS 沙箱的 `HOME`）下
//!      作回退。因 OHOS 无法在主机端验证运行时行为，第 2 步仅尽力而为并记日志，不阻塞首帧。

use std::path::{Path, PathBuf};

/// 上游 `candidates()`（非 Windows/非 macOS 分支）认得的 CJK 候选文件名，取最短的一个挂进沙箱。
/// 同时命中 `Region::Hans` 的 `notosanscjk` 关键词，一个文件补上 CJK 与中文两个槽。
const CJK_REL: &str = "noto-cjk/NotoSansCJK-Regular.ttc";
/// 上游 `arabic_candidates()` 认得的阿拉伯候选名。
const ARABIC_REL: &str = "noto/NotoSansArabic-Regular.ttf";

/// 鸿蒙系统字体目录。
const SYSTEM_FONTS: &str = "/system/fonts";

/// 按界面语言挑 CJK 面。鸿蒙没有日/韩专用面，非繁体一律用 SC（含全部常用汉字）。
fn cjk_face(language: &str) -> &'static str {
    match language {
        "zh-TW" => "HarmonyOS_Sans_TC.ttf",
        _ => "HarmonyOS_Sans_SC.ttf",
    }
}

/// 系统 locale 的 BCP-47 形式（`zh_CN` → `zh-CN`）。`locale` 缺失时退回 `language`。
fn system_tag(locale: &str, language: &str) -> Option<String> {
    let tag = [locale, language].into_iter().map(str::trim).find(|tag| !tag.is_empty())?;
    Some(tag.chars().map(|c| if c == '_' { '-' } else { c }).collect())
}

/// 在 `wc_init` 里、任何 UI 与字体读取之前调用一次。
///
/// 顺序有意义：先写环境变量，再让 `i18n::system_lang()` 首次求值（进程级 `OnceLock`，
/// 第一次读到什么就固定什么）。
pub fn install(files_dir: &str, locale: &str, language: &str) {
    let tag = system_tag(locale, language);
    if let Some(tag) = &tag {
        // 界面语言：wordcraft 的 detect_system_lang 首查此变量。
        std::env::set_var("WORDCRAFT_LANGUAGE", tag);
        // 区域/纸张/小数点：system_region 经 Region::from_locale_vars 读 LANG 等。
        // sys-locale 按 LANGUAGE → LC_ALL → LC_MESSAGES → LANG 取值：先清掉可能残留的高优先级
        // 变量，再写 LANG（与桌面 Linux 用法一致）。
        for key in ["LANGUAGE", "LC_ALL", "LC_MESSAGES"] {
            std::env::remove_var(key);
        }
        std::env::set_var("LANG", tag);
    }
    // 把鸿蒙系统字体挂到 $HOME/.fonts（wordcraft 在 linux 分支只扫 $HOME/.fonts、
    // $HOME/.local/share/fonts 等固定目录，不读任何环境变量）。
    //
    // 关键：wordcraft 的 system_font_dirs() 用 env::var("HOME") 拼路径，而 stage 的目标目录必须
    // 与之一致。OHOS 应用进程里 HOME 可能已被设成别的路径或空串——此时「挂进 files_dir/.fonts」
    // 和「从 $HOME/.fonts 找」就不是同一个目录，find_ui_font 必然找不到（界面中文显示方块）。
    // 因此这里**无条件**把 HOME 指向 files_dir，再挂到 $HOME/.fonts，保证挂与找同目录。
    let prior_home = std::env::var_os("HOME").map(|s| s.to_string_lossy().into_owned());
    std::env::set_var("HOME", files_dir);
    log::info!("locale: prior HOME={prior_home:?} -> now {files_dir}");
    let tag_str = tag.as_deref().unwrap_or_default();
    let home = Path::new(files_dir);
    // 命中 wordcraft 的多个候选名（hans 首选 noto-cjk/NotoSansCJK-Regular.ttc，其次 …/NotoSansCJKsc
    // 等）与两个用户字体根，任一命中即可，尽最大可能让 find_ui_font 找到中文字形。
    let mut ok = false;
    for base in [home.join(".fonts"), home.join(".local/share/fonts")] {
        if let Ok(dir) = stage_fonts(&base, tag_str) {
            let staged = dir.join(CJK_REL);
            if let Ok(meta) = std::fs::metadata(&staged) {
                log::info!(
                    "locale: tag={tag_str:?} HOME={} cjk staged at {} ({} bytes)",
                    home.display(), staged.display(), meta.len()
                );
                ok = true;
                break;
            }
        }
    }
    if !ok {
        log::warn!("locale: tag={tag_str:?} system CJK font NOT staged — interface Chinese may show as boxes");
    }
}

/// 把要用的系统字体挂到 `base`（= `$HOME/.fonts`）下，返回挂好的目录。
///
/// 鸿蒙沙箱可能禁止建软链（实测 EACCES），于是退回复制——只在第一次（或系统换了字体文件名）时
/// 发生，之后按文件名命中直接复用。
fn stage_fonts(base: &Path, language: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(base).map_err(|e| format!("create .fonts: {e}"))?;
    let cjk = cjk_face(language);
    // 相对名带子目录（noto-cjk/…、noto/…），先把父目录建出来。
    stage_one(base, CJK_REL, cjk)?;
    // 阿拉伯面缺失只影响语言列表里那一项的显示，界面照常，不算失败。
    if stage_one(base, ARABIC_REL, "HarmonyOS_Sans_Naskh_Arabic_UI.ttf").is_err() {
        log::info!("locale: no system Arabic face");
    }
    Ok(base.to_path_buf())
}

/// 在 `base` 下按 `rel`（如 `noto-cjk/NotoSansCJK-Regular.ttc`）放一份系统字体 `face`。
fn stage_one(base: &Path, rel: &str, face: &str) -> Result<(), String> {
    let source = Path::new(SYSTEM_FONTS).join(face);
    if !source.is_file() {
        return Err(format!("no {face} in {SYSTEM_FONTS}"));
    }
    let link = base.join(rel);
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    // 断链：系统升级把字体文件改了名。
    if let Ok(meta) = std::fs::symlink_metadata(&link) {
        if meta.file_type().is_symlink() && !link.exists() {
            let _ = std::fs::remove_file(&link);
        }
    }
    if std::fs::symlink_metadata(&link).is_ok() {
        return Ok(());
    }
    // 沙箱常禁软链：退回复制。
    if std::os::unix::fs::symlink(&source, &link).is_err() {
        log::info!("locale: staging {} as {}", source.display(), link.display());
        std::fs::copy(&source, &link).map_err(|e| format!("copy {}: {e}", link.display()))?;
    }
    Ok(())
}
