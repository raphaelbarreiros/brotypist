//! Wake AppKit's event wait without calling UI/controller code on a publisher.
use crate::runtime::owner_wake::OwnerWake;
use anyhow::{Context, Result};
use objc2::{msg_send, rc::Retained, runtime::AnyObject};
use objc2_app_kit::{NSApplication, NSEvent, NSEventModifierFlags, NSEventType};
use objc2_core_foundation::{
    CFRetained, CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext, kCFRunLoopDefaultMode,
};
use objc2_foundation::{MainThreadMarker, NSPoint};
use std::{
    cell::Cell,
    ffi::c_void,
    sync::{
        Arc,
        atomic::{AtomicIsize, Ordering},
    },
};

static NEXT_WAKE_COOKIE: AtomicIsize = AtomicIsize::new(1);

struct Signal {
    source: CFRetained<CFRunLoopSource>,
    run_loop: CFRetained<CFRunLoop>,
}
// Core Foundation explicitly allows cross-thread source signalling and run-loop
// wakeup. These retained handles expose only those operations to publishers;
// their context is never read off-thread and contains no cross-thread AppKit use.
unsafe impl Send for Signal {}
unsafe impl Sync for Signal {}
impl Signal {
    fn notify(&self) {
        self.source.signal();
        self.run_loop.wake_up();
    }
}

struct Event {
    event: Retained<NSEvent>,
    cookie: isize,
    queued: Cell<bool>,
    #[cfg(feature = "native-smoke")]
    delivered: Cell<u64>,
}

/// Main-thread-only owner. Its stable context and reusable event outlive source
/// invalidation; already-entered publishers retain their own Signal allocation.
pub(super) struct AppKitWake {
    signal: Arc<Signal>,
    context: Box<Event>,
    pub(super) notifier: OwnerWake,
    _main_thread: MainThreadMarker,
}
impl AppKitWake {
    pub(super) fn new(mtm: MainThreadMarker) -> Result<Self> {
        let cookie = NEXT_WAKE_COOKIE.fetch_add(1, Ordering::Relaxed);
        let event = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::ApplicationDefined,
            NSPoint::new(0., 0.),
            NSEventModifierFlags::empty(),
            0.,
            0,
            None,
            0x4254,
            cookie,
            0,
        )
        .context("Cannot create the controller wake event.")?;
        let mut context = Box::new(Event {
            event,
            cookie,
            queued: Cell::new(false),
            #[cfg(feature = "native-smoke")]
            delivered: Cell::new(0),
        });
        let mut source_context = CFRunLoopSourceContext {
            version: 0,
            info: (&mut *context as *mut Event).cast(),
            retain: None,
            release: None,
            copyDescription: None,
            equal: None,
            hash: None,
            schedule: None,
            cancel: None,
            perform: Some(perform),
        };
        let run_loop = CFRunLoop::main().context("The main run loop is unavailable.")?;
        // The source copies its context. Box<Event> remains stable until Drop
        // invalidates the source on this same thread, before dropping the box.
        let source = unsafe { CFRunLoopSource::new(None, 0, &mut source_context) }
            .context("Cannot create the controller wake source.")?;
        // Never add this source to common modes: menu/modal tracking must not
        // consume worker results, rearm shortcuts or invoke controller ticks.
        run_loop.add_source(Some(&source), unsafe { kCFRunLoopDefaultMode });
        let signal = Arc::new(Signal { source, run_loop });
        let publisher = signal.clone();
        let notifier = OwnerWake::new(move || publisher.notify());
        Ok(Self {
            signal,
            context,
            notifier,
            _main_thread: mtm,
        })
    }

    pub(super) fn consume(&self, event: &AnyObject) -> bool {
        // AppKit copies posted events; pointer identity does not survive the
        // queue. Match only this instance's queued application-defined event.
        let kind: NSEventType = unsafe { msg_send![event, type] };
        if kind != NSEventType::ApplicationDefined || !self.context.queued.get() {
            return false;
        }
        let subtype: i16 = unsafe { msg_send![event, subtype] };
        let cookie: isize = unsafe { msg_send![event, data1] };
        if subtype == 0x4254 && cookie == self.context.cookie {
            self.context.queued.set(false);
            #[cfg(feature = "native-smoke")]
            self.context
                .delivered
                .set(self.context.delivered.get().saturating_add(1));
            #[cfg(feature = "native-smoke")]
            crate::runtime::stage_diagnostics::mark(
                crate::runtime::stage_diagnostics::Stage::AppKitWakeDelivered,
                None,
            );
            true
        } else {
            false
        }
    }

    #[cfg(feature = "native-smoke")]
    pub(super) fn delivery_count(&self) -> u64 {
        self.context.delivered.get()
    }
}
impl Drop for AppKitWake {
    fn drop(&mut self) {
        self.notifier.close();
        self.signal.source.invalidate();
    }
}

unsafe extern "C-unwind" fn perform(info: *mut c_void) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    // Registered exclusively on the main loop; source invalidation happens on
    // that same owner before this context can be freed. No controller borrow.
    let context = unsafe { &*info.cast::<Event>() };
    if !context.queued.replace(true) {
        // WakeUp alone may let nextEvent continue waiting. Posting an actual
        // private event makes it return, without allocating an event per token.
        NSApplication::sharedApplication(mtm).postEvent_atStart(&context.event, false);
    }
}
