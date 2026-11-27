//! Jump lists (OpenTaskbar's `ApplicationJumpListService` and `NativeJumpListSession`): the pinned, frequent and
//! recent destinations and the custom categories and tasks an app publishes for the taskbar, read from the Shell
//! and shown at the top of the app's context menu; choosing one invokes it the way the taskbar does.
//!
//! Windows has no public API to read another app's jump list. Like OpenTaskbar (after Open-Shell's JumpLists.cpp and
//! System Informer's appresolverp.h) this calls the Shell's application resolver, destination list and automatic
//! destination list through their vtables. Everything runs on one STA worker thread started on first use; the dock
//! only receives finished lists. A read that is no longer wanted stops between items, and the COM objects behind the
//! menu entries are kept only while the menu is open.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, PROPERTYKEY, SIZE};
use windows::Win32::System::Com::StructuredStorage::{
    PROPVARIANT, PropVariantClear, PropVariantToBoolean, PropVariantToStringAlloc,
    PropVariantToUInt32,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
};
use windows::Win32::System::Variant::{VT_BOOL, VT_BSTR, VT_LPWSTR, VT_UI4};
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::Shell::{
    BHID_PropertyStore, BHID_SFUIObject, CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX,
    FOLDERID_AppsFolder, IContextMenu, IShellItem, IShellItemImageFactory, IShellLinkW,
    KF_FLAG_DEFAULT, SHCreateItemFromParsingName, SHCreateItemInKnownFolder, SHLoadIndirectString,
    SIGDN_NORMALDISPLAY, SIIGBF_ICONONLY, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyIcon, DestroyMenu, GET_MENU_DEFAULT_ITEM_FLAGS, GetMenuDefaultItem,
    HICON, PrivateExtractIconsW, SW_SHOWNORMAL,
};
use windows::core::{GUID, HRESULT, HSTRING, IUnknown, Interface, PCSTR, PCWSTR, PWSTR};

use crate::icons::{self, Icon};

/// `JumpListStatus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ready,
    Empty,
    IdentityUnavailable,
    Unsupported,
    Unavailable,
}

#[derive(Clone, Debug)]
pub enum Entry {
    Item {
        command: u32,
        title: String,
        icon: Option<Arc<Icon>>,
    },
    Separator,
}

#[derive(Clone, Debug)]
pub struct Group {
    pub title: String,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug)]
pub struct JumpList {
    pub session: u64,
    pub status: Status,
    pub groups: Vec<Group>,
}

/// Localized group titles (`PinnedDestinations`, `Frequent`, `Recent`, `Tasks`).
#[derive(Clone, Debug)]
pub struct Titles {
    pub pinned: String,
    pub frequent: String,
    pub recent: String,
    pub tasks: String,
}

pub enum Outcome {
    Read(JumpList),
    /// The worker thread could not start COM (`TasksServiceError`).
    ServiceFailed(u64),
    Invoked {
        session: u64,
        ok: bool,
    },
}

enum Request {
    Read {
        session: u64,
        window: isize,
        shortcut: Option<String>,
        titles: Titles,
        resource_icon_px: u32,
    },
    Invoke {
        session: u64,
        command: u32,
        owner: isize,
    },
    Release {
        session: u64,
    },
}

/// The dock's handle to the worker. The thread starts on the first request and ends with this value.
pub struct JumpLists {
    notify: Arc<dyn Fn() + Send + Sync>,
    requests: Option<Sender<Request>>,
    results: Arc<Mutex<Vec<Outcome>>>,
    /// The session whose read is still wanted (0: none); the worker stops a read that is no longer wanted.
    wanted: Arc<AtomicU64>,
    next_session: u64,
}

impl JumpLists {
    /// `notify` runs on the worker thread after each outcome (e.g. posts a message to the dock window).
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            notify: Arc::new(notify),
            requests: None,
            results: Arc::default(),
            wanted: Arc::default(),
            next_session: 0,
        }
    }

    fn send(&mut self, request: Request) {
        if self.requests.is_none() {
            let (sender, receiver) = mpsc::channel::<Request>();
            let results = self.results.clone();
            let wanted = self.wanted.clone();
            let notify = self.notify.clone();
            let spawned = std::thread::Builder::new()
                .name("ot-jumplists".into())
                .spawn(move || worker(receiver, results, wanted, notify));
            if let Err(error) = spawned {
                ot_core::log::warn(format!("Jump list thread could not start: {error}"));
                return;
            }
            self.requests = Some(sender);
        }
        if let Some(requests) = &self.requests {
            let _ = requests.send(request);
        }
    }

    /// Starts reading the jump list of `window` (or of the pinned `shortcut` when the app has no window).
    /// Returns the session the result will carry.
    pub fn read(
        &mut self,
        window: isize,
        shortcut: Option<String>,
        titles: Titles,
        resource_icon_px: u32,
    ) -> u64 {
        self.next_session += 1;
        let session = self.next_session;
        self.wanted.store(session, Ordering::SeqCst);
        self.send(Request::Read {
            session,
            window,
            shortcut,
            titles,
            resource_icon_px,
        });
        session
    }

    /// The read in progress is no longer wanted (menu closed, timeout).
    pub fn cancel(&self) {
        self.wanted.store(0, Ordering::SeqCst);
    }

    pub fn invoke(&mut self, session: u64, command: u32, owner: isize) {
        self.send(Request::Invoke {
            session,
            command,
            owner,
        });
    }

    /// Drops the COM objects behind a session's entries.
    pub fn release(&mut self, session: u64) {
        if self.requests.is_some() {
            self.send(Request::Release { session });
        }
    }

    pub fn drain(&self) -> Vec<Outcome> {
        self.results
            .lock()
            .map(|mut r| std::mem::take(&mut *r))
            .unwrap_or_default()
    }
}

// ---------- Worker thread ----------

fn worker(
    receiver: mpsc::Receiver<Request>,
    results: Arc<Mutex<Vec<Outcome>>>,
    wanted: Arc<AtomicU64>,
    notify: Arc<dyn Fn() + Send + Sync>,
) {
    // SAFETY: COM for the Shell objects on this thread, which owns all of them.
    let com = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
    let push = |outcome: Outcome| {
        if let Ok(mut r) = results.lock() {
            r.push(outcome);
        }
        notify();
    };
    let mut current: Option<Session> = None;
    for request in receiver {
        match request {
            Request::Read {
                session,
                window,
                shortcut,
                titles,
                resource_icon_px,
            } => {
                current = None;
                if com.is_err() {
                    push(Outcome::ServiceFailed(session));
                    continue;
                }
                if wanted.load(Ordering::SeqCst) != session {
                    continue;
                }
                let mut state = Session {
                    id: session,
                    commands: HashMap::new(),
                    next_command: 0,
                    target_window: window,
                    resource_icon_px,
                    wanted: wanted.clone(),
                };
                let list = match state.read(window, shortcut.as_deref(), &titles) {
                    Ok(Some(list)) => list,
                    // Cancelled: nothing to show.
                    Ok(None) => continue,
                    Err(error) => {
                        ot_core::log::warn(format!(
                            "Jump List read failed: 0x{:08X}",
                            error.code().0
                        ));
                        // E_NOINTERFACE or REGDB_E_CLASSNOTREG: this Windows has no such objects.
                        let unsupported =
                            matches!(error.code().0 as u32, 0x8000_4002 | 0x8004_0154);
                        JumpList {
                            session,
                            status: if unsupported {
                                Status::Unsupported
                            } else {
                                Status::Unavailable
                            },
                            groups: Vec::new(),
                        }
                    }
                };
                if list.status == Status::Ready {
                    current = Some(state);
                }
                push(Outcome::Read(list));
            }
            Request::Invoke {
                session,
                command,
                owner,
            } => {
                let ok = match current.as_ref() {
                    Some(state) if state.id == session => match state.invoke(command, owner) {
                        Ok(()) => true,
                        Err(error) => {
                            ot_core::log::warn(format!(
                                "Jump List invocation failed: 0x{:08X}",
                                error.code().0
                            ));
                            false
                        }
                    },
                    _ => false,
                };
                push(Outcome::Invoked { session, ok });
            }
            Request::Release { session } => {
                if current.as_ref().is_some_and(|s| s.id == session) {
                    current = None;
                }
            }
        }
    }
    drop(current);
}

// ---------- Shell ABI ----------

const RESOLVER_CLASS: GUID = GUID::from_u128(0x660b90c8_73a9_4b58_8cae_355b7f55341b);
const RESOLVER_IID: GUID = GUID::from_u128(0xde25675a_72de_44b4_9373_05170450c140);
const DESTINATION_CLASS: GUID = GUID::from_u128(0x77f10cf0_3db5_4966_b520_b7c54fd35ed6);
const DESTINATION_IID: GUID = GUID::from_u128(0x507101cd_f6ad_46c8_8e20_eeb9e6bac47f);
const AUTOMATIC_CLASS: GUID = GUID::from_u128(0xf0ae1542_f497_484b_a175_a20db09144ba);
const AUTOMATIC_IID: GUID = GUID::from_u128(0xe9c5ef8d_fd41_4f72_ba87_eb03bad5817c);
const OBJECT_ARRAY_IID: GUID = GUID::from_u128(0x92ca9dcd_5622_4bba_a805_5e9f541bd8c9);
const APP_FORMAT: GUID = GUID::from_u128(0x9f4c2855_9f79_4b39_a8d0_e1d42de1d5f3);
/// `PKEY_AppUserModel_ID`, `_IsDestListSeparator`, `_HostEnvironment`, `_ActivationContext`.
const APP_ID_KEY: PROPERTYKEY = PROPERTYKEY {
    fmtid: APP_FORMAT,
    pid: 5,
};
const SEPARATOR_KEY: PROPERTYKEY = PROPERTYKEY {
    fmtid: APP_FORMAT,
    pid: 6,
};
const HOST_ENVIRONMENT_KEY: PROPERTYKEY = PROPERTYKEY {
    fmtid: APP_FORMAT,
    pid: 14,
};
const ACTIVATION_CONTEXT_KEY: PROPERTYKEY = PROPERTYKEY {
    fmtid: APP_FORMAT,
    pid: 20,
};
/// `PKEY_Title`.
const TITLE_KEY: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xf29f85e0_4ff9_1068_ab91_08002b27b3d9),
    pid: 2,
};
/// `PKEY_Link_Arguments`: Firefox/LibreWolf keep `-private-window` here instead of in the link's arguments.
const LINK_ARGUMENTS_KEY: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0x436f2667_14e2_4feb_b30a_146c53b5b674),
    pid: 100,
};
/// `SLGP_RAWPATH`.
const RAW_PATH: u32 = 4;
const MAX_COMMANDS: usize = 100;
const MAX_PER_GROUP: u32 = 40;
const MAX_CATEGORIES: u32 = 32;
const ICON_PX: i32 = 24;

/// `APPDESTCATEGORY`: type at 0, then the name (custom) or the known-category kind at 8; 64 bytes in all.
#[repr(C)]
struct Category {
    kind: i32,
    _pad: i32,
    value: usize,
    _rest: [u8; 48],
}

type SetAppIdFn = unsafe extern "system" fn(*mut c_void, PCWSTR) -> HRESULT;
type CountFn = unsafe extern "system" fn(*mut c_void, *mut u32) -> HRESULT;
type CategoryFn = unsafe extern "system" fn(*mut c_void, u32, i32, *mut Category) -> HRESULT;
type ObjectFn =
    unsafe extern "system" fn(*mut c_void, u32, *const GUID, *mut *mut c_void) -> HRESULT;
type InitializeFn = unsafe extern "system" fn(*mut c_void, PCWSTR, PCWSTR, PCWSTR) -> HRESULT;
type ListFn =
    unsafe extern "system" fn(*mut c_void, i32, u32, u32, *const GUID, *mut *mut c_void) -> HRESULT;
type ResolveWindowFn = unsafe extern "system" fn(
    *mut c_void,
    HWND,
    *mut PWSTR,
    *mut c_void,
    *mut c_void,
    *mut c_void,
) -> HRESULT;
type ResolveShortcutFn = unsafe extern "system" fn(*mut c_void, *mut c_void, *mut PWSTR) -> HRESULT;

/// The `index`th vtable entry of a COM object, as `F`.
///
/// # Safety
/// `object` must be a live COM object whose vtable has a function of type `F` at `index`.
unsafe fn method<F: Copy>(object: &IUnknown, index: usize) -> F {
    // SAFETY: per the contract above; function pointers and `usize` have the same size.
    unsafe {
        let vtable = *(object.as_raw() as *const *const usize);
        std::mem::transmute_copy(&*vtable.add(index))
    }
}

fn create(class: &GUID, iid: &GUID) -> windows::core::Result<IUnknown> {
    // SAFETY: plain COM activation.
    let unknown: IUnknown = unsafe { CoCreateInstance(class, None, CLSCTX_INPROC_SERVER)? };
    query(&unknown, iid)
}

fn query(object: &IUnknown, iid: &GUID) -> windows::core::Result<IUnknown> {
    let mut raw = std::ptr::null_mut();
    // SAFETY: QueryInterface with a valid out pointer; the result is owned by the returned value.
    unsafe {
        object.query(iid, &mut raw).ok()?;
        Ok(IUnknown::from_raw(raw))
    }
}

fn take_string(text: PWSTR) -> Option<String> {
    if text.is_null() {
        return None;
    }
    // SAFETY: a CoTaskMem string returned to us; freed here.
    unsafe {
        let value = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const c_void));
        value
    }
}

/// A property value, cleared when dropped.
struct Value(PROPVARIANT);

impl Value {
    fn read(store: &IPropertyStore, key: &PROPERTYKEY) -> windows::core::Result<Self> {
        // SAFETY: a property read; the value is cleared on drop.
        unsafe { store.GetValue(key).map(Self) }
    }

    fn kind(&self) -> windows::Win32::System::Variant::VARENUM {
        // SAFETY: the type tag is always valid to read.
        unsafe { self.0.Anonymous.Anonymous.vt }
    }

    /// `VT_LPWSTR` or `VT_BSTR` as text (`ReadString`).
    fn text(&self) -> Option<String> {
        if self.kind() != VT_LPWSTR && self.kind() != VT_BSTR {
            return None;
        }
        // SAFETY: a string variant.
        unsafe { PropVariantToStringAlloc(&self.0).ok().and_then(take_string) }
    }

    fn flag(&self) -> bool {
        // SAFETY: a boolean variant.
        self.kind() == VT_BOOL
            && unsafe { PropVariantToBoolean(&self.0) }.is_ok_and(|b| b.as_bool())
    }

    fn number(&self) -> Option<u32> {
        if self.kind() != VT_UI4 {
            return None;
        }
        // SAFETY: an unsigned integer variant.
        unsafe { PropVariantToUInt32(&self.0) }.ok()
    }
}

impl Drop for Value {
    fn drop(&mut self) {
        // SAFETY: clearing a value this wrapper owns.
        let _ = unsafe { PropVariantClear(&mut self.0) };
    }
}

fn read_string(store: &IPropertyStore, key: &PROPERTYKEY) -> windows::core::Result<Option<String>> {
    Ok(Value::read(store, key)?.text())
}

/// Resolves `@resource` strings (`SHLoadIndirectString`).
fn resolve_text(text: Option<String>) -> windows::core::Result<Option<String>> {
    match text {
        Some(text) if text.starts_with('@') => {
            let mut buffer = vec![0u16; 1024];
            // SAFETY: an output buffer of the given length.
            unsafe { SHLoadIndirectString(&HSTRING::from(text.as_str()), &mut buffer, None)? };
            let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            Ok(Some(String::from_utf16_lossy(&buffer[..end])))
        }
        other => Ok(other),
    }
}

/// `MissingList`: STG_E_FILENOTFOUND or ERROR_FILE_NOT_FOUND, an app without that list.
fn missing_list(result: HRESULT) -> bool {
    matches!(result.0 as u32, 0x8003_0002 | 0x8007_0002)
}

struct Session {
    id: u64,
    commands: HashMap<u32, IUnknown>,
    next_command: u32,
    /// The app window the list was read for: invocations use it as the activation context owner, so single-instance
    /// apps keep task flags such as `-private-window`.
    target_window: isize,
    resource_icon_px: u32,
    wanted: Arc<AtomicU64>,
}

impl Session {
    fn cancelled(&self) -> bool {
        self.wanted.load(Ordering::SeqCst) != self.id
    }

    /// `NativeJumpListSession.Read`. `Ok(None)`: cancelled.
    fn read(
        &mut self,
        window: isize,
        shortcut: Option<&str>,
        titles: &Titles,
    ) -> windows::core::Result<Option<JumpList>> {
        let session = self.id;
        if cfg!(not(target_pointer_width = "64")) {
            return Ok(Some(JumpList {
                session,
                status: Status::Unsupported,
                groups: Vec::new(),
            }));
        }
        let Some(app_id) = resolve_identity(window, shortcut)?.filter(|id| !id.trim().is_empty())
        else {
            return Ok(Some(JumpList {
                session,
                status: Status::IdentityUnavailable,
                groups: Vec::new(),
            }));
        };
        if self.cancelled() {
            return Ok(None);
        }
        let id = HSTRING::from(app_id.as_str());
        let custom = create(&DESTINATION_CLASS, &DESTINATION_IID)?;
        let automatic = create(&AUTOMATIC_CLASS, &AUTOMATIC_IID)?;
        // SAFETY: vtable calls on the destination lists with the signatures OpenTaskbar uses.
        unsafe {
            method::<SetAppIdFn>(&custom, 4)(custom.as_raw(), PCWSTR(id.as_ptr())).ok()?;
            method::<InitializeFn>(&automatic, 3)(
                automatic.as_raw(),
                PCWSTR(id.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
            )
            .ok()?;
        }
        let mut groups = Vec::new();
        self.add_automatic(&mut groups, &automatic, 0, &titles.pinned)?;
        let mut count = 0u32;
        // SAFETY: vtable call with a valid out pointer.
        let result = unsafe { method::<CountFn>(&custom, 6)(custom.as_raw(), &mut count) };
        if missing_list(result) {
            count = 0;
        } else {
            result.ok()?;
        }
        if count > MAX_CATEGORIES {
            return Err(windows::core::Error::from_hresult(HRESULT(
                0x8000_FFFF_u32 as i32,
            )));
        }
        let mut known_category = false;
        for index in 0..count {
            if self.commands.len() >= MAX_COMMANDS {
                break;
            }
            if self.cancelled() {
                return Ok(None);
            }
            let mut category: Category = unsafe { std::mem::zeroed() };
            // SAFETY: vtable call with a 64-byte out structure.
            unsafe { method::<CategoryFn>(&custom, 7)(custom.as_raw(), index, 1, &mut category) }
                .ok()?;
            let outcome = (|| -> windows::core::Result<()> {
                match category.kind {
                    // A known category: 1 frequent, 2 recent.
                    1 => {
                        let kind = category.value as i32;
                        if kind == 1 || kind == 2 {
                            known_category = true;
                            let title = if kind == 1 {
                                &titles.frequent
                            } else {
                                &titles.recent
                            };
                            self.add_automatic(&mut groups, &automatic, 3 - kind, title)?;
                        }
                    }
                    // 0 custom (named), 2 tasks.
                    0 | 2 => {
                        let mut raw = std::ptr::null_mut();
                        // SAFETY: vtable call returning an IObjectArray we own.
                        unsafe {
                            method::<ObjectFn>(&custom, 9)(
                                custom.as_raw(),
                                index,
                                &OBJECT_ARRAY_IID,
                                &mut raw,
                            )
                            .ok()?;
                        }
                        // SAFETY: an owned COM pointer.
                        let array = unsafe { IUnknown::from_raw(raw) };
                        let title = if category.kind == 2 {
                            Some(titles.tasks.clone())
                        } else {
                            // SAFETY: the category name is a string the list allocated.
                            let name =
                                unsafe { PCWSTR(category.value as *const u16).to_string().ok() };
                            resolve_text(name)?
                        };
                        if let Some(title) = title {
                            self.add_group(&mut groups, &title, &array)?;
                        }
                    }
                    _ => {}
                }
                Ok(())
            })();
            if category.kind == 0 && category.value != 0 {
                // SAFETY: the name was allocated with CoTaskMemAlloc for us.
                unsafe { CoTaskMemFree(Some(category.value as *const c_void)) };
            }
            outcome?;
        }
        if !known_category {
            self.add_automatic(&mut groups, &automatic, 1, &titles.recent)?;
        }
        if self.cancelled() {
            return Ok(None);
        }
        Ok(Some(JumpList {
            session,
            status: if groups.is_empty() {
                Status::Empty
            } else {
                Status::Ready
            },
            groups,
        }))
    }

    /// Pinned (0) or recent (1) / frequent (2) destinations, at most 20.
    fn add_automatic(
        &mut self,
        groups: &mut Vec<Group>,
        automatic: &IUnknown,
        kind: i32,
        title: &str,
    ) -> windows::core::Result<()> {
        let mut raw = std::ptr::null_mut();
        // SAFETY: vtable call returning an IObjectArray we own.
        let result = unsafe {
            method::<ListFn>(automatic, 5)(
                automatic.as_raw(),
                kind,
                20,
                1,
                &OBJECT_ARRAY_IID,
                &mut raw,
            )
        };
        if missing_list(result) {
            return Ok(());
        }
        result.ok()?;
        // SAFETY: an owned COM pointer.
        let array = unsafe { IUnknown::from_raw(raw) };
        self.add_group(groups, title, &array)
    }

    /// `AddGroup`: up to 40 entries with a title (the item's `PKEY_Title`, else its display name) and an icon;
    /// separators collapse and never end a group.
    fn add_group(
        &mut self,
        groups: &mut Vec<Group>,
        title: &str,
        array: &IUnknown,
    ) -> windows::core::Result<()> {
        if title.trim().is_empty() {
            return Ok(());
        }
        let mut count = 0u32;
        // SAFETY: IObjectArray::GetCount.
        unsafe { method::<CountFn>(array, 3)(array.as_raw(), &mut count) }.ok()?;
        let mut entries: Vec<Entry> = Vec::new();
        for index in 0..count.min(MAX_PER_GROUP) {
            if self.commands.len() >= MAX_COMMANDS || self.cancelled() {
                break;
            }
            let mut raw = std::ptr::null_mut();
            // SAFETY: IObjectArray::GetAt returning an IUnknown we own.
            unsafe {
                method::<ObjectFn>(array, 4)(array.as_raw(), index, &IUnknown::IID, &mut raw)
            }
            .ok()?;
            // SAFETY: an owned COM pointer.
            let item = unsafe { IUnknown::from_raw(raw) };
            let store = item.cast::<IPropertyStore>().ok();
            if let Some(store) = &store
                && Value::read(store, &SEPARATOR_KEY)?.flag()
            {
                if entries
                    .last()
                    .is_some_and(|e| !matches!(e, Entry::Separator))
                {
                    entries.push(Entry::Separator);
                }
                continue;
            }
            let mut label = match &store {
                Some(store) => resolve_text(read_string(store, &TITLE_KEY)?)?,
                None => None,
            };
            if label.as_deref().is_none_or(|l| l.trim().is_empty())
                && let Ok(shell_item) = item.cast::<IShellItem>()
            {
                // SAFETY: the display name is freed by `take_string`.
                label = take_string(unsafe { shell_item.GetDisplayName(SIGDN_NORMALDISPLAY)? });
            }
            let Some(label) = label.filter(|l| !l.trim().is_empty()) else {
                continue;
            };
            self.next_command += 1;
            let command = self.next_command;
            let icon = self.read_icon(&item).map(Arc::new);
            entries.push(Entry::Item {
                command,
                title: label,
                icon,
            });
            self.commands.insert(command, item);
        }
        if matches!(entries.last(), Some(Entry::Separator)) {
            entries.pop();
        }
        if !entries.is_empty() {
            groups.push(Group {
                title: title.to_owned(),
                entries,
            });
        }
        Ok(())
    }

    /// `ReadIcon`: the Shell image at 24 px, else a task link's icon resource.
    fn read_icon(&self, item: &IUnknown) -> Option<Icon> {
        if let Ok(factory) = item.cast::<IShellItemImageFactory>() {
            // SAFETY: the bitmap is converted and deleted by `icons::from_hbitmap`.
            let bitmap = unsafe {
                factory.GetImage(
                    SIZE {
                        cx: ICON_PX,
                        cy: ICON_PX,
                    },
                    SIIGBF_ICONONLY,
                )
            };
            if let Ok(bitmap) = bitmap
                && !bitmap.is_invalid()
            {
                return icons::from_hbitmap(bitmap);
            }
        }
        let link = item.cast::<IShellLinkW>().ok()?;
        let mut path = vec![0u16; 2048];
        let mut index = 0i32;
        // SAFETY: buffers of the given sizes.
        unsafe { link.GetIconLocation(&mut path, &mut index).ok()? };
        let mut path = wide_to_string(&path);
        if path.is_empty() {
            let mut buffer = vec![0u16; 2048];
            // SAFETY: as above.
            unsafe {
                link.GetPath(&mut buffer, std::ptr::null_mut(), RAW_PATH)
                    .ok()?
            };
            path = wide_to_string(&buffer);
            index = 0;
            if path.is_empty() {
                return None;
            }
        }
        resource_icon(&path, index, self.resource_icon_px.max(16))
    }

    /// `NativeJumpListSession.Invoke`.
    fn invoke(&self, command: u32, owner: isize) -> windows::core::Result<()> {
        let item = self
            .commands
            .get(&command)
            .ok_or_else(|| windows::core::Error::from_hresult(HRESULT(0x8000_FFFF_u32 as i32)))?;
        let store = item.cast::<IPropertyStore>().ok();
        let app_id = match &store {
            Some(store) => read_string(store, &APP_ID_KEY)?,
            None => None,
        };
        // Packaged task links activate their AppsFolder target with the published activation context.
        if let Some(app_id) = app_id.filter(|id| id.contains('!')) {
            // SAFETY: Shell calls with owned results.
            let target: IShellItem = unsafe {
                SHCreateItemInKnownFolder(
                    &FOLDERID_AppsFolder,
                    KF_FLAG_DEFAULT,
                    &HSTRING::from(app_id.as_str()),
                )?
            };
            let properties: IPropertyStore =
                unsafe { target.BindToHandler(None, &BHID_PropertyStore)? };
            let host = Value::read(&properties, &HOST_ENVIRONMENT_KEY)?.number();
            if host.is_some_and(|h| h != 0) {
                let menu: IContextMenu = unsafe { target.BindToHandler(None, &BHID_SFUIObject)? };
                let context = match &store {
                    Some(store) => read_string(store, &ACTIVATION_CONTEXT_KEY)?,
                    None => None,
                };
                return invoke_default(&menu, owner, context.as_deref());
            }
        }
        // Unpackaged tasks: invoke through the link's own menu with the target app's window, the activation
        // context Explorer's taskbar supplies.
        let context_owner = if self.target_window != 0 {
            self.target_window
        } else {
            owner
        };
        if let Ok(menu) = item.cast::<IContextMenu>() {
            return invoke_default(&menu, context_owner, None);
        }
        if let Ok(shell_item) = item.cast::<IShellItem>() {
            // SAFETY: Shell binding with an owned result.
            let menu: IContextMenu = unsafe { shell_item.BindToHandler(None, &BHID_SFUIObject)? };
            return invoke_default(&menu, context_owner, None);
        }
        if launch_shell_link(item, store.as_ref())? {
            return Ok(());
        }
        Err(windows::core::Error::from_hresult(HRESULT(
            0x8000_4002_u32 as i32,
        )))
    }
}

fn wide_to_string(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// `ResolveIdentity`: the window's explicit AppUserModelID, else the Shell's resolution of the window or of the
/// pinned shortcut.
fn resolve_identity(
    window: isize,
    shortcut: Option<&str>,
) -> windows::core::Result<Option<String>> {
    if window != 0 {
        // SAFETY: a property store for a window handle.
        let store: IPropertyStore = unsafe { SHGetPropertyStoreForWindow(HWND(window as *mut _))? };
        if let Some(id) = read_string(&store, &APP_ID_KEY)?.filter(|id| !id.trim().is_empty()) {
            return Ok(Some(id));
        }
    }
    let resolver = create(&RESOLVER_CLASS, &RESOLVER_IID)?;
    let mut id = PWSTR::null();
    if window != 0 {
        // SAFETY: IApplicationResolver::GetAppIDForWindow.
        unsafe {
            method::<ResolveWindowFn>(&resolver, 5)(
                resolver.as_raw(),
                HWND(window as *mut _),
                &mut id,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
            .ok()?;
        }
    } else if let Some(shortcut) = shortcut.filter(|s| !s.trim().is_empty()) {
        // SAFETY: Shell item creation, then IApplicationResolver::GetAppIDForShortcut.
        unsafe {
            let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(shortcut), None)?;
            method::<ResolveShortcutFn>(&resolver, 3)(resolver.as_raw(), item.as_raw(), &mut id)
                .ok()?;
        }
    } else {
        return Ok(None);
    }
    Ok(take_string(id))
}

/// Runs the default command of a Shell context menu (`InvokeDefault`).
fn invoke_default(
    menu: &IContextMenu,
    owner: isize,
    activation_context: Option<&str>,
) -> windows::core::Result<()> {
    // SAFETY: a popup menu destroyed below; the invoke structure points at buffers that outlive the call.
    unsafe {
        let popup = CreatePopupMenu()?;
        let result = (|| {
            // CMF_DEFAULTONLY.
            menu.QueryContextMenu(popup, 0, 1, 0x7fff, 1).ok()?;
            let id = GetMenuDefaultItem(popup, 0, GET_MENU_DEFAULT_ITEM_FLAGS(0));
            if id == u32::MAX || id == 0 {
                return Err(windows::core::Error::from_hresult(HRESULT(
                    0x8000_4005_u32 as i32,
                )));
            }
            let verb = (id - 1) as usize;
            let narrow: Option<Vec<u8>> =
                activation_context.map(|c| c.bytes().chain(Some(0)).collect());
            let wide: Option<Vec<u16>> =
                activation_context.map(|c| c.encode_utf16().chain(Some(0)).collect());
            let info = CMINVOKECOMMANDINFOEX {
                cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
                // CMIC_MASK_UNICODE
                fMask: 0x4000,
                hwnd: HWND(owner as *mut _),
                lpVerb: PCSTR(verb as *const u8),
                lpParameters: narrow.as_ref().map_or(PCSTR::null(), |n| PCSTR(n.as_ptr())),
                nShow: 1,
                lpVerbW: PCWSTR(verb as *const u16),
                lpParametersW: wide.as_ref().map_or(PCWSTR::null(), |w| PCWSTR(w.as_ptr())),
                ..Default::default()
            };
            menu.InvokeCommand(
                (&info as *const CMINVOKECOMMANDINFOEX).cast::<CMINVOKECOMMANDINFO>(),
            )
        })();
        let _ = DestroyMenu(popup);
        result
    }
}

/// `TryLaunchShellLink`: starts a link's target with its arguments (from the property store when the link has
/// none). Returns false when the item is not a link.
fn launch_shell_link(
    item: &IUnknown,
    store: Option<&IPropertyStore>,
) -> windows::core::Result<bool> {
    let Ok(link) = item.cast::<IShellLinkW>() else {
        return Ok(false);
    };
    let mut path = vec![0u16; 260];
    // SAFETY: buffers of the given sizes.
    if unsafe { link.GetPath(&mut path, std::ptr::null_mut(), RAW_PATH) }.is_err() {
        return Ok(false);
    }
    let path = wide_to_string(&path);
    if path.is_empty() {
        return Ok(false);
    }
    let mut arguments = vec![0u16; 2048];
    let mut args = if unsafe { link.GetArguments(&mut arguments) }.is_ok() {
        wide_to_string(&arguments)
    } else {
        String::new()
    };
    if args.trim().is_empty()
        && let Some(store) = store
    {
        args = read_string(store, &LINK_ARGUMENTS_KEY)?.unwrap_or_default();
    }
    let mut directory = vec![0u16; 260];
    let directory = if unsafe { link.GetWorkingDirectory(&mut directory) }.is_ok() {
        wide_to_string(&directory)
    } else {
        String::new()
    };
    let directory = HSTRING::from(directory.trim());
    // SAFETY: ShellExecute with null-terminated strings that outlive the call.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR::null(),
            &HSTRING::from(path.as_str()),
            &HSTRING::from(args.as_str()),
            if directory.is_empty() {
                PCWSTR::null()
            } else {
                PCWSTR(directory.as_ptr())
            },
            SW_SHOWNORMAL,
        )
    };
    if result.0 as usize <= 32 {
        return Err(windows::core::Error::from_thread());
    }
    ot_core::log::info(format!("Jump List task launched directly: {path} {args}").trim_end());
    Ok(true)
}

/// `ApplicationIconProvider.GetForResource`: an icon resource of a file at `size` pixels.
pub(crate) fn resource_icon(path: &str, index: i32, size: u32) -> Option<Icon> {
    let expanded = expand(path);
    if !std::path::Path::new(&expanded).exists() {
        return None;
    }
    let mut name = [0u16; 260];
    for (slot, unit) in name.iter_mut().zip(expanded.encode_utf16().take(259)) {
        *slot = unit;
    }
    let mut icon = [HICON::default(); 1];
    let mut id = [0u32; 1];
    // SAFETY: one icon requested into arrays of one; destroyed below.
    let count = unsafe {
        PrivateExtractIconsW(
            &name,
            index,
            size as i32,
            size as i32,
            Some(&mut icon),
            Some(id.as_mut_ptr()),
            0,
        )
    };
    if count == 0 || count == u32::MAX || icon[0].is_invalid() {
        return None;
    }
    let pixels = icons::from_hicon(icon[0]);
    // SAFETY: an icon we own.
    let _ = unsafe { DestroyIcon(icon[0]) };
    pixels
}

fn expand(path: &str) -> String {
    use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
    let source = HSTRING::from(path);
    let mut buffer = vec![0u16; 1024];
    // SAFETY: an output buffer of the given length.
    let written = unsafe { ExpandEnvironmentStringsW(&source, Some(&mut buffer)) } as usize;
    if written == 0 || written > buffer.len() {
        return path.to_owned();
    }
    wide_to_string(&buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_matches_the_shell_layout() {
        assert_eq!(size_of::<Category>(), 64);
        assert_eq!(std::mem::offset_of!(Category, value), 8);
    }

    #[test]
    fn missing_lists_are_not_errors() {
        assert!(missing_list(HRESULT(0x8003_0002_u32 as i32)));
        assert!(missing_list(HRESULT(0x8007_0002_u32 as i32)));
        assert!(!missing_list(HRESULT(0x8000_4005_u32 as i32)));
    }
}
