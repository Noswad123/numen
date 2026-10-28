//! System-wide Option+I reveal shortcut, owned by the main-thread winit loop.
//!
//! Create the winit event loop before registering, keep this guard alive while
//! it runs, and drop it on the main thread. The callback must only post the
//! caller's coalesced reveal event; it must not block or manipulate windows.
//! This module is the sole owner of global-hotkey's process-global handler.

#[cfg(target_os = "macos")]
use macos::Registration;

/// Keeps the system-wide reveal shortcut and its callback alive.
pub struct GlobalRevealHotkey {
    #[cfg(target_os = "macos")]
    _registration: Registration,
}

impl GlobalRevealHotkey {
    /// Register Option+I after creating the main-thread winit event loop.
    ///
    /// Registration errors (including shortcut collisions) are recoverable:
    /// callers should warn and retain their CLI/local-shortcut fallback.
    pub fn register(on_reveal: impl Fn() + Send + Sync + 'static) -> anyhow::Result<Self> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                _registration: Registration::new(on_reveal)?,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = on_reveal;
            anyhow::bail!(
                "System-wide Option+I is supported only on macOS; use the reveal CLI command \
                 or the local window shortcut on this platform"
            )
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use anyhow::{Context, Result, bail};
    use global_hotkey::{
        GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
        hotkey::{Code, HotKey, Modifiers},
    };
    use std::{
        marker::PhantomData,
        rc::Rc,
        sync::{
            Arc, Mutex, Once, Weak,
            atomic::{AtomicBool, Ordering},
        },
    };

    static HANDLER: Mutex<Option<Weak<RevealHandler>>> = Mutex::new(None);
    static INSTALL_HANDLER: Once = Once::new();

    struct RevealHandler {
        id: u32,
        pressed: AtomicBool,
        on_reveal: Box<dyn Fn() + Send + Sync>,
    }

    impl RevealHandler {
        fn dispatch(&self, event: GlobalHotKeyEvent) {
            if event.id != self.id {
                return;
            }
            match event.state {
                HotKeyState::Pressed => {
                    if !self.pressed.swap(true, Ordering::Relaxed) {
                        (self.on_reveal)();
                    }
                }
                HotKeyState::Released => self.pressed.store(false, Ordering::Relaxed),
            }
        }
    }

    fn dispatch_global(event: GlobalHotKeyEvent) {
        // Never wait in an OS callback, and release the slot before calling out.
        let handler = HANDLER
            .try_lock()
            .ok()
            .and_then(|slot| slot.as_ref().and_then(Weak::upgrade));
        if let Some(handler) = handler {
            handler.dispatch(event);
        }
    }

    struct HandlerGuard {
        _handler: Arc<RevealHandler>,
    }

    impl Drop for HandlerGuard {
        fn drop(&mut self) {
            // 0.9.0 uses OnceCell: set_event_handler(None::<fn(GlobalHotKeyEvent)>)
            // cannot clear it. Leave only the capture-free dispatcher installed;
            // removing this weak target releases the caller's proxy closure.
            *HANDLER.lock().unwrap_or_else(|error| error.into_inner()) = None;
        }
    }

    pub(super) struct Registration {
        manager: GlobalHotKeyManager,
        hotkey: HotKey,
        _handler: HandlerGuard,
        // The crate's manager is Send + Sync, but Carbon ownership stays here.
        _main_thread: PhantomData<Rc<()>>,
    }

    impl Registration {
        pub(super) fn new(on_reveal: impl Fn() + Send + Sync + 'static) -> Result<Self> {
            // SAFETY: pthread_main_np has no arguments or preconditions.
            if unsafe { libc::pthread_main_np() } == 0 {
                bail!("Register Option+I on the main thread after creating the winit event loop");
            }
            let mut slot = HANDLER
                .lock()
                .map_err(|_| anyhow::anyhow!("Option+I handler ownership lock was poisoned"))?;
            if slot.as_ref().and_then(Weak::upgrade).is_some() {
                bail!("Option+I is already registered by this app; keep only one hotkey owner");
            }

            let manager = GlobalHotKeyManager::new()
                .context("Could not initialize the macOS global hotkey manager")?;
            let hotkey = HotKey::new(Some(Modifiers::ALT), Code::KeyI);
            // KeyI uses Carbon RegisterEventHotKey, not a keyboard event tap.
            // Do not install or change the global handler if registration fails.
            manager.register(hotkey).context(
                "Could not register system-wide Option+I; another app may own it. \
                 Free the shortcut or use the reveal CLI/local shortcut",
            )?;

            let handler = Arc::new(RevealHandler {
                id: hotkey.id(),
                pressed: AtomicBool::new(false),
                on_reveal: Box::new(on_reveal),
            });
            INSTALL_HANDLER.call_once(|| {
                GlobalHotKeyEvent::set_event_handler(Some(
                    dispatch_global as fn(GlobalHotKeyEvent),
                ));
            });
            *slot = Some(Arc::downgrade(&handler));
            Ok(Self {
                manager,
                hotkey,
                _handler: HandlerGuard { _handler: handler },
                _main_thread: PhantomData,
            })
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            if let Err(error) = self.manager.unregister(self.hotkey) {
                tracing::warn!(%error, "Could not unregister system-wide Option+I");
            }
            // The manager also cleans up its Carbon event handler on drop.
            // HandlerGuard then detaches and releases the reveal callback.
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::AtomicUsize;

        #[test]
        fn dispatch_filters_ids_and_latches_until_matching_release() {
            let calls = Arc::new(AtomicUsize::new(0));
            let count = Arc::clone(&calls);
            let handler = RevealHandler {
                id: 42,
                pressed: AtomicBool::new(false),
                on_reveal: Box::new(move || {
                    count.fetch_add(1, Ordering::Relaxed);
                }),
            };
            for state in [HotKeyState::Pressed, HotKeyState::Released] {
                handler.dispatch(GlobalHotKeyEvent { id: 7, state });
            }
            assert_eq!(calls.load(Ordering::Relaxed), 0);

            for _ in 0..100 {
                handler.dispatch(GlobalHotKeyEvent {
                    id: 42,
                    state: HotKeyState::Pressed,
                });
            }
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            handler.dispatch(GlobalHotKeyEvent {
                id: 7,
                state: HotKeyState::Released,
            });
            handler.dispatch(GlobalHotKeyEvent {
                id: 42,
                state: HotKeyState::Pressed,
            });
            assert_eq!(calls.load(Ordering::Relaxed), 1);

            for _ in 0..2 {
                handler.dispatch(GlobalHotKeyEvent {
                    id: 42,
                    state: HotKeyState::Released,
                });
            }
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            handler.dispatch(GlobalHotKeyEvent {
                id: 42,
                state: HotKeyState::Pressed,
            });
            assert_eq!(calls.load(Ordering::Relaxed), 2);
        }

        #[test]
        fn weak_dispatch_target_does_not_retain_callback() {
            let captured = Arc::new(());
            let retained = Arc::clone(&captured);
            let handler = Arc::new(RevealHandler {
                id: 42,
                pressed: AtomicBool::new(false),
                on_reveal: Box::new(move || {
                    let _ = &retained;
                }),
            });
            let target = Arc::downgrade(&handler);
            assert_eq!(Arc::strong_count(&captured), 2);
            drop(handler);
            assert!(target.upgrade().is_none());
            assert_eq!(Arc::strong_count(&captured), 1);
        }
    }
}
