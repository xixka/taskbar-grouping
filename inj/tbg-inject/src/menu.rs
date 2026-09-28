//! 注入版交互菜单（任务 35）。
//!
//! 模式对齐主版任务 14/29：无参数启动进入菜单；退出走 `[0]`（无
//! Ctrl+C）；`L` 切换语言；stdin 首行 UTF-8 BOM 容错（CI run
//! 35698563610 同款教训）；stdin EOF 视同 `[0]`（默认保持注入）。
//! ZH 横幅保留英文子串 `interactive menu`（CI 断言双保险，任务 29
//! 同口径）。

use std::io::{self, BufRead, Write};

use windows::Win32::Globalization::GetUserDefaultUILanguage;

use tbg_proto::STATE_ACTIVE;

use crate::{do_inject, do_status, do_stop};

#[derive(Clone, Copy, PartialEq)]
enum Lang {
    Zh,
    En,
}

impl Lang {
    fn zh(self) -> bool {
        matches!(self, Lang::Zh)
    }
}

fn detect_lang() -> Lang {
    // 主语言 ID == 0x04（中文）→ ZH；否则 EN（任务 29 同策略）
    if unsafe { GetUserDefaultUILanguage() } & 0x3FF == 0x04 {
        Lang::Zh
    } else {
        Lang::En
    }
}

/// 菜单主循环，返回进程退出码。
pub(crate) fn run() -> i32 {
    let mut lang = detect_lang();
    let stdin = io::stdin();
    let mut out = io::stdout();
    loop {
        let active = hook_active();
        let ver = env!("CARGO_PKG_VERSION");
        let banner = if lang.zh() {
            format!(
                "tbg-inject {ver} — 交互菜单 interactive menu（注入版 route A）\n\
                 提示：`tbg-inject --help` 查看 CLI 用法；用 [0] 退出\n\
                 \n\
                 hook 状态：{}\n\
                 [1] 注入——取消分组（线路一等价：任务栏不合并按钮）\n\
                 [2] 注入——自定义分组（输入组名）\n\
                 [3] 停止注入（摘钩 + 卸载 + 统计）\n\
                 [4] 状态（拦截计数 / 补丁模块）\n\
                 [0] 退出（注入运行中：[y] 先停止 / [n] 保持注入）\n\
                 语言：中文——按 L 切换 English\n\
                 > ",
                state_word_zh(active)
            )
        } else {
            format!(
                "tbg-inject {ver} — interactive menu (injection edition, route A)\n\
                 tip: `tbg-inject --help` for CLI usage; exit with [0]\n\
                 \n\
                 hook state: {}\n\
                 [1] inject — ungroup (taskbar never combines buttons)\n\
                 [2] inject — custom group (enter group name)\n\
                 [3] stop inject (unhook + unload + stats)\n\
                 [4] status (interception counters / patched modules)\n\
                 [0] exit (while active: [y] stop first / [n] keep injected)\n\
                 language: English — press L for 中文\n\
                 > ",
                state_word_en(active)
            )
        };
        let _ = out.write_all(banner.as_bytes());
        let _ = out.flush();

        let Some(line) = read_line(&stdin) else {
            return 0; // EOF：视同 [0]（默认保持注入）
        };
        let cmd = strip_bom(&line).trim().to_string();
        match cmd.as_str() {
            "1" => report(do_inject(None), lang),
            "2" => {
                let prompt = if lang.zh() {
                    "组名（[A-Za-z0-9._-]，1..=32）："
                } else {
                    "group name ([A-Za-z0-9._-], 1..=32): "
                };
                println!("{prompt}");
                let _ = out.flush();
                if let Some(name_line) = read_line(&stdin) {
                    let name = strip_bom(&name_line).trim().to_string();
                    if name.is_empty() {
                        println!("{}", if lang.zh() { "（已取消）" } else { "(cancelled)" });
                    } else {
                        report(do_inject(Some(&name)), lang);
                    }
                } else {
                    return 0;
                }
            }
            "3" => report(do_stop(), lang),
            "4" => {
                print!("{}", do_status());
                let _ = out.flush();
            }
            "0" => {
                if hook_active() {
                    let q = if lang.zh() {
                        "注入运行中——先停止？([y] 停止 / [n] 保持注入)："
                    } else {
                        "hook is active — stop first? ([y] stop / [n] keep): "
                    };
                    println!("{q}");
                    let _ = out.flush();
                    if let Some(ans) = read_line(&stdin) {
                        if is_yes(&ans) {
                            report(do_stop(), lang);
                        } else {
                            println!(
                                "{}",
                                if lang.zh() {
                                    "（保持注入，后台生效）"
                                } else {
                                    "(staying injected)"
                                }
                            );
                        }
                    }
                }
                return 0;
            }
            "L" | "l" => {
                lang = if lang.zh() { Lang::En } else { Lang::Zh };
            }
            "" => {}
            other => {
                println!(
                    "{}",
                    if lang.zh() {
                        format!("（未知指令 {other:?}——输入数字或 L）")
                    } else {
                        format!("(unknown command {other:?} — digits or L)")
                    }
                );
            }
        }
    }
}

fn state_word_zh(active: bool) -> String {
    if active {
        "运行中（已注入 explorer）".into()
    } else {
        "未运行".into()
    }
}

fn state_word_en(active: bool) -> String {
    if active {
        "running (injected into explorer)".into()
    } else {
        "not running".into()
    }
}

fn report(r: Result<String, String>, lang: Lang) {
    match r {
        Ok(msg) => println!("{msg}"),
        Err(e) => println!(
            "{}",
            if lang.zh() { format!("失败：{e}") } else { format!("failed: {e}") }
        ),
    }
}

fn hook_active() -> bool {
    unsafe {
        crate::sharedmem::SharedView::ensure()
            .map(|v| v.as_ref().state.load(std::sync::atomic::Ordering::Acquire) == STATE_ACTIVE)
            .unwrap_or(false)
    }
}

fn read_line(stdin: &io::Stdin) -> Option<String> {
    let mut s = String::new();
    match stdin.lock().read_line(&mut s) {
        Ok(0) => None, // EOF
        Ok(_) => Some(s),
        Err(_) => None,
    }
}

/// 首行 BOM 容错（CI run 35698563610：PS StandardInput 首写前置 U+FEFF）。
fn strip_bom(s: &str) -> &str {
    s.strip_prefix('\u{feff}').unwrap_or(s)
}

fn is_yes(s: &str) -> bool {
    let t = s.trim();
    t.eq_ignore_ascii_case("y") || t.eq_ignore_ascii_case("yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yes_variants() {
        assert!(is_yes("y"));
        assert!(is_yes("Y"));
        assert!(is_yes("yes\n"));
        assert!(is_yes(" YES "));
        assert!(!is_yes(""));
        assert!(!is_yes("n"));
        assert!(!is_yes("yeah"));
    }

    #[test]
    fn bom_stripped() {
        assert_eq!(strip_bom("\u{feff}1"), "1");
        assert_eq!(strip_bom("1"), "1");
        assert_eq!(strip_bom(""), "");
        assert_eq!(strip_bom("a\u{feff}b"), "a\u{feff}b"); // 只剥行首
    }
}
