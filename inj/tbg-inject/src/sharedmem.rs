//! 共享内存宿主侧（任务 35）。
//!
//! `inject` / `status` / `stop` 打开（或创建）`Local\tbg_lite_inject_v1`
//! 页支持节：首次创建时写入协议头与默认态；已存在（explorer 内 DLL
//! 仍映射着）则沿用其数据——节对象在宿主退出后仍由 explorer 侧视图
//! 保活，注入效果不随宿主生命周期消失。

use std::ffi::c_void;
use std::ptr;

use windows::core::HSTRING;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::System::Memory::{
    CreateFileMappingW, MapViewOfFile, UnmapViewOfFile, FILE_MAP_READ, FILE_MAP_WRITE,
    MEMORY_MAPPED_VIEW_ADDRESS, PAGE_READWRITE,
};

use tbg_proto::{SharedState, MAGIC, PROTO_VERSION, SECTION_NAME, SHARED_SIZE};

/// 宿主持有的共享节视图（Drop 时解除映射；节本体由各映射视图保活）。
pub(crate) struct SharedView {
    _h: HANDLE,
    ptr: *mut SharedState,
    /// 本次 ensure() 打开的是否为已存在的节（false = 本次新建）。
    /// 诊断用：注入失败时可区分"宿主新建节"与"复用 explorer 侧节"。
    pub(crate) existed: bool,
}

// 仅主线程按序使用（CLI 单命令进程 / 菜单单线程派发）。
unsafe impl Send for SharedView {}
unsafe impl Sync for SharedView {}

impl SharedView {
    /// 判别"新建 / 已存在"：CreateFileMappingW 语义——句柄有效但
    /// GetLastError == ERROR_ALREADY_EXISTS 表示节早已存在（explorer 侧
    /// DLL 或上一个宿主创建）。须在后续任何 Win32 调用前立即取走。
    fn section_existed() -> bool {
        const ERROR_ALREADY_EXISTS: u32 = 183;
        unsafe { windows::Win32::Foundation::GetLastError().0 == ERROR_ALREADY_EXISTS }
    }

    /// 打开或创建共享节（不存在则初始化协议头）。
    pub(crate) unsafe fn ensure() -> Result<Self, String> {
        let name = HSTRING::from(SECTION_NAME);
        let h = CreateFileMappingW(
            INVALID_HANDLE_VALUE,
            None,
            PAGE_READWRITE,
            0,
            SHARED_SIZE as u32,
            &name,
        )
        .map_err(|e| format!("inject: CreateFileMappingW({SECTION_NAME}) failed: {e}"))?;
        let existed = Self::section_existed();
        // 0.58 签名：MapViewOfFile 返回 MEMORY_MAPPED_VIEW_ADDRESS（失败 .Value 为 null）
        let view = MapViewOfFile(h, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 0);
        if view.Value.is_null() {
            let _ = CloseHandle(h);
            return Err("inject: MapViewOfFile failed".into());
        }
        let p = view.Value as *mut SharedState;
        if (*p).magic != MAGIC || (*p).proto != PROTO_VERSION {
            // 页支持节零初始化：视为全新（或旧协议）→ 写默认态
            ptr::write(p, SharedState::new_default());
        }
        Ok(Self {
            _h: h,
            ptr: p,
            existed,
        })
    }

    pub(crate) fn as_ref(&self) -> &SharedState {
        unsafe { &*self.ptr }
    }

    pub(crate) fn as_mut(&self) -> &mut SharedState {
        unsafe { &mut *self.ptr }
    }
}

impl Drop for SharedView {
    fn drop(&mut self) {
        unsafe {
            // 0.58 签名：UnmapViewOfFile 取视图结构体（非裸指针）
            let _ = UnmapViewOfFile(MEMORY_MAPPED_VIEW_ADDRESS {
                Value: self.ptr as *mut c_void,
            });
            let _ = CloseHandle(self._h);
        }
    }
}
