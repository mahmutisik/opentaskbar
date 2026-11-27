//! The Audio widget's mixer flyout (`AudioWidget.BuildFlyoutContent`): a title, then Output and Input sections
//! (heading with icon and level, a device combo box with a mute button, a volume slider) and the Applications
//! section (one row per audio session: icon, name, mute button, slider, level), 314 DIP wide.
//!
//! The flyout binds its own endpoints and listens to them (as OpenTaskbar's shared `AudioEndpointMonitor`), so the
//! sliders follow the volume keys while it is open. Choosing a device makes it the default for every role
//! (`IPolicyConfig.SetDefaultEndpoint`, what Windows' own sound settings use). App sessions are collected when the
//! flyout opens and held until it closes (`ReleaseAppSessions`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};

use ot_core::language::{AppLanguage, Catalog};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    AUDIO_VOLUME_NOTIFICATION_DATA, AudioSessionStateExpired, DEVICE_STATE, DEVICE_STATE_ACTIVE,
    EDataFlow, ERole, IAudioSessionControl, IAudioSessionControl2, IAudioSessionManager2,
    IMMDevice, IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl,
    ISimpleAudioVolume, MMDeviceEnumerator, eCapture, eMultimedia, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::core::{GUID, HRESULT, IUnknown, Interface, PCWSTR, Result as WinResult, implement};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::icons::Icon;
use crate::panel::{self, Align, Button, Content, Element, Kind, Ui, Weight};

const WIDTH: f32 = 314.0;
/// The mixer's app list scrolls past 160 DIP.
const APPS_MAX_HEIGHT: f32 = 160.0;

const OUT_COMBO: u32 = 1;
const OUT_MUTE: u32 = 2;
const OUT_SLIDER: u32 = 3;
const IN_COMBO: u32 = 11;
const IN_MUTE: u32 = 12;
const IN_SLIDER: u32 = 13;
/// App row `i`: mute `APP + 4i`, slider `+1`, name `+2` (tooltip only).
const APP: u32 = 100;

/// The event context of the flyout's own changes.
const CONTEXT: GUID = GUID::from_u128(0x6f1d_2b3a_4c5e_4f60_8a7b_9c0d_1e2f_3a4b);

/// Wakes the flyout: posts `WM_PANEL_REFRESH` to it.
#[derive(Clone, Default)]
struct Waker(Arc<AtomicIsize>);

impl Waker {
    fn wake(&self) {
        let hwnd = self.0.load(Ordering::SeqCst);
        if hwnd != 0 {
            // SAFETY: posting to the flyout window (any thread may post).
            unsafe { PostMessageW(hwnd as HWND, panel::WM_PANEL_REFRESH, 0, 0) };
        }
    }
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback(Waker);

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(&self, _data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> WinResult<()> {
        self.0.wake();
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DeviceCallback(Waker, Arc<AtomicBool>, Arc<AtomicBool>);

impl IMMNotificationClient_Impl for DeviceCallback_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> WinResult<()> {
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> WinResult<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> WinResult<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _id: &PCWSTR) -> WinResult<()> {
        if role == eMultimedia {
            if flow == eRender {
                self.1.store(true, Ordering::SeqCst);
            } else if flow == eCapture {
                self.2.store(true, Ordering::SeqCst);
            }
            self.0.wake();
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> WinResult<()> {
        Ok(())
    }
}

/// A bound default endpoint and its change callback.
struct Endpoint {
    volume: IAudioEndpointVolume,
    callback: IAudioEndpointVolumeCallback,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        // SAFETY: unregistering the callback registered in `bind`.
        unsafe {
            let _ = self.volume.UnregisterControlChangeNotify(&self.callback);
        }
    }
}

impl Endpoint {
    /// (level 0..=100 rounded, muted).
    fn read(&self) -> (i64, bool) {
        // SAFETY: COM calls on the bound endpoint.
        unsafe {
            let scalar = self.volume.GetMasterVolumeLevelScalar().unwrap_or(0.0);
            let muted = self.volume.GetMute().map(|m| m.as_bool()).unwrap_or(false);
            ((f64::from(scalar) * 100.0).round_ties_even() as i64, muted)
        }
    }

    /// `SetVolume`: also unmutes.
    fn set(&self, level: f64) -> bool {
        // SAFETY: COM calls on the bound endpoint.
        unsafe {
            let ok = self
                .volume
                .SetMasterVolumeLevelScalar((level / 100.0).clamp(0.0, 1.0) as f32, &CONTEXT)
                .is_ok();
            if ok && self.volume.GetMute().is_ok_and(|m| m.as_bool()) {
                let _ = self.volume.SetMute(false, &CONTEXT);
            }
            ok
        }
    }

    fn toggle_mute(&self) -> bool {
        let (_, muted) = self.read();
        // SAFETY: COM call on the bound endpoint.
        unsafe { self.volume.SetMute(!muted, &CONTEXT).is_ok() }
    }
}

struct Device {
    id: String,
    name: String,
}

/// An app's audio session, held while the flyout is open.
struct Session {
    _control: IAudioSessionControl,
    volume: ISimpleAudioVolume,
    name: String,
    icon: Option<Arc<Icon>>,
    /// The slider's value (0..=100) and the session's mute state as the row shows them.
    level: f64,
    muted: bool,
}

pub struct AudioFlyout {
    language: AppLanguage,
    waker: Waker,
    enumerator: Option<IMMDeviceEnumerator>,
    client: Option<IMMNotificationClient>,
    output_changed: Arc<AtomicBool>,
    input_changed: Arc<AtomicBool>,
    output: Option<Endpoint>,
    input: Option<Endpoint>,
    outputs: Vec<Device>,
    inputs: Vec<Device>,
    output_default: Option<usize>,
    input_default: Option<usize>,
    sessions: Vec<Session>,
}

fn friendly_name(device: &IMMDevice) -> Option<String> {
    // SAFETY: COM calls; the string is freed with CoTaskMemFree and the value cleared.
    unsafe {
        let store = device.OpenPropertyStore(STGM_READ).ok()?;
        let mut value = store.GetValue(&PKEY_Device_FriendlyName).ok()?;
        let text = PropVariantToStringAlloc(&value);
        let _ = PropVariantClear(&mut value);
        let text = text.ok()?;
        let name = text.to_string().ok();
        CoTaskMemFree(Some(text.0 as *const _));
        name.filter(|n| !n.trim().is_empty())
    }
}

fn device_id(device: &IMMDevice) -> Option<String> {
    // SAFETY: the id string is freed with CoTaskMemFree.
    unsafe {
        let id = device.GetId().ok()?;
        let text = id.to_string().ok();
        CoTaskMemFree(Some(id.0 as *const _));
        text
    }
}

/// `IsHeadphoneDevice`.
fn is_headphones(name: Option<&str>) -> bool {
    let Some(name) = name else {
        return false;
    };
    let lower = name.to_lowercase();
    ["headphone", "kulaklık", "headset", "buds", "airpod"]
        .iter()
        .any(|k| lower.contains(k))
}

/// `AudioEndpointMonitor.SetDefaultDevice`: the device becomes the default for the console, multimedia and
/// communications roles (`IPolicyConfig`, undocumented but what the Sound settings use).
fn set_default_device(id: &str) -> bool {
    const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
    const IID_POLICY_CONFIG: GUID = GUID::from_u128(0xF8679F50_850A_41CF_9C72_430F290290C8);
    // SAFETY: COM creation; the vtable slot 13 is `SetDefaultEndpoint(LPCWSTR, ERole)` (IUnknown's three, then
    // ten `IPolicyConfig` methods before it); the pointer is released through the `IUnknown` wrapper.
    unsafe {
        let Ok(unknown) = CoCreateInstance::<_, IUnknown>(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL)
        else {
            ot_core::log::warn("SetDefaultEndpoint failed: no policy config.");
            return false;
        };
        let mut raw = std::ptr::null_mut();
        if unknown.query(&IID_POLICY_CONFIG, &mut raw).is_err() || raw.is_null() {
            ot_core::log::warn("SetDefaultEndpoint failed: no IPolicyConfig.");
            return false;
        }
        let policy = IUnknown::from_raw(raw);
        let vtable = *(raw as *const *const usize);
        let set: extern "system" fn(*mut std::ffi::c_void, PCWSTR, i32) -> HRESULT =
            std::mem::transmute(*vtable.add(13));
        let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
        for role in [0, 1, 2] {
            let result = set(policy.as_raw(), PCWSTR(wide.as_ptr()), role);
            if result.is_err() {
                ot_core::log::warn(format!("SetDefaultEndpoint failed: {result:?}"));
            }
        }
        true
    }
}

impl AudioFlyout {
    pub fn new(language: AppLanguage) -> Self {
        let mut flyout = Self {
            language,
            waker: Waker::default(),
            enumerator: None,
            client: None,
            output_changed: Arc::new(AtomicBool::new(false)),
            input_changed: Arc::new(AtomicBool::new(false)),
            output: None,
            input: None,
            outputs: Vec::new(),
            inputs: Vec::new(),
            output_default: None,
            input_default: None,
            sessions: Vec::new(),
        };
        // SAFETY: COM is initialized on the dock's thread.
        unsafe {
            match CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
            {
                Ok(enumerator) => {
                    let client: IMMNotificationClient = DeviceCallback(
                        flyout.waker.clone(),
                        Arc::clone(&flyout.output_changed),
                        Arc::clone(&flyout.input_changed),
                    )
                    .into();
                    let _ = enumerator.RegisterEndpointNotificationCallback(&client);
                    flyout.enumerator = Some(enumerator);
                    flyout.client = Some(client);
                }
                Err(error) => ot_core::log::warn(format!("Core Audio unavailable: {error}")),
            }
        }
        flyout.output = flyout.bind(eRender);
        flyout.input = flyout.bind(eCapture);
        flyout.populate();
        flyout
    }

    fn text(&self, key: &str) -> String {
        Catalog::get(key, &self.language).into_owned()
    }

    fn percent(&self, n: i64) -> String {
        self.text("Percent").replace("{0}", &n.to_string())
    }

    /// `BindDefaultDevice` / `BindDefaultMicDevice`.
    fn bind(&self, flow: EDataFlow) -> Option<Endpoint> {
        let enumerator = self.enumerator.as_ref()?;
        // SAFETY: COM calls on the dock's thread.
        unsafe {
            let device = enumerator.GetDefaultAudioEndpoint(flow, eMultimedia).ok()?;
            let volume = device
                .Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)
                .ok()?;
            let callback: IAudioEndpointVolumeCallback = VolumeCallback(self.waker.clone()).into();
            let _ = volume.RegisterControlChangeNotify(&callback);
            Some(Endpoint { volume, callback })
        }
    }

    /// `GetDevices`: the active endpoints of `flow` and the default one's index.
    fn devices(&self, flow: EDataFlow) -> (Vec<Device>, Option<usize>) {
        let Some(enumerator) = self.enumerator.as_ref() else {
            return (Vec::new(), None);
        };
        let mut list = Vec::new();
        // SAFETY: COM calls on the dock's thread.
        unsafe {
            let default = enumerator
                .GetDefaultAudioEndpoint(flow, eMultimedia)
                .ok()
                .and_then(|d| device_id(&d));
            if let Ok(collection) = enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) {
                let count = collection.GetCount().unwrap_or(0);
                for i in 0..count {
                    let Ok(device) = collection.Item(i) else {
                        continue;
                    };
                    let Some(id) = device_id(&device) else {
                        continue;
                    };
                    let name = friendly_name(&device).unwrap_or_else(|| {
                        if flow == eCapture {
                            "Microphone".to_owned()
                        } else {
                            "Speaker".to_owned()
                        }
                    });
                    list.push(Device { id, name });
                }
            }
            let index =
                default.and_then(|d| list.iter().position(|x| x.id.eq_ignore_ascii_case(&d)));
            (list, index)
        }
    }

    /// `PopulateFlyout`'s data: device lists and the app sessions.
    fn populate(&mut self) {
        (self.outputs, self.output_default) = self.devices(eRender);
        (self.inputs, self.input_default) = self.devices(eCapture);
        self.sessions = self.collect_sessions();
    }

    /// `GetAppSessions()`: the default output's sessions first, then every active output's; one row per process
    /// (system sounds once), never the dock's own.
    fn collect_sessions(&self) -> Vec<Session> {
        let mut sessions = Vec::new();
        let Some(enumerator) = self.enumerator.as_ref() else {
            return sessions;
        };
        let mut seen_pids = std::collections::HashSet::new();
        let mut seen_names = std::collections::HashSet::new();
        let own = std::process::id();
        let mut devices = Vec::new();
        // SAFETY: COM calls on the dock's thread.
        unsafe {
            if let Ok(d) = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) {
                devices.push(d);
            }
            if let Ok(collection) = enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE) {
                for i in 0..collection.GetCount().unwrap_or(0) {
                    if let Ok(d) = collection.Item(i) {
                        devices.push(d);
                    }
                }
            }
            for device in devices {
                let Ok(manager) = device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) else {
                    continue;
                };
                let Ok(list) = manager.GetSessionEnumerator() else {
                    continue;
                };
                for i in 0..list.GetCount().unwrap_or(0) {
                    let Ok(control) = list.GetSession(i) else {
                        continue;
                    };
                    let (Ok(control2), Ok(volume)) = (
                        control.cast::<IAudioSessionControl2>(),
                        control.cast::<ISimpleAudioVolume>(),
                    ) else {
                        continue;
                    };
                    if control2
                        .GetState()
                        .map_or(true, |s| s == AudioSessionStateExpired)
                    {
                        continue;
                    }
                    let system =
                        control2.IsSystemSoundsSession() == windows::Win32::Foundation::S_OK;
                    let pid = control2.GetProcessId().unwrap_or(0);
                    if pid == own {
                        continue;
                    }
                    if pid > 0 && !seen_pids.insert(pid) {
                        continue;
                    }
                    let (name, icon) = if system {
                        if !seen_names.insert("SystemSounds".to_owned()) {
                            continue;
                        }
                        (
                            self.text("SystemSounds"),
                            crate::jumplist::resource_icon(
                                r"%SystemRoot%\System32\audiosrv.dll",
                                -1,
                                32,
                            ),
                        )
                    } else if pid > 0 {
                        match ot_shell::win::process_path(pid) {
                            Some(path) => {
                                let stem = std::path::Path::new(&path)
                                    .file_stem()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| format!("App ({pid})"));
                                let icon = crate::jumplist::resource_icon(&path, 0, 32)
                                    .or_else(|| crate::icons::load(&path, 32));
                                (stem, icon)
                            }
                            None => (format!("App ({pid})"), None),
                        }
                    } else {
                        let display = control2
                            .GetDisplayName()
                            .ok()
                            .and_then(|p| {
                                let s = p.to_string().ok();
                                CoTaskMemFree(Some(p.0 as *const _));
                                s
                            })
                            .filter(|s| !s.trim().is_empty())
                            .unwrap_or_else(|| "Audio".to_owned());
                        if !seen_names.insert(display.clone()) {
                            continue;
                        }
                        (display, None)
                    };
                    let level = f64::from(volume.GetMasterVolume().unwrap_or(0.0)) * 100.0;
                    let muted = volume.GetMute().map(|m| m.as_bool()).unwrap_or(false);
                    sessions.push(Session {
                        _control: control,
                        volume,
                        name,
                        icon: icon.map(Arc::new),
                        level: level.round(),
                        muted,
                    });
                }
            }
        }
        sessions
    }

    /// Rebinds after a default device change (`Monitor_DefaultDeviceChanged`: the flyout is rebuilt).
    fn follow_defaults(&mut self) {
        let output = self.output_changed.swap(false, Ordering::SeqCst);
        let input = self.input_changed.swap(false, Ordering::SeqCst);
        if output {
            self.output = self.bind(eRender);
        }
        if input {
            self.input = self.bind(eCapture);
        }
        if output || input {
            self.populate();
        }
    }

    fn mute_button(&self, ui: &Ui, icon: &'static str, muted: bool, enabled: bool) -> Button {
        let p = ui.palette;
        let (foreground, border, background) = if muted {
            (p.danger_text, p.danger_stroke, p.danger_hover)
        } else {
            (p.text_secondary, p.stroke_subtle, p.surface_selected)
        };
        Button {
            icon,
            icon_size: 15.0,
            foreground,
            background,
            border: Some(border),
            radius: 8.0,
            hover: p.widget_hover,
            pressed: p.widget_pressed,
            enabled,
        }
    }

    /// One endpoint section; returns its bottom.
    #[allow(clippy::too_many_arguments)]
    fn section(
        &self,
        ui: &mut Ui,
        top: f32,
        input: bool,
        heading_icon: &'static str,
        label: &str,
        ids: (u32, u32, u32),
    ) -> WinResult<f32> {
        let p = ui.palette.clone();
        let endpoint = if input { &self.input } else { &self.output };
        let read = endpoint.as_ref().map(Endpoint::read);
        let level_text = match read {
            None => "\u{2014}".to_owned(),
            Some((_, true)) => "0%".to_owned(),
            Some((l, false)) => self.percent(l),
        };
        // Heading: icon + label on the left, the level docked right.
        let level = ui.text(&level_text, 11.5, Weight::SemiBold, true, None)?;
        let name = ui.text(label, 12.0, Weight::SemiBold, false, None)?;
        let row = level.height.max(name.height).max(15.0);
        let icon_top = top + (row - 15.0) / 2.0;
        ui.push(Element {
            id: 0,
            rect: (0.0, icon_top, 15.0, icon_top + 15.0),
            kind: Kind::Icon {
                name: heading_icon,
                color: p.text_secondary,
            },
            tip: None,
            name: None,
            scrolled: false,
        });
        ui.push(Element {
            id: 0,
            rect: (22.0, top, WIDTH - level.width, top + row),
            kind: Kind::Text {
                text: name,
                color: p.text,
                align: Align::Left,
            },
            tip: None,
            name: None,
            scrolled: false,
        });
        ui.push(Element {
            id: 0,
            rect: (WIDTH - level.width, top, WIDTH, top + row),
            kind: Kind::Text {
                text: level,
                color: p.text_secondary,
                align: Align::Right,
            },
            tip: None,
            name: None,
            scrolled: false,
        });
        let mut y = top + row + 8.0;

        // The device combo box and the mute button.
        let (devices, selected) = if input {
            (&self.inputs, self.input_default)
        } else {
            (&self.outputs, self.output_default)
        };
        let combo_text = if devices.is_empty() {
            self.text(if input {
                "NoMicDevice"
            } else {
                "NoAudioDevice"
            })
        } else {
            devices[selected.unwrap_or(0)].name.clone()
        };
        let text = ui.text(&combo_text, 11.5, Weight::SemiBold, false, None)?;
        ui.push(Element {
            id: ids.0,
            rect: (0.0, y, WIDTH - 32.0 - 8.0, y + 32.0),
            kind: Kind::Combo {
                text,
                enabled: !devices.is_empty(),
            },
            tip: None,
            name: Some(self.text(if input {
                "AudioInputDevices"
            } else {
                "AudioOutputDevices"
            })),
            scrolled: false,
        });
        let muted = read.is_some_and(|(_, m)| m);
        let icon = if input {
            if muted { "MicMute" } else { "Mic" }
        } else {
            match read {
                None => "VolumeMute",
                Some((l, m)) if m || l == 0 => "VolumeMute",
                Some((l, _)) if l < 50 => "VolumeLow",
                Some(_) => "Volume",
            }
        };
        let tip = self.text(if input { "MicMuteToggle" } else { "MuteToggle" });
        ui.push(Element {
            id: ids.1,
            rect: (WIDTH - 32.0, y, WIDTH, y + 32.0),
            kind: Kind::Button(self.mute_button(ui, icon, muted, read.is_some())),
            tip: Some(tip.clone()),
            name: Some(tip),
            scrolled: false,
        });
        y += 32.0 + 8.0;

        // The volume slider (0 while muted).
        let value = match read {
            Some((_, true)) | None => 0.0,
            Some((l, false)) => l as f64,
        };
        ui.push(Element {
            id: ids.2,
            rect: (0.0, y, WIDTH, y + panel::SLIDER_HEIGHT),
            kind: Kind::Slider {
                value,
                enabled: read.is_some(),
            },
            tip: None,
            name: Some(label.to_owned()),
            scrolled: false,
        });
        Ok(y + panel::SLIDER_HEIGHT)
    }

    /// `CreateSectionDivider`: 1 DIP `StrokeSubtle`, 11 DIP above and below; returns the next top.
    fn divider(ui: &mut Ui, top: f32) -> f32 {
        let color = ui.palette.stroke_subtle;
        ui.push(Element {
            id: 0,
            rect: (0.0, top + 11.0, WIDTH, top + 12.0),
            kind: Kind::Fill(color),
            tip: None,
            name: None,
            scrolled: false,
        });
        top + 23.0
    }

    fn apps(&self, ui: &mut Ui, top: f32) -> WinResult<f32> {
        let p = ui.palette.clone();
        let header = ui.text(&self.text("AudioApps"), 12.0, Weight::SemiBold, false, None)?;
        let h = header.height;
        ui.push(Element {
            id: 0,
            rect: (0.0, top, WIDTH, top + h),
            kind: Kind::Text {
                text: header,
                color: p.text,
                align: Align::Left,
            },
            tip: None,
            name: None,
            scrolled: false,
        });
        let mut y = top + h + 8.0;
        if self.sessions.is_empty() {
            let empty = ui.text(
                &self.text("NoActiveAudioApps"),
                11.5,
                Weight::Normal,
                false,
                None,
            )?;
            let h = empty.height;
            ui.push(Element {
                id: 0,
                rect: (0.0, y + 2.0, WIDTH, y + 2.0 + h),
                kind: Kind::Text {
                    text: empty,
                    color: p.text_secondary,
                    align: Align::Left,
                },
                tip: None,
                name: None,
                scrolled: false,
            });
            return Ok(y + 2.0 + h + 2.0);
        }
        // Rows: margin 3 above and below, at least 27 tall; columns 24 | 82 | 24 | * | 34.
        const ROW: f32 = 27.0;
        let extent = self.sessions.len() as f32 * (ROW + 6.0);
        let width = ui.scroll_width(extent, APPS_MAX_HEIGHT);
        ui.begin_scroll(y, APPS_MAX_HEIGHT);
        let mute_tip = self.text("MuteToggle");
        for (i, session) in self.sessions.iter().enumerate() {
            let id = APP + 4 * i as u32;
            let top = y + 3.0;
            let mid = top + ROW / 2.0;
            match &session.icon {
                Some(icon) => ui.push(Element {
                    id: 0,
                    rect: (0.0, mid - 9.0, 18.0, mid + 9.0),
                    kind: Kind::Image(Arc::clone(icon)),
                    tip: None,
                    name: None,
                    scrolled: false,
                }),
                None => ui.push(Element {
                    id: 0,
                    rect: (0.0, mid - 7.5, 15.0, mid + 7.5),
                    kind: Kind::Icon {
                        name: "Sliders",
                        color: p.text_secondary,
                    },
                    tip: None,
                    name: None,
                    scrolled: false,
                }),
            }
            // The name: margin 2,0,4,0 in its 82 DIP column, trimmed, its tooltip the full name.
            let name = ui.text(&session.name, 11.5, Weight::Medium, false, Some(76.0))?;
            ui.push(Element {
                id: id + 2,
                rect: (26.0, top, 102.0, top + ROW),
                kind: Kind::Text {
                    text: name,
                    color: p.text,
                    align: Align::Left,
                },
                tip: Some(session.name.clone()),
                name: None,
                scrolled: false,
            });
            let silent = session.muted || session.level.round() == 0.0;
            ui.push(Element {
                id,
                rect: (107.0, mid - 11.0, 129.0, mid + 11.0),
                kind: Kind::Button(Button {
                    icon: if silent { "VolumeMute" } else { "Volume" },
                    icon_size: 13.0,
                    foreground: p.text,
                    background: windows::UI::Color::default(),
                    border: None,
                    radius: 8.0,
                    hover: p.widget_hover,
                    pressed: p.widget_pressed,
                    enabled: true,
                }),
                tip: Some(mute_tip.clone()),
                name: Some(format!("{}: {}", session.name, mute_tip)),
                scrolled: false,
            });
            ui.push(Element {
                id: id + 1,
                rect: (134.0, mid - 6.0, width - 34.0 - 4.0, mid + 6.0),
                kind: Kind::Slider {
                    value: session.level,
                    enabled: true,
                },
                tip: None,
                name: Some(session.name.clone()),
                scrolled: false,
            });
            let level_text = if session.muted {
                "\u{2014}".to_owned()
            } else {
                self.percent(session.level.round() as i64)
            };
            let level = ui.text(&level_text, 11.0, Weight::SemiBold, true, None)?;
            ui.push(Element {
                id: 0,
                rect: (width - 34.0, top, width, top + ROW),
                kind: Kind::Text {
                    text: level,
                    color: p.text_secondary,
                    align: Align::Right,
                },
                tip: None,
                name: None,
                scrolled: false,
            });
            y += ROW + 6.0;
        }
        Ok(ui.end_scroll(y))
    }
}

impl Drop for AudioFlyout {
    fn drop(&mut self) {
        self.sessions.clear();
        self.output = None;
        self.input = None;
        if let (Some(enumerator), Some(client)) = (&self.enumerator, &self.client) {
            // SAFETY: unregistering the client registered in `new`.
            unsafe {
                let _ = enumerator.UnregisterEndpointNotificationCallback(client);
            }
        }
    }
}

impl Content for AudioFlyout {
    fn width(&self) -> f32 {
        WIDTH
    }

    fn attach(&mut self, panel: isize) {
        self.waker.0.store(panel, Ordering::SeqCst);
    }

    fn build(&mut self, ui: &mut Ui) -> WinResult<()> {
        self.follow_defaults();
        let p = ui.palette.clone();
        let title = ui.text(&self.text("WidgetAudio"), 12.5, Weight::Bold, false, None)?;
        let h = title.height;
        ui.push(Element {
            id: 0,
            rect: (0.0, 0.0, WIDTH, h),
            kind: Kind::Text {
                text: title,
                color: p.text,
                align: Align::Left,
            },
            tip: None,
            name: None,
            scrolled: false,
        });
        let mut y = h + 10.0;
        let output_name = self
            .output_default
            .and_then(|i| self.outputs.get(i))
            .map(|d| d.name.as_str());
        let heading = if is_headphones(output_name) {
            "Headphones"
        } else {
            "Volume"
        };
        y = self.section(
            ui,
            y,
            false,
            heading,
            &self.text("AudioOutput"),
            (OUT_COMBO, OUT_MUTE, OUT_SLIDER),
        )?;
        y = Self::divider(ui, y);
        y = self.section(
            ui,
            y,
            true,
            "Mic",
            &self.text("AudioInput"),
            (IN_COMBO, IN_MUTE, IN_SLIDER),
        )?;
        y = Self::divider(ui, y);
        self.apps(ui, y)?;
        Ok(())
    }

    fn click(&mut self, id: u32) {
        match id {
            OUT_MUTE => {
                if let Some(e) = &self.output
                    && !e.toggle_mute()
                {
                    ot_core::log::warn(self.text("ErrorVolumeChange"));
                }
            }
            IN_MUTE => {
                if let Some(e) = &self.input {
                    e.toggle_mute();
                }
            }
            id if id >= APP && (id - APP).is_multiple_of(4) => {
                let i = ((id - APP) / 4) as usize;
                if let Some(s) = self.sessions.get_mut(i) {
                    let mute = !s.muted;
                    // SAFETY: COM call on a held session.
                    if unsafe { s.volume.SetMute(mute, &CONTEXT) }.is_ok() {
                        s.muted = mute;
                    } else {
                        ot_core::log::warn(Catalog::get("ErrorVolumeChange", &self.language));
                    }
                }
            }
            _ => {}
        }
    }

    fn slide(&mut self, id: u32, value: f64) {
        match id {
            OUT_SLIDER => {
                if let Some(e) = &self.output
                    && !e.set(value)
                {
                    ot_core::log::warn(self.text("ErrorVolumeChange"));
                }
            }
            IN_SLIDER => {
                if let Some(e) = &self.input {
                    e.set(value);
                }
            }
            id if id >= APP && (id - APP) % 4 == 1 => {
                let i = ((id - APP) / 4) as usize;
                let language = self.language.clone();
                if let Some(s) = self.sessions.get_mut(i) {
                    // SAFETY: COM calls on a held session.
                    unsafe {
                        if s.volume
                            .SetMasterVolume((value / 100.0).clamp(0.0, 1.0) as f32, &CONTEXT)
                            .is_err()
                        {
                            ot_core::log::warn(Catalog::get("ErrorVolumeChange", &language));
                            return;
                        }
                        s.level = value;
                        if s.muted
                            && value.round() > 0.0
                            && s.volume.SetMute(false, &CONTEXT).is_ok()
                        {
                            s.muted = false;
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn choices(&self, id: u32) -> (Vec<String>, Option<usize>) {
        let (devices, selected) = match id {
            OUT_COMBO => (&self.outputs, self.output_default),
            IN_COMBO => (&self.inputs, self.input_default),
            _ => return (Vec::new(), None),
        };
        (
            devices.iter().map(|d| d.name.clone()).collect(),
            selected.or(Some(0)).filter(|_| !devices.is_empty()),
        )
    }

    fn select(&mut self, id: u32, index: usize) {
        let devices = match id {
            OUT_COMBO => &self.outputs,
            IN_COMBO => &self.inputs,
            _ => return,
        };
        let Some(device) = devices.get(index) else {
            return;
        };
        let current = if id == OUT_COMBO {
            self.output_default
        } else {
            self.input_default
        };
        if current == Some(index) {
            return;
        }
        let device_id = device.id.clone();
        if set_default_device(&device_id) {
            // The default-device notification rebinds and repopulates; do it now too, so the flyout redraws at once.
            if id == OUT_COMBO {
                self.output = self.bind(eRender);
            } else {
                self.input = self.bind(eCapture);
            }
            self.populate();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headphone_names() {
        assert!(is_headphones(Some("Kulaklık (Realtek(R) Audio)")));
        assert!(is_headphones(Some("Galaxy Buds2 Pro")));
        assert!(is_headphones(Some("Headset Earphone (Jabra)")));
        assert!(!is_headphones(Some("Hoparlör (Realtek(R) Audio)")));
        assert!(!is_headphones(None));
    }
}
