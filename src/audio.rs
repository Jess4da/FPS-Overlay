//! WASAPI session enumeration, mute control and change notifications.
// Ported from BGMuter `src/audio.rs`. COM callbacks arrive on other threads and
// only post `WM_AUDIO` to the main window; no audio state is touched off-thread.

use std::collections::HashMap;
use std::ffi::c_void;
use std::sync::atomic::Ordering::Relaxed;

use crate::mute::{Mixer, Session};
use windows::Win32::Foundation::{HWND, LPARAM, PROPERTYKEY, S_OK, WPARAM};
use windows::Win32::Media::Audio::{
    AudioSessionStateExpired, DEVICE_STATE, DEVICE_STATE_ACTIVE, EDataFlow, ERole,
    IAudioSessionControl, IAudioSessionControl2, IAudioSessionManager2, IAudioSessionNotification,
    IAudioSessionNotification_Impl, IMMDeviceEnumerator, IMMNotificationClient,
    IMMNotificationClient_Impl, ISimpleAudioVolume, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::{GUID, Interface, PCWSTR, Ref, Result, implement};

/// `WPARAM` of `WM_AUDIO` for a new audio session.
pub const NOTIFY_SESSION: usize = 1;
/// `WPARAM` of `WM_AUDIO` for a device change.
pub const NOTIFY_DEVICE: usize = 2;

/// `CLSID_MMDeviceEnumerator`.
const CLSID_MM_DEVICE_ENUMERATOR: GUID = GUID::from_u128(0xBCDE_0395_E52F_467C_8E3D_C457_9291_692E);

/// COM sink for both notification interfaces. It only posts a window message:
/// callbacks arrive on COM threads and must not touch audio state.
#[implement(IMMNotificationClient, IAudioSessionNotification)]
struct NotifySink;

impl NotifySink {
    fn post(&self, kind: usize) {
        let raw = crate::MAIN_HWND.load(Relaxed);
        if raw == 0 {
            return;
        }
        // SAFETY: `raw` is our own message window when non-zero, and PostMessageW
        // only queues data; if the window is gone the post just fails harmlessly.
        unsafe {
            let _ = PostMessageW(
                Some(HWND(raw as *mut c_void)),
                crate::WM_AUDIO,
                WPARAM(kind),
                LPARAM(0),
            );
        }
    }
}

impl IMMNotificationClient_Impl for NotifySink_Impl {
    fn OnDeviceStateChanged(&self, _: &PCWSTR, _: DEVICE_STATE) -> Result<()> {
        self.post(NOTIFY_DEVICE);
        Ok(())
    }

    fn OnDeviceAdded(&self, _: &PCWSTR) -> Result<()> {
        self.post(NOTIFY_DEVICE);
        Ok(())
    }

    fn OnDeviceRemoved(&self, _: &PCWSTR) -> Result<()> {
        self.post(NOTIFY_DEVICE);
        Ok(())
    }

    fn OnDefaultDeviceChanged(&self, _: EDataFlow, _: ERole, _: &PCWSTR) -> Result<()> {
        self.post(NOTIFY_DEVICE);
        Ok(())
    }

    fn OnPropertyValueChanged(&self, _: &PCWSTR, _: &PROPERTYKEY) -> Result<()> {
        // Nothing we act on changes with a device property.
        Ok(())
    }
}

impl IAudioSessionNotification_Impl for NotifySink_Impl {
    fn OnSessionCreated(&self, _: Ref<IAudioSessionControl>) -> Result<()> {
        self.post(NOTIFY_SESSION);
        Ok(())
    }
}

/// Lives on the UI thread: COM must be an STA when this is created and dropped.
pub struct WasapiMixer {
    enumerator: IMMDeviceEnumerator,
    /// Same COM object as `session_notify`, seen through the other interface.
    notify: IMMNotificationClient,
    session_notify: IAudioSessionNotification,
    /// Endpoints whose session notifications are registered.
    managers: Vec<IAudioSessionManager2>,
    /// Session instance id -> volume control, rebuilt by `sessions`.
    cache: HashMap<String, ISimpleAudioVolume>,
}

impl WasapiMixer {
    /// Caller must have initialized COM as apartment-threaded.
    pub fn new() -> Result<Self> {
        let enumerator: IMMDeviceEnumerator =
            // SAFETY: COM is initialized on this thread by the caller.
            unsafe { CoCreateInstance(&CLSID_MM_DEVICE_ENUMERATOR, None, CLSCTX_ALL)? };
        let notify: IMMNotificationClient = NotifySink.into();
        let session_notify: IAudioSessionNotification = notify.cast()?;
        // SAFETY: the callback object outlives the registration (see Drop).
        unsafe { enumerator.RegisterEndpointNotificationCallback(&notify)? };
        let mut mixer = Self {
            enumerator,
            notify,
            session_notify,
            managers: Vec::new(),
            cache: HashMap::new(),
        };
        mixer.refresh_endpoints()?;
        Ok(mixer)
    }

    /// Re-enumerate active render endpoints and re-register session notifications.
    pub fn refresh_endpoints(&mut self) -> Result<()> {
        // Enumerate first: if it fails, the existing registrations stay in place.
        // SAFETY: plain query on a live COM object owned by this STA thread.
        let devices = unsafe {
            self.enumerator
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)?
        };
        for manager in self.managers.drain(..) {
            // SAFETY: every manager was registered with this same object.
            unsafe {
                let _ = manager.UnregisterSessionNotification(&self.session_notify);
            }
        }
        self.cache.clear();
        // SAFETY: live COM object on the owning STA thread.
        let count = unsafe { devices.GetCount()? };
        for i in 0..count {
            // SAFETY: live COM object on the owning STA thread.
            let Ok(device) = (unsafe { devices.Item(i) }) else {
                continue;
            };
            // SAFETY: live COM object on the owning STA thread.
            let Ok(manager) =
                (unsafe { device.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None) })
            else {
                continue;
            };
            // SAFETY: the callback object outlives the registration (see `Drop`).
            if unsafe { manager.RegisterSessionNotification(&self.session_notify) }.is_err() {
                continue;
            }
            // Sessions already running only start notifying after the enumerator is
            // fetched once (documented IAudioSessionManager2 requirement).
            // SAFETY: live COM object on the owning STA thread.
            let _ = unsafe { manager.GetSessionEnumerator() };
            self.managers.push(manager);
        }
        Ok(())
    }
}

impl Mixer for WasapiMixer {
    fn sessions(&mut self) -> Vec<Session> {
        self.cache.clear();
        let mut out = Vec::new();
        for manager in &self.managers {
            // SAFETY: live COM object on the owning STA thread.
            let Ok(enumerator) = (unsafe { manager.GetSessionEnumerator() }) else {
                continue;
            };
            // SAFETY: live COM object on the owning STA thread.
            let count = unsafe { enumerator.GetCount() }.unwrap_or(0);
            for i in 0..count {
                // SAFETY: live COM object on the owning STA thread.
                let Ok(control) = (unsafe { enumerator.GetSession(i) }) else {
                    continue;
                };
                // Nothing here is worth muting: expired sessions are gone.
                // SAFETY: live COM object on the owning STA thread.
                match unsafe { control.GetState() } {
                    Ok(state) if state != AudioSessionStateExpired => {}
                    _ => continue,
                }
                let Ok(control2) = control.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                // SAFETY: live COM object on the owning STA thread.
                if unsafe { control2.IsSystemSoundsSession() } == S_OK {
                    continue;
                }
                // SAFETY: live COM object on the owning STA thread.
                let Ok(pid) = (unsafe { control2.GetProcessId() }) else {
                    continue;
                };
                let Some(exe) = crate::process::exe_name(pid) else {
                    continue;
                };
                let Ok(volume) = control.cast::<ISimpleAudioVolume>() else {
                    continue;
                };
                // SAFETY: live COM object on the owning STA thread.
                let Ok(raw_id) = (unsafe { control2.GetSessionInstanceIdentifier() }) else {
                    continue;
                };
                // SAFETY: the id is valid until freed; copy it out first.
                let id = unsafe { raw_id.to_string() };
                // SAFETY: the buffer came from GetSessionInstanceIdentifier, the session
                // is still alive, and the pointer is freed exactly once here.
                unsafe { CoTaskMemFree(Some(raw_id.0.cast())) };
                let Ok(id) = id else {
                    continue;
                };
                // SAFETY: live COM object on the owning STA thread.
                let Ok(muted) = (unsafe { volume.GetMute() }) else {
                    continue;
                };
                let muted = muted.as_bool();
                self.cache.insert(id.clone(), volume);
                out.push(Session { id, exe, muted });
            }
        }
        out
    }

    fn set_mute(&mut self, id: &str, mute: bool) -> bool {
        let Some(volume) = self.cache.get(id) else {
            return false;
        };
        // SAFETY: null event context, as documented for the aggregate control.
        unsafe { volume.SetMute(mute, std::ptr::null()).is_ok() }
    }
}

impl Drop for WasapiMixer {
    fn drop(&mut self) {
        for manager in &self.managers {
            // SAFETY: matches the registration in `refresh_endpoints`.
            unsafe {
                let _ = manager.UnregisterSessionNotification(&self.session_notify);
            }
        }
        // SAFETY: matches the registration in `new`.
        unsafe {
            let _ = self
                .enumerator
                .UnregisterEndpointNotificationCallback(&self.notify);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

    #[test]
    #[ignore = "requires a real audio endpoint and a playing app"]
    fn lists_live_sessions() {
        // SAFETY: this thread owns the apartment it initializes and uninitializes.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
        {
            let mut mixer = WasapiMixer::new().expect("create mixer");
            let sessions = mixer.sessions();
            for s in &sessions {
                println!("exe={:?} muted={} id={}", s.exe, s.muted, s.id);
            }
            println!("{} session(s)", sessions.len());
            assert!(!sessions.is_empty(), "play some audio");
            assert!(sessions.iter().all(|s| !s.exe.is_empty()));
        }
        // SAFETY: balances the initialization above.
        unsafe { CoUninitialize() };
    }
}
