//! The dock window as an OLE drop target: files dragged onto the Recycle Bin item go to the Recycle Bin, as when
//! they are dropped on Explorer's Recycle Bin. Anything else, or a drop elsewhere on the dock, is refused (no
//! effect), so other drags pass over the dock unchanged.

use std::cell::Cell;

use windows::Win32::Foundation::{HWND, POINTL};
use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_MOVE, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl,
    ReleaseStgMedium,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use windows::core::{Ref, implement};

#[implement(IDropTarget)]
pub struct DropTarget {
    /// The dragged data carries files (`CF_HDROP`).
    files: Cell<bool>,
}

impl Default for DropTarget {
    fn default() -> Self {
        Self::new()
    }
}

/// Writes the effect OLE asks for (a valid pointer from OLE; null is ignored).
///
/// # Safety
/// `effect` is null or points to a writable `DROPEFFECT`.
unsafe fn write_effect(effect: *mut DROPEFFECT, value: DROPEFFECT) {
    if !effect.is_null() {
        // SAFETY: the caller's contract.
        unsafe { effect.write(value) };
    }
}

impl DropTarget {
    pub fn new() -> Self {
        Self {
            files: Cell::new(false),
        }
    }

    fn effect(&self, pt: &POINTL) -> DROPEFFECT {
        if self.files.get() && crate::dock::recycle_bin_at(pt.x, pt.y) {
            DROPEFFECT_MOVE
        } else {
            DROPEFFECT_NONE
        }
    }
}

fn hdrop_format() -> FORMATETC {
    FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

/// The file paths in a `CF_HDROP` data object.
fn dropped_files(data: &IDataObject) -> Vec<String> {
    let format = hdrop_format();
    // SAFETY: the medium is released after its HDROP is read.
    unsafe {
        let Ok(mut medium) = data.GetData(&format) else {
            return Vec::new();
        };
        let drop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(drop, u32::MAX, None);
        let mut files = Vec::with_capacity(count as usize);
        for index in 0..count {
            let length = DragQueryFileW(drop, index, None) as usize;
            let mut buffer = vec![0u16; length + 1];
            let copied = DragQueryFileW(drop, index, Some(&mut buffer)) as usize;
            if copied > 0 {
                files.push(String::from_utf16_lossy(&buffer[..copied]));
            }
        }
        ReleaseStgMedium(&mut medium);
        files
    }
}

// The COM interface hands the effect over as a raw pointer (the generated trait's signature).
#[allow(clippy::not_unsafe_ptr_arg_deref)]
impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let format = hdrop_format();
        // SAFETY: a query on the dragged data object.
        let files = data
            .as_ref()
            .is_some_and(|d| unsafe { d.QueryGetData(&format) }.is_ok());
        self.files.set(files);
        // SAFETY: OLE passes a valid effect pointer.
        unsafe { write_effect(effect, self.effect(pt)) };
        Ok(())
    }

    fn DragOver(
        &self,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        // SAFETY: as above.
        unsafe { write_effect(effect, self.effect(pt)) };
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        self.files.set(false);
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<IDataObject>,
        _keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let accepted = self.effect(pt);
        self.files.set(false);
        // SAFETY: as above.
        unsafe { write_effect(effect, accepted) };
        if accepted != DROPEFFECT_MOVE {
            return Ok(());
        }
        let files = data.as_ref().map(dropped_files).unwrap_or_default();
        if files.is_empty() {
            return Ok(());
        }
        crate::dock::recycle_bin_dropped();
        crate::actions::recycle_files(files);
        Ok(())
    }
}

/// Registers `hwnd` as a drop target (OLE must be initialized on this thread).
pub fn register(hwnd: isize) {
    let target: IDropTarget = DropTarget::new().into();
    // SAFETY: our own window on its thread.
    if let Err(error) =
        unsafe { windows::Win32::System::Ole::RegisterDragDrop(HWND(hwnd as _), &target) }
    {
        ot_core::log::warn(format!("The dock cannot accept dropped files: {error}"));
    }
}

pub fn revoke(hwnd: isize) {
    // SAFETY: our own window; harmless when it was never registered.
    let _ = unsafe { windows::Win32::System::Ole::RevokeDragDrop(HWND(hwnd as _)) };
}
