//! The Audio widget's card (`AudioWidget`): a speaker icon (mute, low below 50 %, full) and the volume of the
//! default output device (`eRender`, `eMultimedia`); the device's name is the tooltip. Core Audio change
//! notifications (volume, mute, default device) wake the dock at once, so the card follows the volume keys like
//! OpenTaskbar's event-driven widget; nothing is polled.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT_SEMI_BOLD;
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, EDataFlow, ERole, IMMDevice, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator, eMultimedia, eRender,
};
use windows::Win32::System::Com::StructuredStorage::{PropVariantClear, PropVariantToStringAlloc};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::core::{PCWSTR, Result as WinResult, implement};

use super::{Canvas, Card, Env, Painter, Widget, neutral_card, text_colors, wake};

const PAD: f32 = 10.0;
const ICON: f32 = 16.0;
const ICON_GAP: f32 = 7.0;
const TEXT_SIZE: f32 = 12.5;
const TEXT_MIN: f32 = 34.0;

/// `IAudioEndpointVolumeCallback`: any volume or mute change wakes the dock.
#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback;

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(&self, _data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> WinResult<()> {
        wake();
        Ok(())
    }
}

/// `IMMNotificationClient`: a new default output device (multimedia role) is bound on the next update.
#[implement(IMMNotificationClient)]
struct DeviceCallback(Arc<AtomicBool>);

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
        if flow == eRender && role == eMultimedia {
            self.0.store(true, Ordering::SeqCst);
            wake();
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> WinResult<()> {
        Ok(())
    }
}

/// The bound default output device.
struct Endpoint {
    volume: IAudioEndpointVolume,
    callback: IAudioEndpointVolumeCallback,
    name: Option<String>,
}

impl Drop for Endpoint {
    fn drop(&mut self) {
        // SAFETY: unregistering the callback registered in `bind`.
        unsafe {
            let _ = self.volume.UnregisterControlChangeNotify(&self.callback);
        }
    }
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

/// `DockSurface_MouseWheel`: moves the default output's volume by `delta` (0..1 scale), unmuting it like
/// `AudioEndpointMonitor.SetVolume`. False when there is no device or the change failed.
pub fn nudge_volume(delta: f32) -> bool {
    // SAFETY: COM calls on the dock's thread (COM is initialized there).
    unsafe {
        let Ok(enumerator) =
            CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
        else {
            return false;
        };
        let Ok(device) = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) else {
            return false;
        };
        let Ok(volume) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else {
            return false;
        };
        let level = (volume.GetMasterVolumeLevelScalar().unwrap_or(0.0) + delta).clamp(0.0, 1.0);
        let ok = volume
            .SetMasterVolumeLevelScalar(level, std::ptr::null())
            .is_ok();
        if ok && volume.GetMute().is_ok_and(|m| m.as_bool()) {
            let _ = volume.SetMute(false, std::ptr::null());
        }
        ok
    }
}

#[derive(Default)]
pub struct Audio {
    enumerator: Option<IMMDeviceEnumerator>,
    client: Option<IMMNotificationClient>,
    device_changed: Arc<AtomicBool>,
    endpoint: Option<Endpoint>,
    started: bool,
    level: Option<i64>,
    muted: bool,
    /// The em dash without a device, the literal "0%" when muted (as OpenTaskbar), else `Percent(level)`.
    display: String,
    tooltip: String,
    name: String,
}

impl Audio {
    fn start(&mut self) {
        self.started = true;
        // SAFETY: COM is initialized on the dock's thread.
        unsafe {
            let Ok(enumerator) =
                CoCreateInstance::<_, IMMDeviceEnumerator>(&MMDeviceEnumerator, None, CLSCTX_ALL)
            else {
                ot_core::log::warn("Core Audio unavailable.");
                return;
            };
            let client: IMMNotificationClient =
                DeviceCallback(Arc::clone(&self.device_changed)).into();
            let _ = enumerator.RegisterEndpointNotificationCallback(&client);
            self.enumerator = Some(enumerator);
            self.client = Some(client);
        }
        self.bind();
    }

    /// Binds the default output device and listens to its volume.
    fn bind(&mut self) {
        self.endpoint = None;
        let Some(enumerator) = &self.enumerator else {
            return;
        };
        // SAFETY: COM calls on the dock's thread.
        unsafe {
            let Ok(device) = enumerator.GetDefaultAudioEndpoint(eRender, eMultimedia) else {
                return;
            };
            let Ok(volume) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) else {
                return;
            };
            let callback: IAudioEndpointVolumeCallback = VolumeCallback.into();
            let _ = volume.RegisterControlChangeNotify(&callback);
            let name = friendly_name(&device);
            ot_core::log::info(format!(
                "Default audio device: {}",
                name.as_deref().unwrap_or("?")
            ));
            self.endpoint = Some(Endpoint {
                volume,
                callback,
                name,
            });
        }
    }

    fn icon(&self) -> &'static str {
        match self.level {
            None => "VolumeMute",
            Some(_) if self.muted => "VolumeMute",
            Some(0) => "VolumeMute",
            Some(l) if l < 50 => "VolumeLow",
            Some(_) => "Volume",
        }
    }

    fn silent(&self) -> bool {
        self.level.is_none() || self.muted || self.level == Some(0)
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.endpoint = None;
        if let (Some(enumerator), Some(client)) = (&self.enumerator, &self.client) {
            // SAFETY: unregistering the client registered in `start`.
            unsafe {
                let _ = enumerator.UnregisterEndpointNotificationCallback(client);
            }
        }
    }
}

impl Widget for Audio {
    fn id(&self) -> &'static str {
        "Audio"
    }

    fn update(&mut self, env: &Env) -> bool {
        if !self.started {
            self.start();
        }
        if self.device_changed.swap(false, Ordering::SeqCst) {
            self.bind();
        }
        let (level, muted) = match &self.endpoint {
            // SAFETY: COM calls on the bound endpoint.
            Some(e) => unsafe {
                let scalar = e.volume.GetMasterVolumeLevelScalar().unwrap_or(0.0);
                let muted = e.volume.GetMute().map(|m| m.as_bool()).unwrap_or(false);
                (
                    Some((f64::from(scalar) * 100.0).round_ties_even() as i64),
                    muted,
                )
            },
            None => (None, false),
        };
        let tooltip = match &self.endpoint {
            Some(e) => e.name.clone().unwrap_or_else(|| env.text("AudioOutput")),
            None => env.text("NoAudioDevice"),
        };
        let name = env.text("WidgetAudio");
        let display = match level {
            None => "—".to_owned(),
            Some(_) if muted => "0%".to_owned(),
            Some(l) => env.percent(l),
        };
        let changed = level != self.level
            || muted != self.muted
            || display != self.display
            || tooltip != self.tooltip
            || name != self.name;
        self.level = level;
        self.muted = muted;
        self.display = display;
        self.tooltip = tooltip;
        self.name = name;
        changed
    }

    fn width(&self, painter: &Painter) -> WinResult<f32> {
        let text = painter.text(&self.display, TEXT_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?;
        Ok((PAD + ICON + ICON_GAP + text.width.max(TEXT_MIN) + PAD).ceil())
    }

    fn draw(&self, canvas: &Canvas) -> WinResult<()> {
        let (primary, secondary) = text_colors(canvas.dark);
        let ink = if self.silent() { secondary } else { primary };
        let h = canvas.height;
        canvas.icon(self.icon(), PAD, (h - ICON) / 2.0, ICON, ink)?;
        let text =
            canvas
                .painter
                .text(&self.display, TEXT_SIZE, DWRITE_FONT_WEIGHT_SEMI_BOLD, true)?;
        canvas.text(&text, PAD + ICON + ICON_GAP, (h - text.height) / 2.0, ink)
    }

    fn card(&self, dark: bool) -> Card {
        neutral_card(dark)
    }

    fn tooltip(&self) -> Option<String> {
        Some(self.tooltip.clone())
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    /// `ComboButton_Click`: the mixer flyout.
    fn click(&self) -> Option<super::Action> {
        Some(super::Action::Flyout)
    }
}
