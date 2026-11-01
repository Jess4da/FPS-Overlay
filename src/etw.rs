//! Raw ETW realtime session: counts present events per target PID.

use std::ffi::c_void;
use std::mem::{size_of, zeroed};
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering::Relaxed, Ordering::Release};
use std::thread::JoinHandle;

use windows::core::{GUID, PCWSTR, PWSTR};
use windows::Win32::Foundation::{ERROR_ALREADY_EXISTS, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Diagnostics::Etw::{
    CONTROLTRACE_HANDLE, ControlTraceW, ENABLE_TRACE_PARAMETERS, ENABLE_TRACE_PARAMETERS_VERSION_2,
    EVENT_CONTROL_CODE_DISABLE_PROVIDER, EVENT_CONTROL_CODE_ENABLE_PROVIDER, EVENT_FILTER_DESCRIPTOR,
    EVENT_FILTER_EVENT_ID, EVENT_FILTER_TYPE_EVENT_ID, EVENT_RECORD, EVENT_TRACE_CONTROL_STOP,
    EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES, EVENT_TRACE_REAL_TIME_MODE, EnableTraceEx2,
    OpenTraceW, PROCESSTRACE_HANDLE, PROCESS_TRACE_MODE_EVENT_RECORD, PROCESS_TRACE_MODE_REAL_TIME,
    ProcessTrace, StartTraceW, TRACE_LEVEL_INFORMATION, WNODE_FLAG_TRACED_GUID,
};
use windows::Win32::System::Performance::QueryPerformanceFrequency;

use crate::fps::{FpsCounter, Source};

const SESSION_NAME: &str = "FPSOverlayRs_ETW";
const NAME_CHARS: usize = 64;

// DXGI {CA11C036-0102-4A2D-A6AD-F03CFED5D3C9} event 42.
const DXGI_PROVIDER: GUID = GUID::from_u128(0xCA11C036_0102_4A2D_A6AD_F03CFED5D3C9);
// D3D9 {783ACA0A-790E-4D7F-8451-AA850511C6B9} event 1.
const D3D9_PROVIDER: GUID = GUID::from_u128(0x783ACA0A_790E_4D7F_8451_AA850511C6B9);
// DxgKrnl {802EC45A-1E99-4B83-9920-87C98277BA9D}: Present 0xB8, Flip 0xA8, Blit 0xA6.
const DXGKRNL_PROVIDER: GUID = GUID::from_u128(0x802EC45A_1E99_4B83_9920_87C98277BA9D);
const DXGKRNL_KEYWORDS: u64 = 0x8000000 | 0x1;

/// Written by the ETW consumer thread, read by the UI timer. No locks, no allocation.
pub struct Shared {
    pub target_pid: AtomicU32,
    pub fps_bits: AtomicU32,
    pub sample_pid: AtomicU32,
    /// QPC timestamp of the sample, 0 = none.
    pub sample_qpc: AtomicI64,
    /// PID for which a DXGI/D3D9 present was counted since it became the target;
    /// 0 = none. Read it against the current target, never as a bare flag: a
    /// stale `true` would keep DxgKrnl off for the next (Vulkan/GL) game.
    pub saw_pid: AtomicU32,
}

impl Shared {
    pub const fn new() -> Self {
        Self {
            target_pid: AtomicU32::new(0),
            fps_bits: AtomicU32::new(0),
            sample_pid: AtomicU32::new(0),
            sample_qpc: AtomicI64::new(0),
            saw_pid: AtomicU32::new(0),
        }
    }
}

impl Default for Shared {
    fn default() -> Self {
        Self::new()
    }
}

pub static SHARED: Shared = Shared::new();

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Providers {
    Off,
    /// DXGI + D3D9.
    UserMode,
    /// + DxgKrnl (Vulkan/OpenGL fallback).
    WithKernel,
}

pub struct Etw {
    session: CONTROLTRACE_HANDLE,
    log: PROCESSTRACE_HANDLE,
    thread: Option<JoinHandle<()>>,
    providers: Providers,
}

/// Owned by the consumer thread, reached from the callback through `UserContext`.
struct Consumer {
    shared: &'static Shared,
    counter: FpsCounter,
    /// Last `target_pid` the counter window belongs to.
    last_target: u32,
}

/// `EVENT_TRACE_PROPERTIES` followed by its inline logger-name buffer.
#[repr(C)]
struct SessionProps {
    props: EVENT_TRACE_PROPERTIES,
    name: [u16; NAME_CHARS],
}

impl SessionProps {
    fn new() -> Self {
        // SAFETY: all-zero is a valid EVENT_TRACE_PROPERTIES / name buffer.
        let mut p: Self = unsafe { zeroed() };
        for (slot, c) in p.name.iter_mut().zip(SESSION_NAME.encode_utf16()) {
            *slot = c;
        }
        p.props.Wnode.BufferSize = size_of::<Self>() as u32;
        p.props.Wnode.ClientContext = 1; // QPC timestamps
        p.props.LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;
        p
    }

    fn realtime() -> Self {
        let mut p = Self::new();
        p.props.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
        p.props.LogFileMode = EVENT_TRACE_REAL_TIME_MODE;
        p.props.BufferSize = 64; // KB
        p.props.MinimumBuffers = 4;
        p.props.MaximumBuffers = 8;
        p.props.FlushTimer = 1;
        p
    }

    fn name_ptr(&self) -> PCWSTR {
        PCWSTR(self.name.as_ptr())
    }
}

/// Stops the session by name, so a stale one from a crashed run is cleared too.
fn stop_session() {
    let mut p = SessionProps::new();
    // SAFETY: buffer is a valid, sized EVENT_TRACE_PROPERTIES with its name.
    unsafe {
        let _ = ControlTraceW(
            CONTROLTRACE_HANDLE::default(),
            p.name_ptr(),
            &mut p.props,
            EVENT_TRACE_CONTROL_STOP,
        );
    }
}

impl Etw {
    /// Opens the realtime session. All providers start disabled.
    // The logfile struct is filled field by field because its union members
    // cannot be used in a struct literal on stable Rust.
    #[allow(clippy::field_reassign_with_default)]
    pub fn start(shared: &'static Shared) -> windows::core::Result<Etw> {
        let mut freq = 0i64;
        // SAFETY: out-param only.
        unsafe { QueryPerformanceFrequency(&mut freq) }?;

        let mut session = CONTROLTRACE_HANDLE::default();
        let mut props = SessionProps::realtime();
        // SAFETY: props outlives both calls.
        let mut rc = unsafe { StartTraceW(&mut session, props.name_ptr(), &mut props.props) };
        if rc == ERROR_ALREADY_EXISTS {
            // A previous run left the session behind; stop it by name and retry once.
            stop_session();
            session = CONTROLTRACE_HANDLE::default();
            props = SessionProps::realtime();
            // SAFETY: same as above.
            rc = unsafe { StartTraceW(&mut session, props.name_ptr(), &mut props.props) };
        }
        if rc != ERROR_SUCCESS {
            return Err(rc.into());
        }

        let consumer = Box::into_raw(Box::new(Consumer {
            shared,
            counter: FpsCounter::new(freq),
            last_target: shared.target_pid.load(Relaxed),
        }));
        let mut logfile = EVENT_TRACE_LOGFILEW::default();
        logfile.LoggerName = PWSTR(props.name.as_mut_ptr());
        logfile.Context = consumer as *mut c_void;
        // Edition 2024: assigning Copy fields of a union is safe.
        logfile.Anonymous1.ProcessTraceMode =
            PROCESS_TRACE_MODE_REAL_TIME | PROCESS_TRACE_MODE_EVENT_RECORD;
        logfile.Anonymous2.EventRecordCallback = Some(on_event);
        // SAFETY: logfile is a valid, filled EVENT_TRACE_LOGFILEW.
        let log = unsafe { OpenTraceW(&mut logfile) };
        if log.Value == 0 || log.Value == u64::MAX {
            stop_session();
            // SAFETY: pointer came from Box::into_raw above and was never handed to ETW.
            drop(unsafe { Box::from_raw(consumer) });
            return Err(WIN32_ERROR(6).into()); // ERROR_INVALID_HANDLE
        }

        let consumer_addr = consumer as usize; // raw pointers are not Send
        let thread = std::thread::spawn(move || {
            // SAFETY: one realtime handle; returns when the session is stopped/closed.
            unsafe {
                let _ = ProcessTrace(&[log], None, None);
            }
            // SAFETY: the trace is finished, so no callback can reach the consumer again.
            drop(unsafe { Box::from_raw(consumer_addr as *mut Consumer) });
        });

        Ok(Etw {
            session,
            log,
            thread: Some(thread),
            providers: Providers::Off,
        })
    }

    /// Idempotent; DXGI failure is fatal, D3D9/DxgKrnl failures are ignored.
    pub fn set_providers(&mut self, p: Providers) -> windows::core::Result<()> {
        if p == self.providers {
            return Ok(());
        }
        if p == Providers::Off {
            self.disable(&DXGI_PROVIDER);
            self.disable(&D3D9_PROVIDER);
            self.disable(&DXGKRNL_PROVIDER);
            self.providers = p;
            return Ok(());
        }
        // An EVENT_FILTER_TYPE_EVENT_ID filter is not supported everywhere; the
        // callback discards by ID anyway, so plain enable is an acceptable fallback.
        if self.enable(&DXGI_PROVIDER, Some(42), 0).is_err() {
            self.enable(&DXGI_PROVIDER, None, 0)?;
        }
        if self.enable(&D3D9_PROVIDER, Some(1), 0).is_err() {
            let _ = self.enable(&D3D9_PROVIDER, None, 0);
        }
        if p == Providers::WithKernel {
            let _ = self.enable(&DXGKRNL_PROVIDER, None, DXGKRNL_KEYWORDS);
        } else {
            self.disable(&DXGKRNL_PROVIDER);
        }
        self.providers = p;
        Ok(())
    }

    fn enable(
        &self,
        provider: &GUID,
        event_id: Option<u16>,
        keywords: u64,
    ) -> windows::core::Result<()> {
        let mut data = EVENT_FILTER_EVENT_ID {
            FilterIn: true,
            Reserved: 0,
            Count: 1,
            Events: [0],
        };
        let mut desc = EVENT_FILTER_DESCRIPTOR::default();
        let mut params = ENABLE_TRACE_PARAMETERS::default();
        let params_ptr = match event_id {
            Some(id) => {
                data.Events[0] = id;
                desc.Ptr = &data as *const _ as u64;
                desc.Size = size_of::<EVENT_FILTER_EVENT_ID>() as u32;
                desc.Type = EVENT_FILTER_TYPE_EVENT_ID;
                params.Version = ENABLE_TRACE_PARAMETERS_VERSION_2;
                params.EnableFilterDesc = &mut desc;
                params.FilterDescCount = 1;
                Some(&params as *const _)
            }
            None => None,
        };
        // SAFETY: pointers refer to locals that outlive the call.
        let rc = unsafe {
            EnableTraceEx2(
                self.session,
                provider,
                EVENT_CONTROL_CODE_ENABLE_PROVIDER.0,
                TRACE_LEVEL_INFORMATION as u8,
                keywords,
                0,
                0,
                params_ptr,
            )
        };
        if rc != ERROR_SUCCESS {
            return Err(rc.into());
        }
        Ok(())
    }

    fn disable(&self, provider: &GUID) {
        // SAFETY: session handle is live until Drop.
        unsafe {
            let _ = EnableTraceEx2(
                self.session,
                provider,
                EVENT_CONTROL_CODE_DISABLE_PROVIDER.0,
                0,
                0,
                0,
                0,
                None,
            );
        }
    }
}

impl Drop for Etw {
    fn drop(&mut self) {
        // SAFETY: stopping by our own handle cannot hit an unrelated session.
        unsafe {
            let mut p = SessionProps::new();
            let _ = ControlTraceW(self.session, None, &mut p.props, EVENT_TRACE_CONTROL_STOP);
        }
        // SAFETY: closes the processing handle we opened.
        unsafe {
            let _ = windows::Win32::System::Diagnostics::Etw::CloseTrace(self.log);
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// ETW consumer callback: atomics only, no allocation.
unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    // SAFETY: ProcessTrace passes a live record whose UserContext is our Consumer.
    let rec = unsafe { &*record };
    let ctx = rec.UserContext as *mut Consumer;
    if ctx.is_null() {
        return;
    }
    // SAFETY: Consumer lives on the consumer thread for the whole trace.
    let consumer = unsafe { &mut *ctx };
    let header = &rec.EventHeader;
    let src = match header.EventDescriptor.Id {
        42 if header.ProviderId == DXGI_PROVIDER => Source::Dxgi,
        1 if header.ProviderId == D3D9_PROVIDER => Source::D3d9,
        0xB8 | 0xA8 | 0xA6 if header.ProviderId == DXGKRNL_PROVIDER => Source::Kernel,
        _ => return,
    };
    let target = consumer.shared.target_pid.load(Relaxed);
    if target != consumer.last_target {
        // A brand-new target has no history: drop the old window and its evidence,
        // even if no event ever shows up for it (Vulkan/OpenGL).
        consumer.last_target = target;
        consumer.counter.reset(target, header.TimeStamp);
        consumer.shared.saw_pid.store(0, Relaxed);
    }
    if header.ProcessId != target {
        return;
    }
    if let Some(fps) = consumer
        .counter
        .on_present(header.ProcessId, target, src, header.TimeStamp)
    {
        consumer.shared.fps_bits.store(fps.to_bits(), Relaxed);
        consumer.shared.sample_pid.store(target, Relaxed);
        // Release last: readers load sample_qpc with Acquire, then the fields above.
        consumer.shared.sample_qpc.store(header.TimeStamp, Release);
    }
    if consumer.counter.saw_user_mode() {
        consumer.shared.saw_pid.store(target, Relaxed);
    }
}

