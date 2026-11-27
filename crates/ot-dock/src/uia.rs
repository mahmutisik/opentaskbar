//! UI Automation for the dock: the window is a fragment root and every dock item is a Button with the Invoke
//! pattern, so screen readers announce and press dock items, and test scripts find them like OpenTaskbar's WPF
//! buttons (AutomationIds `StartButton`, `SearchButton`, `TaskViewButton`, `ShowDesktopButton`,
//! `RecycleBinButton`; apps carry no AutomationId, as in OpenTaskbar).
//!
//! Providers read a shared model the dock refreshes after every layout; invoking an item posts a message to the
//! dock window, so the click runs on the UI thread exactly like a mouse click.

#![allow(non_upper_case_globals)] // Windows API constant names

use std::sync::{Arc, Mutex};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Com::SAFEARRAY;
use windows::Win32::System::Ole::{SafeArrayCreateVector, SafeArrayPutElement};
use windows::Win32::System::Variant::{VARIANT, VT_I4};
use windows::Win32::UI::Accessibility::*;
use windows::core::{Error, IUnknown, Interface, Result, implement};
use windows_core::IUnknownImpl;

#[derive(Clone, Debug, Default)]
pub struct UiaItem {
    pub name: String,
    pub automation_id: String,
    /// Screen rectangle: left, top, right, bottom.
    pub rect: (i32, i32, i32, i32),
}

pub struct UiaModel {
    hwnd: isize,
    /// The root element: name, AutomationId and control type.
    root: (String, String, UIA_CONTROLTYPE_ID),
    items: Mutex<Vec<UiaItem>>,
    invoke_message: u32,
    /// The item with keyboard focus during keyboard navigation.
    focused: Mutex<Option<usize>>,
}

impl UiaModel {
    pub fn new(hwnd: isize, invoke_message: u32) -> Arc<Self> {
        Self::with_root(
            hwnd,
            invoke_message,
            "OpenTaskBar",
            "Dock",
            UIA_ToolBarControlTypeId,
        )
    }

    /// A window whose root is described by `name`, `automation_id` and `control_type` (the dock is a ToolBar).
    pub fn with_root(
        hwnd: isize,
        invoke_message: u32,
        name: &str,
        automation_id: &str,
        control_type: UIA_CONTROLTYPE_ID,
    ) -> Arc<Self> {
        Arc::new(Self {
            hwnd,
            root: (name.to_owned(), automation_id.to_owned(), control_type),
            items: Mutex::new(Vec::new()),
            invoke_message,
            focused: Mutex::new(None),
        })
    }

    /// Moves keyboard focus (keyboard navigation) and tells listening clients, so screen readers announce the
    /// item as they do for OpenTaskbar's focused WPF buttons.
    pub fn set_focus(self: &Arc<Self>, index: Option<usize>) {
        if let Ok(mut focused) = self.focused.lock() {
            *focused = index;
        }
        let Some(index) = index.filter(|&i| i < self.len()) else {
            return;
        };
        // SAFETY: plain UIA calls with a provider we own.
        unsafe {
            if UiaClientsAreListening().as_bool() {
                let item: IRawElementProviderSimple = Item {
                    model: self.clone(),
                    index,
                }
                .into();
                let _ = UiaRaiseAutomationEvent(&item, UIA_AutomationFocusChangedEventId);
            }
        }
    }

    fn focused(&self) -> Option<usize> {
        self.focused.lock().ok().and_then(|f| *f)
    }

    pub fn set_items(&self, items: Vec<UiaItem>) {
        if let Ok(mut current) = self.items.lock() {
            *current = items;
        }
    }

    fn item(&self, index: usize) -> Option<UiaItem> {
        self.items
            .lock()
            .ok()
            .and_then(|items| items.get(index).cloned())
    }

    fn len(&self) -> usize {
        self.items.lock().map(|items| items.len()).unwrap_or(0)
    }
}

/// Answers `WM_GETOBJECT` for the UIA root; returns `None` for other object ids.
pub fn handle_get_object(model: &Arc<UiaModel>, wparam: usize, lparam: isize) -> Option<isize> {
    if lparam as i32 != UiaRootObjectId {
        return None;
    }
    let root: IRawElementProviderSimple = Root {
        model: model.clone(),
    }
    .into();
    // SAFETY: standard WM_GETOBJECT answer for our own window.
    let result = unsafe {
        UiaReturnRawElementProvider(
            HWND(model.hwnd as *mut _),
            WPARAM(wparam),
            LPARAM(lparam),
            &root,
        )
    };
    Some(result.0)
}

/// Releases providers handed to UIA (call on WM_DESTROY).
pub fn disconnect(hwnd: isize) {
    // SAFETY: documented cleanup call with a null provider.
    unsafe {
        let _ = UiaReturnRawElementProvider(
            HWND(hwnd as *mut _),
            WPARAM(0),
            LPARAM(0),
            None::<&IRawElementProviderSimple>,
        );
    }
}

fn item_fragment(model: &Arc<UiaModel>, index: usize) -> Result<IRawElementProviderFragment> {
    if index < model.len() {
        Ok(Item {
            model: model.clone(),
            index,
        }
        .into())
    } else {
        Err(Error::empty()) // S_OK with no element
    }
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IRawElementProviderFragmentRoot
)]
struct Root {
    model: Arc<UiaModel>,
}

impl IRawElementProviderSimple_Impl for Root_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }

    fn GetPatternProvider(&self, _pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
        Err(Error::empty())
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> Result<VARIANT> {
        Ok(match property {
            UIA_NamePropertyId => VARIANT::from(self.model.root.0.as_str()),
            UIA_ControlTypePropertyId => VARIANT::from(self.model.root.2.0),
            UIA_AutomationIdPropertyId => VARIANT::from(self.model.root.1.as_str()),
            _ => VARIANT::default(),
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        // SAFETY: our own window.
        unsafe { UiaHostProviderFromHwnd(HWND(self.model.hwnd as *mut _)) }
    }
}

impl IRawElementProviderFragment_Impl for Root_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        match direction {
            NavigateDirection_FirstChild => item_fragment(&self.model, 0),
            NavigateDirection_LastChild => {
                item_fragment(&self.model, self.model.len().wrapping_sub(1))
            }
            _ => Err(Error::empty()),
        }
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut()) // the host window provides it
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        Ok(UiaRect::default()) // the host window provides it
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(self.to_interface())
    }
}

impl IRawElementProviderFragmentRoot_Impl for Root_Impl {
    fn ElementProviderFromPoint(&self, x: f64, y: f64) -> Result<IRawElementProviderFragment> {
        let items = self
            .model
            .items
            .lock()
            .map(|i| i.clone())
            .unwrap_or_default();
        let (x, y) = (x as i32, y as i32);
        match items
            .iter()
            .position(|i| x >= i.rect.0 && x < i.rect.2 && y >= i.rect.1 && y < i.rect.3)
        {
            Some(index) => item_fragment(&self.model, index),
            None => Ok(self.to_interface()),
        }
    }

    fn GetFocus(&self) -> Result<IRawElementProviderFragment> {
        match self.model.focused() {
            Some(index) => item_fragment(&self.model, index),
            None => Err(Error::empty()),
        }
    }
}

#[implement(
    IRawElementProviderSimple,
    IRawElementProviderFragment,
    IInvokeProvider
)]
struct Item {
    model: Arc<UiaModel>,
    index: usize,
}

impl IRawElementProviderSimple_Impl for Item_Impl {
    fn ProviderOptions(&self) -> Result<ProviderOptions> {
        Ok(ProviderOptions_ServerSideProvider | ProviderOptions_UseComThreading)
    }

    fn GetPatternProvider(&self, pattern: UIA_PATTERN_ID) -> Result<IUnknown> {
        if pattern == UIA_InvokePatternId {
            let invoke: IInvokeProvider = self.to_interface();
            return invoke.cast();
        }
        Err(Error::empty())
    }

    fn GetPropertyValue(&self, property: UIA_PROPERTY_ID) -> Result<VARIANT> {
        let Some(item) = self.model.item(self.index) else {
            return Ok(VARIANT::default());
        };
        Ok(match property {
            UIA_NamePropertyId => VARIANT::from(item.name.as_str()),
            UIA_ControlTypePropertyId => VARIANT::from(UIA_ButtonControlTypeId.0),
            UIA_AutomationIdPropertyId if !item.automation_id.is_empty() => {
                VARIANT::from(item.automation_id.as_str())
            }
            UIA_IsEnabledPropertyId => VARIANT::from(true),
            UIA_IsKeyboardFocusablePropertyId => VARIANT::from(true),
            UIA_HasKeyboardFocusPropertyId => {
                VARIANT::from(self.model.focused() == Some(self.index))
            }
            _ => VARIANT::default(),
        })
    }

    fn HostRawElementProvider(&self) -> Result<IRawElementProviderSimple> {
        Err(Error::empty())
    }
}

impl IRawElementProviderFragment_Impl for Item_Impl {
    fn Navigate(&self, direction: NavigateDirection) -> Result<IRawElementProviderFragment> {
        match direction {
            NavigateDirection_Parent => Ok(Root {
                model: self.model.clone(),
            }
            .into()),
            NavigateDirection_NextSibling => item_fragment(&self.model, self.index + 1),
            NavigateDirection_PreviousSibling if self.index > 0 => {
                item_fragment(&self.model, self.index - 1)
            }
            _ => Err(Error::empty()),
        }
    }

    fn GetRuntimeId(&self) -> Result<*mut SAFEARRAY> {
        // SAFETY: a two-element VT_I4 SAFEARRAY owned by the caller.
        unsafe {
            let array = SafeArrayCreateVector(VT_I4, 0, 2);
            if array.is_null() {
                return Err(Error::from_hresult(
                    windows::Win32::Foundation::E_OUTOFMEMORY,
                ));
            }
            let values = [UiaAppendRuntimeId as i32, self.index as i32 + 1];
            for (position, value) in values.iter().enumerate() {
                let position = position as i32;
                SafeArrayPutElement(array, &position, (value as *const i32).cast())?;
            }
            Ok(array)
        }
    }

    fn BoundingRectangle(&self) -> Result<UiaRect> {
        let Some(item) = self.model.item(self.index) else {
            return Ok(UiaRect::default());
        };
        let (l, t, r, b) = item.rect;
        Ok(UiaRect {
            left: f64::from(l),
            top: f64::from(t),
            width: f64::from(r - l),
            height: f64::from(b - t),
        })
    }

    fn GetEmbeddedFragmentRoots(&self) -> Result<*mut SAFEARRAY> {
        Ok(std::ptr::null_mut())
    }

    fn SetFocus(&self) -> Result<()> {
        Ok(())
    }

    fn FragmentRoot(&self) -> Result<IRawElementProviderFragmentRoot> {
        Ok(Root {
            model: self.model.clone(),
        }
        .into())
    }
}

impl IInvokeProvider_Impl for Item_Impl {
    fn Invoke(&self) -> Result<()> {
        // SAFETY: posting to our own window; the UI thread performs the click.
        let posted = unsafe {
            windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                self.model.hwnd as windows_sys::Win32::Foundation::HWND,
                self.model.invoke_message,
                self.index,
                0,
            )
        };
        if posted == 0 {
            Err(Error::from_thread())
        } else {
            Ok(())
        }
    }
}

/// Result type of the window procedure for `WM_GETOBJECT`.
pub type GetObjectResult = LRESULT;
