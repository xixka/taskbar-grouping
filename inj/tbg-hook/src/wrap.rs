//! COM 委托属性存储（任务 34，docs/plan.md v2 §5）。
//!
//! 手写最小 `IPropertyStore` 委托实现（不使用 windows crate 的
//! `#[implement]` 宏——cdylib 内零宏魔法、零隐式生成面，全部代码
//! 可读可审计）。对象持有 inner 接口的一个自有引用；`GetValue`
//! 仅对 `PKEY_AppUserModel_ID` 按策略改写返回值，其余键与
//! `SetValue` / `Commit` 逐字透传 inner。
//!
//! 接口语义依据（公开文档）：
//! - IUnknown 三方法 + IPropertyStore 五方法（GetCount/GetAt/GetValue/
//!   SetValue/Commit）的 vtable 次序；
//! - out 参数 `PROPVARIANT` 的所有权归调用方（调用方负责
//! PropVariantClear）——改写路径用 `ptr::write` 整体移交新值，
//! 透传路径位搬运 + forget 避免双重释放；
//! - `QueryInterface` 仅应答 IUnknown / IPropertyStore（其余返回
//! E_NOINTERFACE——文档化限制：任务栏以 riid=IPropertyStore 取得
//! 接口后极少再 QI 其他接口）。

use std::ffi::c_void;
use std::ptr;
use std::sync::atomic::{AtomicU32, Ordering};

// windows 0.58 实源核对：E_*/S_OK 错误码在 Win32::Foundation（非 core），
// PROPERTYKEY 在 PropertiesSystem，IID 关联常量需 Interface trait 在作用域。
use windows::core::{BSTR, GUID, HRESULT, IUnknown, Interface, PROPVARIANT};
use windows::Win32::Foundation::{E_NOINTERFACE, E_POINTER, S_OK, HWND};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::UI::Shell::PropertiesSystem::{
    IPropertyStore, IPropertyStoreCache, PSC_STATE, PROPERTYKEY,
};

use tbg_proto::{SharedState, STATE_ACTIVE};

use crate::filter;

/// 委托对象（COM 对象本体；lpVtbl 在偏移 0）。
#[repr(C)]
struct ProxyStore {
    vtbl: *const ProxyVtbl,
    refs: AtomicU32,
    /// 被委托的真实 IPropertyStore 接口指针（自有 1 引用）。
    inner: *mut c_void,
    /// inner 经其自身 QI 取得的 IPropertyStoreCache 指针（QI 自带 1 引用；
    /// null = inner 不实现该接口，代理亦不对外宣告）。cache 槽位委托至此，
    /// 而非 inner store 指针——接口层级指针不保证同一。
    inner_cache: *mut c_void,
    /// 请求该存储的窗口（线路一后缀用）。
    hwnd: isize,
    /// 共享状态（策略 + 统计；指向宿主节映射，DLL 生命周期内有效）。
    shared: *const SharedState,
}

/// IPropertyStore 的 COM vtable 次序（对照 windows 0.58
/// PropertiesSystem 生成源码）：IUnknown(3) + GetCount + GetAt +
/// GetValue + SetValue + Commit。**GetCount/GetAt 槽位必须存在**——
/// 任务栏枚举属性时会调用它们，缺失会导致以错位参数跳进 GetValue。
/// 任务 36 修复轮 6：扩展至 IPropertyStoreCache（propsys 存储实际实现
/// 的超集接口；run 36532473377 实测 wrapped=2 而 GetValue 从未被调——
/// 任务栏 QI IPropertyStoreCache 遭 E_NOINTERFACE 拒绝后放弃读取）。
/// 槽序（0.58 生成源码逐字对照）：GetState + GetValueAndState +
/// SetState + SetValueAndState。
#[repr(C)]
struct ProxyVtbl {
    query_interface:
        unsafe extern "system" fn(*mut ProxyStore, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut ProxyStore) -> u32,
    release: unsafe extern "system" fn(*mut ProxyStore) -> u32,
    get_count: unsafe extern "system" fn(*mut ProxyStore, *mut u32) -> HRESULT,
    get_at:
        unsafe extern "system" fn(*mut ProxyStore, u32, *mut PROPERTYKEY) -> HRESULT,
    get_value:
        unsafe extern "system" fn(*mut ProxyStore, *const PROPERTYKEY, *mut PROPVARIANT) -> HRESULT,
    set_value:
        unsafe extern "system" fn(*mut ProxyStore, *const PROPERTYKEY, *const PROPVARIANT) -> HRESULT,
    commit: unsafe extern "system" fn(*mut ProxyStore) -> HRESULT,
    get_state:
        unsafe extern "system" fn(*mut ProxyStore, *const PROPERTYKEY, *mut PSC_STATE) -> HRESULT,
    get_value_and_state: unsafe extern "system" fn(
        *mut ProxyStore,
        *const PROPERTYKEY,
        *mut PROPVARIANT,
        *mut PSC_STATE,
    ) -> HRESULT,
    set_state:
        unsafe extern "system" fn(*mut ProxyStore, *const PROPERTYKEY, PSC_STATE) -> HRESULT,
    set_value_and_state: unsafe extern "system" fn(
        *mut ProxyStore,
        *const PROPERTYKEY,
        *const PROPVARIANT,
        PSC_STATE,
    ) -> HRESULT,
}

/// inner 接口的原始 vtable 视图（调用被委托实现，不经 windows crate
/// 生成签名——原始 COM 原型零歧义）。
#[repr(C)]
struct InnerVtbl {
    query_interface:
        unsafe extern "system" fn(*mut c_void, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut c_void) -> u32,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    get_count: unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT,
    get_at: unsafe extern "system" fn(*mut c_void, u32, *mut PROPERTYKEY) -> HRESULT,
    get_value:
        unsafe extern "system" fn(*mut c_void, *const PROPERTYKEY, *mut PROPVARIANT) -> HRESULT,
    set_value:
        unsafe extern "system" fn(*mut c_void, *const PROPERTYKEY, *const PROPVARIANT) -> HRESULT,
    commit: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    get_state:
        unsafe extern "system" fn(*mut c_void, *const PROPERTYKEY, *mut PSC_STATE) -> HRESULT,
    get_value_and_state: unsafe extern "system" fn(
        *mut c_void,
        *const PROPERTYKEY,
        *mut PROPVARIANT,
        *mut PSC_STATE,
    ) -> HRESULT,
    set_state:
        unsafe extern "system" fn(*mut c_void, *const PROPERTYKEY, PSC_STATE) -> HRESULT,
    set_value_and_state: unsafe extern "system" fn(
        *mut c_void,
        *const PROPERTYKEY,
        *const PROPVARIANT,
        PSC_STATE,
    ) -> HRESULT,
    // IPropertyStoreCache 之后的方法槽不透传（见 QI 策略），不声明。
}

static PROXY_VTBL: ProxyVtbl = ProxyVtbl {
    query_interface: proxy_query_interface,
    add_ref: proxy_add_ref,
    release: proxy_release,
    get_count: proxy_get_count,
    get_at: proxy_get_at,
    get_value: proxy_get_value,
    set_value: proxy_set_value,
    commit: proxy_commit,
    get_state: proxy_get_state,
    get_value_and_state: proxy_get_value_and_state,
    set_state: proxy_set_state,
    set_value_and_state: proxy_set_value_and_state,
};

/// 构造委托对象：对 inner 自持一个引用，返回 COM 指针（refs = 1）。
/// 同时探测 inner 的 IPropertyStoreCache 能力（经其自身 QI；成功则
/// 缓存该接口指针供 cache 槽位委托）。
pub(crate) unsafe fn proxy_new(
    inner: *mut c_void,
    hwnd: HWND,
    shared: *const SharedState,
) -> *mut c_void {
    let vt = inner as *mut *const InnerVtbl;
    ((*(*vt)).add_ref)(inner); // 自持一个 inner 引用
    let mut inner_cache: *mut c_void = std::ptr::null_mut();
    let _ = ((*(*vt)).query_interface)(
        inner,
        &IPropertyStoreCache::IID,
        &mut inner_cache as *mut *mut c_void,
    ); // 成功时 QI 自带 1 引用；失败保持 null
    Box::into_raw(Box::new(ProxyStore {
        vtbl: &PROXY_VTBL,
        refs: AtomicU32::new(1),
        inner,
        inner_cache,
        hwnd: hwnd.0 as isize,
        shared,
    })) as *mut c_void
}

unsafe extern "system" fn proxy_query_interface(
    this: *mut ProxyStore,
    iid: *const GUID,
    out: *mut *mut c_void,
) -> HRESULT {
    if iid.is_null() || out.is_null() {
        return E_POINTER;
    }
    if *iid == IPropertyStore::IID || *iid == IUnknown::IID {
        proxy_add_ref(this);
        *out = this as *mut c_void;
        S_OK
    } else if *iid == IPropertyStoreCache::IID && !(*this).inner_cache.is_null() {
        // 仅当 inner 真实实现该接口（构造时 QI 探测成功）才宣告
        proxy_add_ref(this);
        *out = this as *mut c_void;
        S_OK
    } else {
        *out = ptr::null_mut();
        E_NOINTERFACE
    }
}

unsafe extern "system" fn proxy_add_ref(this: *mut ProxyStore) -> u32 {
    let me = &*this;
    me.refs.fetch_add(1, Ordering::Relaxed) + 1
}

unsafe extern "system" fn proxy_release(this: *mut ProxyStore) -> u32 {
    let me = &*this;
    let left = me.refs.fetch_sub(1, Ordering::AcqRel);
    if left == 1 {
        // 释放 inner 的自有引用（store 与 cache 各一），再销毁自身
        if !me.inner_cache.is_null() {
            let vtc = me.inner_cache as *mut *const InnerVtbl;
            ((*(*vtc)).release)(me.inner_cache);
        }
        let vt = me.inner as *mut *const InnerVtbl;
        ((*(*vt)).release)(me.inner);
        drop(Box::from_raw(this));
        0
    } else {
        left - 1
    }
}

/// GetCount 透传（raw 原型：this + out u32 → HRESULT）。
unsafe extern "system" fn proxy_get_count(this: *mut ProxyStore, out: *mut u32) -> HRESULT {
    let me = &*this;
    if out.is_null() {
        return E_POINTER;
    }
    let vt = me.inner as *mut *const InnerVtbl;
    ((*(*vt)).get_count)(me.inner, out)
}

/// GetAt 透传（raw 原型：this + 索引 + out PROPERTYKEY → HRESULT）。
unsafe extern "system" fn proxy_get_at(
    this: *mut ProxyStore,
    index: u32,
    pkey: *mut PROPERTYKEY,
) -> HRESULT {
    let me = &*this;
    if pkey.is_null() {
        return E_POINTER;
    }
    let vt = me.inner as *mut *const InnerVtbl;
    ((*(*vt)).get_at)(me.inner, index, pkey)
}

unsafe extern "system" fn proxy_get_value(
    this: *mut ProxyStore,
    key: *const PROPERTYKEY,
    out: *mut PROPVARIANT,
) -> HRESULT {
    let me = &*this;
    if key.is_null() || out.is_null() {
        return E_POINTER;
    }
    let vt = me.inner as *mut *const InnerVtbl;
    let mut inner_pv = PROPVARIANT::new();
    let hr = ((*(*vt)).get_value)(me.inner, key, &mut inner_pv);
    if !hr.is_ok() {
        return hr;
    }
    if *key == PKEY_AppUserModel_ID {
        let s = &*me.shared;
        if s.enabled.load(Ordering::Acquire) == 1
            && s.state.load(Ordering::Acquire) == STATE_ACTIVE
        {
            // 原值 → 策略改写（线路一后缀 / 线路二共享 AUMID）
            let orig: String = BSTR::try_from(&inner_pv)
                .map(|b| b.to_string())
                .unwrap_or_default();
            let modified = filter::rewrite_aumid(&orig, me.hwnd, s);
            let new_pv = PROPVARIANT::from(modified.as_str());
            s.aumid_served.fetch_add(1, Ordering::Relaxed);
            drop(inner_pv); // inner 值就此释放（Drop = PropVariantClear）
            ptr::write(out, new_pv); // 所有权移交调用方
            return S_OK;
        }
    }
    // 透传：位搬运 + forget（值的所有权从 inner 转给调用方，不双重释放）
    ptr::write(out, inner_pv);
    hr
}

unsafe extern "system" fn proxy_set_value(
    this: *mut ProxyStore,
    key: *const PROPERTYKEY,
    val: *const PROPVARIANT,
) -> HRESULT {
    let me = &*this;
    if key.is_null() || val.is_null() {
        return E_POINTER;
    }
    let vt = me.inner as *mut *const InnerVtbl;
    ((*(*vt)).set_value)(me.inner, key, val)
}

unsafe extern "system" fn proxy_commit(this: *mut ProxyStore) -> HRESULT {
    let me = &*this;
    let vt = me.inner as *mut *const InnerVtbl;
    ((*(*vt)).commit)(me.inner)
}

// ---------------- IPropertyStoreCache 扩展（任务 36 修复轮 6） ----------------

/// GetState 透传。
unsafe extern "system" fn proxy_get_state(
    this: *mut ProxyStore,
    key: *const PROPERTYKEY,
    state: *mut PSC_STATE,
) -> HRESULT {
    let me = &*this;
    if key.is_null() || state.is_null() {
        return E_POINTER;
    }
    if me.inner_cache.is_null() {
        return E_NOINTERFACE;
    }
    let vt = me.inner_cache as *mut *const InnerVtbl;
    ((*(*vt)).get_state)(me.inner_cache, key, state)
}

/// GetValueAndState：值路径与 GetValue 同一改写语义（任务栏经 cache
/// 接口读 AUMID 的路径），state 原样透传。
unsafe extern "system" fn proxy_get_value_and_state(
    this: *mut ProxyStore,
    key: *const PROPERTYKEY,
    ppropvar: *mut PROPVARIANT,
    pstate: *mut PSC_STATE,
) -> HRESULT {
    let me = &*this;
    if key.is_null() || ppropvar.is_null() || pstate.is_null() {
        return E_POINTER;
    }
    if me.inner_cache.is_null() {
        return E_NOINTERFACE;
    }
    let vt = me.inner_cache as *mut *const InnerVtbl;
    let hr = ((*(*vt)).get_value_and_state)(me.inner_cache, key, ppropvar, pstate);
    if hr.is_ok() && *key == PKEY_AppUserModel_ID {
        let s = &*me.shared;
        if s.enabled.load(Ordering::Acquire) == 1
            && s.state.load(Ordering::Acquire) == STATE_ACTIVE
        {
            // inner 已把值写入调用方缓冲；就地读出 → 改写 → 归还所有权
            let inner_pv = ptr::read(ppropvar);
            let orig: String = BSTR::try_from(&inner_pv)
                .map(|b| b.to_string())
                .unwrap_or_default();
            let modified = filter::rewrite_aumid(&orig, me.hwnd, s);
            let new_pv = PROPVARIANT::from(modified.as_str());
            s.aumid_served.fetch_add(1, Ordering::Relaxed);
            drop(inner_pv);
            ptr::write(ppropvar, new_pv);
            return S_OK;
        }
    }
    hr
}

/// SetState 透传。
unsafe extern "system" fn proxy_set_state(
    this: *mut ProxyStore,
    key: *const PROPERTYKEY,
    state: PSC_STATE,
) -> HRESULT {
    let me = &*this;
    if key.is_null() {
        return E_POINTER;
    }
    if me.inner_cache.is_null() {
        return E_NOINTERFACE;
    }
    let vt = me.inner_cache as *mut *const InnerVtbl;
    ((*(*vt)).set_state)(me.inner_cache, key, state)
}

/// SetValueAndState 透传（路线 A 永不写真实属性）。
unsafe extern "system" fn proxy_set_value_and_state(
    this: *mut ProxyStore,
    key: *const PROPERTYKEY,
    val: *const PROPVARIANT,
    state: PSC_STATE,
) -> HRESULT {
    let me = &*this;
    if key.is_null() || val.is_null() {
        return E_POINTER;
    }
    if me.inner_cache.is_null() {
        return E_NOINTERFACE;
    }
    let vt = me.inner_cache as *mut *const InnerVtbl;
    ((*(*vt)).set_value_and_state)(me.inner_cache, key, val, state)
}
