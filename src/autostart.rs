//! 开机自启（任务 19，docs/plan.md v2 §3 Phase 3）。
//!
//! HKCU Run 键（`Software\Microsoft\Windows\CurrentVersion\Run`）注册 `tbg-lite`
//! 值：仅当前用户、无需管理员。三个命令的底层实现：
//! - `install`：写入"当前 exe + `watch` 参数尾"命令行；重装 = 覆盖（返回旧值
//!   供输出），幂等无累积；
//! - `uninstall`：删除值（幂等：值/键不存在不算错误，明确报告"未安装"）；
//! - `status` / `read_command`：读取当前命令行。
//!
//! 生命周期注记：注册的命令以 `--duration 0`（常驻直至停止）运行 watch；自启
//! 进程的控制台窗口可见性、explorer 重启自动重应用与异常熔断属任务 20 范围
//! （plan v2 §3 Phase 3；plan v2 §6-3 常驻默认值讨论）。
//!
//! 注册表读写均为无副作用设计：`read_command` 只读；`status` 探测映射表走
//! `restoremap::status`（纯只读，不建目录、不迁移旧表——审计 BUG-02 红线：
//! 任何写 `tbg-restore.tsv` 的路径必须先持 `Local\tbg-lite.map` 互斥体）。

use windows::core::PCWSTR;
use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE,
    REG_OPTION_NON_VOLATILE, REG_SZ, REG_VALUE_TYPE,
};

/// HKCU 下 Run 子键完整路径（开机自启注册位置，无需管理员）。
pub(crate) const RUN_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

/// Run 键中的值名（与 exe 同名；单值覆盖式安装，重复 install 不累积）。
pub(crate) const VALUE_NAME: &str = "tbg-lite";

/// `install` 的结果：新注册 / 覆盖旧命令（附旧命令行供回显）。
pub(crate) enum InstallOutcome {
    New,
    Replaced(String),
}

/// `uninstall` 的结果：已删除（附被删命令行）/ 本来就没装。
pub(crate) enum UninstallOutcome {
    Removed(String),
    NotInstalled,
}

/// 打开的注册表键守卫：任何 return / Err 路径都保证 RegCloseKey。
struct KeyGuard(HKEY);

impl Drop for KeyGuard {
    fn drop(&mut self) {
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn win32_err(api: &str, code: WIN32_ERROR) -> String {
    format!("{api} failed (win32 error {})", code.0)
}

/// NUL 结尾 UTF-16（PCWSTR 输入用）。
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// 组装自启命令行：`"<exe>" <args...>`。exe 恒加引号（路径可含空格；引号
/// 本身是 Windows 路径非法字符，无需转义处理）；args 由调用方保证无需引号
/// （本工具参数均为无空格的策略名/组名/数字）。
pub(crate) fn build_command(exe: &str, args: &[&str]) -> String {
    let mut cmd = format!("\"{exe}\"");
    for a in args {
        cmd.push(' ');
        cmd.push_str(a);
    }
    cmd
}

/// 词典法路径规范化（任务 19）：去 `\\?\` / `\\?\UNC\` 前缀、折叠 `.`/`..`
/// 段。**纯字符串操作**（不查文件系统），唯一用途是把 `current_exe()` 的
/// 自报路径变成注册表里的干净命令——GetModuleFileNameW 可能保留启动路径
/// 中的 `..` 段，注册脏路径会让"命令可读性 + CI 逐字节断言"双双翻车。
/// 仅处理绝对路径（盘符 / UNC）；相对路径等异形输入原样返回；绝对路径的
/// `..` 上溢（如 `C:\..\x`）丢弃该段（防御，实际不会出现）。
pub(crate) fn normalize_win_path(path: &str) -> String {
    let stripped = if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else {
        path.strip_prefix(r"\\?\")
            .map(str::to_string)
            .unwrap_or_else(|| path.to_string())
    };
    let bytes = stripped.as_bytes();
    let is_drive_abs = bytes.len() >= 2 && bytes[1] == b':';
    let is_unc = stripped.starts_with(r"\\");
    if !is_drive_abs && !is_unc {
        return stripped; // 相对/异形：不猜，原样返回
    }
    let (root, segs_src): (String, Vec<String>) = if is_unc {
        let parts: Vec<&str> = stripped
            .trim_start_matches('\\')
            .split('\\')
            .filter(|s| !s.is_empty())
            .collect();
        if parts.len() < 3 {
            return stripped; // \\server 之类不完整 UNC：原样
        }
        (
            format!(r"\\{}\{}", parts[0], parts[1]),
            parts[2..].iter().map(|s| s.to_string()).collect(),
        )
    } else {
        (
            stripped[..2].to_string(), // "C:"
            stripped[2..]
                .split('\\')
                .filter(|s| !s.is_empty())
                .map(|s| s.to_string())
                .collect(),
        )
    };
    let mut segs: Vec<String> = Vec::new();
    for s in segs_src {
        match s.as_str() {
            "." => {}
            ".." => {
                segs.pop();
            }
            _ => segs.push(s),
        }
    }
    if segs.is_empty() {
        return root;
    }
    format!("{root}\\{}", segs.join("\\"))
}

/// REG_SZ 写入形态：UTF-16LE 字节流 + 结尾 NUL 码元（cbdata 含 NUL）。
fn reg_sz_string_to_bytes(s: &str) -> Vec<u8> {
    let mut data = Vec::with_capacity(s.len() * 2 + 2);
    for unit in s.encode_utf16().chain(std::iter::once(0)) {
        data.extend_from_slice(&unit.to_le_bytes());
    }
    data
}

/// REG_SZ 字节流 → String：按小端 u16 解码、截到首个 NUL。奇数长度（损坏
/// 数据）丢弃末字节；残缺代理对由 from_utf16_lossy 兜底为 U+FFFD，不 panic。
fn reg_sz_bytes_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    String::from_utf16_lossy(&units[..end])
}

/// 读取当前自启命令行。Run 键或值不存在 → `Ok(None)`（= 未安装）；其余
/// 注册表错误 → Err。值类型非 REG_SZ → Err（防误读其他工具的写入形态）。
pub(crate) fn read_command() -> Result<Option<String>, String> {
    unsafe {
        let subkey = wide(RUN_SUBKEY);
        let name = wide(VALUE_NAME);
        let mut hkey = HKEY::default();
        let code = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(subkey.as_ptr()),
            0,
            KEY_QUERY_VALUE,
            &mut hkey,
        );
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None); // Run 键本身不存在（极端环境）= 未安装
        }
        if code != ERROR_SUCCESS {
            return Err(win32_err("RegOpenKeyExW", code));
        }
        let _key = KeyGuard(hkey);

        // 两段式查询：第一次 lpdata=None 只取长度（文档允许的用法）
        let mut vtype = REG_VALUE_TYPE(0);
        let mut len: u32 = 0;
        let code = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(name.as_ptr()),
            None,
            Some(&mut vtype),
            None,
            Some(&mut len),
        );
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if code != ERROR_SUCCESS {
            return Err(win32_err("RegQueryValueExW(size)", code));
        }
        if vtype != REG_SZ {
            return Err(format!(
                "autostart value has unexpected registry type {} (expected REG_SZ)",
                vtype.0
            ));
        }
        if len == 0 {
            return Ok(Some(String::new()));
        }
        let mut buf = vec![0u8; len as usize];
        let code = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(name.as_ptr()),
            None,
            Some(&mut vtype),
            Some(buf.as_mut_ptr()),
            Some(&mut len),
        );
        if code != ERROR_SUCCESS {
            return Err(win32_err("RegQueryValueExW(data)", code));
        }
        Ok(Some(reg_sz_bytes_to_string(&buf)))
    }
}

/// 写入自启命令行（REG_SZ）。重装覆盖，返回旧值（若有）。
pub(crate) fn install(command: &str) -> Result<InstallOutcome, String> {
    let previous = read_command()?;
    unsafe {
        let subkey = wide(RUN_SUBKEY);
        let name = wide(VALUE_NAME);
        let data = reg_sz_string_to_bytes(command);
        let mut hkey = HKEY::default();
        // Run 键常态存在；REG_OPTION_NON_VOLATILE + create-or-open 双保险
        let code = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(subkey.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut hkey,
            None,
        );
        if code != ERROR_SUCCESS {
            return Err(win32_err("RegCreateKeyExW", code));
        }
        let _key = KeyGuard(hkey);
        let code = RegSetValueExW(
            hkey,
            PCWSTR::from_raw(name.as_ptr()),
            0,
            REG_SZ,
            Some(&data),
        );
        if code != ERROR_SUCCESS {
            return Err(win32_err("RegSetValueExW", code));
        }
    }
    Ok(match previous {
        Some(prev) => InstallOutcome::Replaced(prev),
        None => InstallOutcome::New,
    })
}

/// 删除自启项。值不存在 → `Ok(NotInstalled)`（幂等，不算错误）。
pub(crate) fn uninstall() -> Result<UninstallOutcome, String> {
    let previous = read_command()?;
    let Some(previous) = previous else {
        return Ok(UninstallOutcome::NotInstalled);
    };
    unsafe {
        let subkey = wide(RUN_SUBKEY);
        let name = wide(VALUE_NAME);
        let mut hkey = HKEY::default();
        let code = RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(subkey.as_ptr()),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        );
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(UninstallOutcome::NotInstalled);
        }
        if code != ERROR_SUCCESS {
            return Err(win32_err("RegOpenKeyExW", code));
        }
        let _key = KeyGuard(hkey);
        let code = RegDeleteValueW(hkey, PCWSTR::from_raw(name.as_ptr()));
        if code == ERROR_FILE_NOT_FOUND {
            return Ok(UninstallOutcome::NotInstalled); // 竞态窗口：读后被外部删除
        }
        if code != ERROR_SUCCESS {
            return Err(win32_err("RegDeleteValueW", code));
        }
    }
    Ok(UninstallOutcome::Removed(previous))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_command_quotes_exe_and_joins_args() {
        assert_eq!(
            build_command(r"C:\Tools\tbg-lite.exe", &["watch", "--duration", "0"]),
            "\"C:\\Tools\\tbg-lite.exe\" watch --duration 0"
        );
        // 带空格路径 + 线路二全参数尾
        assert_eq!(
            build_command(
                r"C:\Program Files\tbg\tbg-lite.exe",
                &[
                    "watch",
                    "--strategy",
                    "group",
                    "--group",
                    "work",
                    "--duration",
                    "0"
                ]
            ),
            "\"C:\\Program Files\\tbg\\tbg-lite.exe\" watch --strategy group --group work --duration 0"
        );
        // 无参数：仅带引号的 exe
        assert_eq!(build_command("a.exe", &[]), "\"a.exe\"");
    }

    #[test]
    fn wide_is_nul_terminated() {
        assert_eq!(wide("hi"), vec!['h' as u16, 'i' as u16, 0]);
        assert_eq!(wide(""), vec![0]);
        // 非 BMP 字符（增补平面）完整编码为代理对 + NUL
        assert_eq!(wide("\u{1F600}").len(), 3);
    }

    #[test]
    fn normalize_win_path_folds_and_strips() {
        // 已干净路径原样（仅去前缀）
        assert_eq!(
            normalize_win_path(r"C:\Tools\tbg-lite.exe"),
            r"C:\Tools\tbg-lite.exe"
        );
        // `..` 段折叠（GetModuleFileNameW 保留启动路径形态的场景）
        assert_eq!(
            normalize_win_path(r"D:\a\tbg\ci\..\target\release\tbg-lite.exe"),
            r"D:\a\tbg\target\release\tbg-lite.exe"
        );
        // `.` 段折叠 + 重复分隔符
        assert_eq!(
            normalize_win_path(r"D:\a\.\\b\\app.exe"),
            r"D:\a\b\app.exe"
        );
        // `\\?\` 前缀剥离
        assert_eq!(
            normalize_win_path(r"\\?\C:\x\y\tbg-lite.exe"),
            r"C:\x\y\tbg-lite.exe"
        );
        // `\\?\UNC\` → 标准 UNC
        assert_eq!(
            normalize_win_path(r"\\?\UNC\srv\share\tbg-lite.exe"),
            r"\\srv\share\tbg-lite.exe"
        );
        // UNC 的 `..` 只折叠 share 之后的部分（server/share 是根）
        assert_eq!(
            normalize_win_path(r"\\srv\share\a\..\b\app.exe"),
            r"\\srv\share\b\app.exe"
        );
        // 绝对路径 `..` 上溢：丢弃（防御）
        assert_eq!(normalize_win_path(r"C:\..\app.exe"), r"C:\app.exe");
        // 相对路径：不猜，原样返回
        assert_eq!(normalize_win_path(r"a\b\..\c"), r"a\b\..\c");
        assert_eq!(normalize_win_path("app.exe"), "app.exe");
        // 空串/单字符不 panic
        assert_eq!(normalize_win_path(""), "");
        assert_eq!(normalize_win_path("C:"), "C:");
        assert_eq!(normalize_win_path(r"C:\"), "C:");
    }

    #[test]
    fn reg_sz_roundtrip_and_nul_cut() {
        let cmd = "\"D:\\tools\\tbg-lite.exe\" watch --strategy ungroup --duration 0";
        // install 的写入形态 ↔ read 的解码形态往返
        let data = reg_sz_string_to_bytes(cmd);
        assert!(data.len() % 2 == 0);
        assert_eq!(&data[data.len() - 2..], &[0, 0]); // 结尾 NUL 码元
        assert_eq!(reg_sz_bytes_to_string(&data), cmd);
        // 注册表尾部对齐填充（多余零字节）：截到首个 NUL
        let mut padded = data.clone();
        padded.extend_from_slice(&[0, 0, 0, 0]);
        assert_eq!(reg_sz_bytes_to_string(&padded), cmd);
        // 无 NUL 的裸数据（防御路径）
        let bare: Vec<u8> = "abc".encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        assert_eq!(reg_sz_bytes_to_string(&bare), "abc");
        // 空数据 / 单 NUL
        assert_eq!(reg_sz_bytes_to_string(&[]), "");
        assert_eq!(reg_sz_bytes_to_string(&[0, 0]), "");
        // 奇数字节：末尾不完整码元丢弃
        assert_eq!(reg_sz_bytes_to_string(&[b'a', 0, b'b']), "a");
        // 残缺代理对 → U+FFFD（lossy 兜底，不 panic）
        assert_eq!(reg_sz_bytes_to_string(&[0x00, 0xD8]), "\u{FFFD}");
    }
}
