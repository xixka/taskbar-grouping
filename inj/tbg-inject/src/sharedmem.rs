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
    PAGE_READWRITE,
};

use tbg_proto::{SharedState, MAGIC, PROTO_VERSION, SECTION_NAME, SHARED_SIZE};

/// 宿主持有的共享节视图（Drop 时解除映射；节本体由各映射视图保活）。
pub(crate) struct SharedView {
    _h: HANDLE,
    ptr: *mut SharedState,
}

// 仅主线程按序使用（CLI 单命令进程 / 菜单单线程派发）。
unsafe impl Send for SharedView {}
unsafe impl Sync for SharedView {}

impl SharedView {
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
        let p = MapViewOfFile(h, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 0)
            .map_err(|e| format!("inject: MapViewOfFile failed: {e}"))? as *mut SharedState;
        if (*p).magic != MAGIC || (*p).proto != PROTO_VERSION {
            // 页支持节零初始化：视为全新（或旧协议）→ 写默认态
            ptr::write(p, SharedState::new_default());
        }
        Ok(Self { _h: h, ptr: p })
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
            let _ = UnmapViewOfFile(self.ptr as *const c_void);
            let _ = CloseHandle(self._h);
        }
    }
}
