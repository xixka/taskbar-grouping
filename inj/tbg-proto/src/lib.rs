//! 路线 A 双进程共享协议（任务 34，docs/plan.md v2 SS5）。
//!
//! 宿主 `tbg-inject.exe` 创建页支持文件映射节（`Local\tbg_lite_inject_v1`），
//! 写入配置；explorer 内的 `tbg_hook.dll` 打开同一节，读配置、写状态与
//! 统计。**零依赖纯数据定义**——两侧（cdylib / bin）各自以相同 crate
//! 版本编译，布局一致性由同一源码保证。
//!
//! 同步口径：可变字段一律 `AtomicU32`。跨进程映射上 x86-TSO 的原子
//! load/store 即普通 32 位访存，缓存一致性保证可见性；字段分域
//! "单写者"（配置 = 宿主写，状态/统计 = DLL 写），宿主轮询读取，
//! 无跨进程锁。`group_aumid` 仅在注入前由宿主一次性写入，此后不变
//! （远程线程创建边界提供实际序）。

use std::sync::atomic::AtomicU32;

/// 共享内存节名（`Local\` = 当前登录会话命名空间，与
/// singleinstance.rs 的互斥体同前缀约定）。
pub const SECTION_NAME: &str = "Local\\tbg_lite_inject_v1";

/// 协议魔数 "TBG1"。
pub const MAGIC: u32 = 0x5442_4731;
/// 协议版本（布局不兼容演进时递增，init 侧拒载旧节）。
pub const PROTO_VERSION: u32 = 1;
/// 节大小：一个 4 KiB 页（`SharedState` 必须可整装其中）。
pub const SHARED_SIZE: usize = 4096;

/// 策略：线路一等价（每窗口 `~TBG~w<HWND>` 后缀，取消分组）。
pub const MODE_UNGROUP: u32 = 0;
/// 策略：线路二等价（统一共享 AUMID `TBG.Group.<NAME>`）。
pub const MODE_GROUP: u32 = 1;

/// DLL 状态机（`state` 字段取值）。
pub const STATE_DETACHED: u32 = 0; // 未注入 / 已卸载并复位
pub const STATE_INITING: u32 = 1;
pub const STATE_ACTIVE: u32 = 2;
pub const STATE_STOPPING: u32 = 3;
pub const STATE_UNLOADED: u32 = 4;
pub const STATE_ERROR: u32 = 5;

/// `tbg_hook_init` 远程线程退出码。
pub const INIT_OK: u32 = 0;
pub const INIT_ALREADY: u32 = 1; // 同一 explorer 内重复 init（幂等拒绝）
pub const INIT_NO_SECTION: u32 = 2; // 找不到共享节（宿主未创建）
pub const INIT_BAD_PROTO: u32 = 3; // magic / 版本不符
pub const INIT_HOOK_FAIL: u32 = 4; // IAT 补丁失败（err 字段有细分码）

/// `tbg_hook_stop` 远程线程退出码（FreeLibraryAndExitThread 的退出码）。
pub const STOP_OK: u32 = 0;
pub const STOP_NOT_ACTIVE: u32 = 1;

/// `tbg_hook_init` 失败细分码（`err` 字段）。
pub const ERR_NONE: u32 = 0;
pub const ERR_SNAPSHOT: u32 = 1; // Toolhelp 模块快照失败
pub const ERR_NO_SLOTS: u32 = 2; // 全进程无 SHGetPropertyStoreForWindow 导入槽

/// 每条补丁记录：模块名（ASCII 截断）+ 槽位数。
#[repr(C)]
pub struct PatchedModule {
    /// 模块名（如 `Taskbar.dll`），NUL 结尾，超长截断。
    pub name: [u8; 24],
    /// 该模块内被重定向的 IAT 槽数。
    pub slots: u32,
}

/// 双向共享状态。
///
/// 字段分组见模块注释；`#[repr(C)]` + 两侧同源编译保证布局一致。
#[repr(C)]
pub struct SharedState {
    // --- 协议头（宿主创建节时一次写入） ---
    pub magic: u32,
    pub proto: u32,
    /// sizeof(SharedState)，init 侧交叉校验。
    pub size: u32,

    // --- 配置（宿主写） ---
    /// 0 = 停止改写（摘钩前先置 0，让在途读快速回原值）；1 = 生效。
    pub enabled: AtomicU32,
    /// MODE_UNGROUP / MODE_GROUP。
    pub mode: u32,
    /// MODE_GROUP 的共享 AUMID（NUL 结尾宽字符串，注入前写定）。
    pub group_aumid: [u16; 48],

    // --- 状态与统计（DLL 写；宿主只读） ---
    pub state: AtomicU32,
    /// ERR_* 细分码。
    pub err: u32,
    /// 目标 explorer 进程 ID（宿主注入时写；DLL init 用本进程 PID 复核）。
    pub explorer_pid: AtomicU32,
    /// DLL 自报加载基址（宿主无需从 32 位线程退出码反解截断的 HMODULE）。
    pub self_module: usize,
    /// 注入代数（每次 init +1；explorer 重启后重新注入递增）。
    pub generation: AtomicU32,
    /// SHGetPropertyStoreForWindow 拦截次数。
    pub calls_seen: AtomicU32,
    /// 包装成委托对象的属性存储个数。
    pub stores_wrapped: AtomicU32,
    /// GetValue(PKEY_AppUserModel_ID) 实际改写次数。
    pub aumid_served: AtomicU32,
    /// 扫描的模块总数。
    pub modules_scanned: u32,
    /// 重定向的 IAT 槽总数。
    pub slots_patched: u32,
    /// 补丁明细（前 12 个命中的模块）。
    pub patched: [PatchedModule; 12],
}

impl SharedState {
    /// 全零 + 协议头的初始态（宿主首次创建节时写入）。
    pub fn new_default() -> Self {
        Self {
            magic: MAGIC,
            proto: PROTO_VERSION,
            size: std::mem::size_of::<SharedState>() as u32,
            enabled: AtomicU32::new(0),
            mode: MODE_UNGROUP,
            group_aumid: [0; 48],
            state: AtomicU32::new(STATE_DETACHED),
            err: ERR_NONE,
            explorer_pid: AtomicU32::new(0),
            self_module: 0,
            generation: AtomicU32::new(0),
            calls_seen: AtomicU32::new(0),
            stores_wrapped: AtomicU32::new(0),
            aumid_served: AtomicU32::new(0),
            modules_scanned: 0,
            slots_patched: 0,
            patched: [PatchedModule {
                name: [0; 24],
                slots: 0,
            }; 12],
        }
    }

    /// `group_aumid`（NUL 截断宽字符串）→ String。
    pub fn group_aumid_string(&self) -> String {
        let end = self
            .group_aumid
            .iter()
            .position(|&u| u == 0)
            .unwrap_or(self.group_aumid.len());
        String::from_utf16_lossy(&self.group_aumid[..end])
    }

    /// 状态码 → 展示名（宿主 status / 菜单共用）。
    pub fn state_name(&self) -> &'static str {
        match self.state.load(std::sync::atomic::Ordering::Acquire) {
            STATE_INITING => "initing",
            STATE_ACTIVE => "active",
            STATE_STOPPING => "stopping",
            STATE_UNLOADED => "unloaded",
            STATE_ERROR => "error",
            _ => "detached",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn layout_fits_single_page() {
        // 节大小按一页申请；结构体必须整装其中（否则 init 侧 size 校验
        // 与宿主写入都会越界）。
        assert!(size_of::<SharedState>() <= SHARED_SIZE);
        assert!(size_of::<SharedState>() > 256); // 防意外退化成空壳
    }

    #[test]
    fn default_writes_protocol_header() {
        let s = SharedState::new_default();
        assert_eq!(s.magic, MAGIC);
        assert_eq!(s.proto, PROTO_VERSION);
        assert_eq!(s.size, size_of::<SharedState>() as u32);
        assert_eq!(s.state_name(), "detached");
        assert_eq!(s.mode, MODE_UNGROUP);
        assert_eq!(s.group_aumid_string(), "");
    }

    #[test]
    fn group_aumid_roundtrip() {
        let mut s = SharedState::new_default();
        let wide: Vec<u16> = "TBG.Group.smoke".encode_utf16().collect();
        s.group_aumid[..wide.len()].copy_from_slice(&wide);
        assert_eq!(s.group_aumid_string(), "TBG.Group.smoke");
        // 无 NUL 时取全 48 码元（防御路径，不 panic）
        s.group_aumid = [b'x' as u16; 48];
        assert_eq!(s.group_aumid_string().len(), 48);
    }

    #[test]
    fn state_names() {
        let mut s = SharedState::new_default();
        for (v, name) in [
            (STATE_INITING, "initing"),
            (STATE_ACTIVE, "active"),
            (STATE_STOPPING, "stopping"),
            (STATE_UNLOADED, "unloaded"),
            (STATE_ERROR, "error"),
            (STATE_DETACHED, "detached"),
            (99, "detached"),
        ] {
            s.state = AtomicU32::new(v);
            assert_eq!(s.state_name(), name);
        }
    }
}
