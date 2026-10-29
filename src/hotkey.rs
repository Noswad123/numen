//! System-wide Option+I reveal and Option+M go-to-mouse shortcuts.
//!
//! Register after creating the main-thread winit event loop, and drop guards on
//! that thread. Callbacks must only enqueue events, never block or move windows.
//! This module exclusively owns global-hotkey's write-once process-wide handler.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Shortcut {
    Reveal,
    GoToMouse,
}

/// Independently owns one shortcut. Registration failure leaves others intact.
pub struct GlobalHotkey {
    #[cfg(target_os = "macos")]
    _registration: macos::Registration,
    _main_thread: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl GlobalHotkey {
    /// Register on the main thread after winit initialization.
    ///
    /// Collisions are recoverable: retain CLI/local shortcuts on error. The
    /// callback runs in the OS handler and must only post a nonblocking event.
    pub fn register(
        shortcut: Shortcut,
        callback: impl Fn() + Send + Sync + 'static,
    ) -> anyhow::Result<Self> {
        #[cfg(target_os = "macos")]
        {
            Ok(Self {
                _registration: macos::Registration::new(shortcut, callback)?,
                _main_thread: std::marker::PhantomData,
            })
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = (shortcut, callback);
            anyhow::bail!(
                "System-wide shortcuts are supported only on macOS; use the CLI \
                 command or local window shortcut on this platform"
            )
        }
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::Shortcut;
    use anyhow::{Context, Result, bail};
    use global_hotkey::{
        GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState,
        hotkey::{Code, HotKey, Modifiers},
    };
    use std::{
        cell::RefCell,
        rc::{Rc, Weak as RcWeak},
        sync::{
            Arc, Mutex, Once, Weak,
            atomic::{AtomicBool, Ordering},
        },
    };

    static HANDLERS: Mutex<Registry> = Mutex::new(Registry([None, None]));
    static INSTALL_HANDLER: Once = Once::new();

    thread_local! {
        // Separate managers install competing Carbon handlers; the last guard
        // must be the only one that destroys the shared manager and handler.
        static MANAGER: RefCell<RcWeak<GlobalHotKeyManager>> = const { RefCell::new(RcWeak::new()) };
    }

    impl Shortcut {
        fn index(self) -> usize {
            match self {
                Self::Reveal => 0,
                Self::GoToMouse => 1,
            }
        }

        fn hotkey(self) -> HotKey {
            HotKey::new(
                Some(Modifiers::ALT),
                match self {
                    Self::Reveal => Code::KeyI,
                    Self::GoToMouse => Code::KeyM,
                },
            )
        }

        fn label(self) -> &'static str {
            match self {
                Self::Reveal => "Option+I",
                Self::GoToMouse => "Option+M",
            }
        }
    }

    struct Handler {
        id: u32,
        active: AtomicBool,
        pressed: AtomicBool,
        callback: Box<dyn Fn() + Send + Sync>,
    }

    impl Handler {
        fn dispatch(&self, event: GlobalHotKeyEvent) {
            if event.id != self.id || !self.active.load(Ordering::Acquire) {
                return;
            }
            match event.state {
                HotKeyState::Pressed => {
                    if !self.pressed.swap(true, Ordering::Relaxed) {
                        (self.callback)();
                    }
                }
                HotKeyState::Released => self.pressed.store(false, Ordering::Relaxed),
            }
        }
    }

    // Fixed capacity, no callback ownership and no unbounded stale-ID growth.
    struct Registry([Option<Weak<Handler>>; 2]);

    impl Registry {
        fn reserve(&mut self, shortcut: Shortcut, handler: &Arc<Handler>) -> Result<()> {
            let slot = &mut self.0[shortcut.index()];
            if slot.as_ref().and_then(Weak::upgrade).is_some() {
                bail!("{} is already registered by this app", shortcut.label());
            }
            *slot = Some(Arc::downgrade(handler));
            Ok(())
        }

        fn detach(&mut self, shortcut: Shortcut, handler: &Arc<Handler>) {
            let slot = &mut self.0[shortcut.index()];
            if slot
                .as_ref()
                .is_some_and(|target| target.ptr_eq(&Arc::downgrade(handler)))
            {
                *slot = None;
            }
        }

        fn target(&self, id: u32) -> Option<Arc<Handler>> {
            self.0
                .iter()
                .flatten()
                .filter_map(Weak::upgrade)
                .find(|handler| handler.id == id)
        }
    }

    fn dispatch(registry: &Mutex<Registry>, event: GlobalHotKeyEvent) {
        // Never wait in the OS callback; release the registry before calling out.
        let handler = registry
            .try_lock()
            .ok()
            .and_then(|slots| slots.target(event.id));
        if let Some(handler) = handler {
            handler.dispatch(event);
        }
    }

    fn dispatch_global(event: GlobalHotKeyEvent) {
        dispatch(&HANDLERS, event);
    }

    struct HandlerGuard<'a> {
        registry: &'a Mutex<Registry>,
        shortcut: Shortcut,
        handler: Arc<Handler>,
    }

    impl<'a> HandlerGuard<'a> {
        fn register(
            registry: &'a Mutex<Registry>,
            shortcut: Shortcut,
            callback: impl Fn() + Send + Sync + 'static,
            register: impl FnOnce() -> Result<()>,
        ) -> Result<Self> {
            let handler = Arc::new(Handler {
                id: shortcut.hotkey().id(),
                active: AtomicBool::new(false),
                pressed: AtomicBool::new(false),
                callback: Box::new(callback),
            });
            registry
                .lock()
                .map_err(|_| anyhow::anyhow!("Hotkey ownership lock was poisoned"))?
                .reserve(shortcut, &handler)?;
            let guard = Self {
                registry,
                shortcut,
                handler,
            };
            // A failed OS registration rolls back only this reservation. Pending
            // entries cannot invoke callbacks before registration succeeds.
            register()?;
            guard.handler.active.store(true, Ordering::Release);
            Ok(guard)
        }
    }

    impl Drop for HandlerGuard<'_> {
        fn drop(&mut self) {
            self.handler.active.store(false, Ordering::Release);
            self.registry
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .detach(self.shortcut, &self.handler);
        }
    }

    pub(super) struct Registration {
        manager: Rc<GlobalHotKeyManager>,
        hotkey: HotKey,
        handler: HandlerGuard<'static>,
    }

    impl Registration {
        pub(super) fn new(
            shortcut: Shortcut,
            callback: impl Fn() + Send + Sync + 'static,
        ) -> Result<Self> {
            // SAFETY: pthread_main_np has no arguments or preconditions.
            if unsafe { libc::pthread_main_np() } == 0 {
                bail!("Register hotkeys on the main thread after creating the winit event loop");
            }
            let manager = MANAGER.with(|slot| -> Result<_> {
                if let Some(manager) = slot.borrow().upgrade() {
                    return Ok(manager);
                }
                let manager = Rc::new(
                    GlobalHotKeyManager::new()
                        .context("Could not initialize the macOS global hotkey manager")?,
                );
                *slot.borrow_mut() = Rc::downgrade(&manager);
                Ok(manager)
            })?;
            let hotkey = shortcut.hotkey();
            let handler = HandlerGuard::register(&HANDLERS, shortcut, callback, || {
                // Install before Carbon can deliver anything: 0.9's OnceCell
                // would otherwise permanently initialize itself to None.
                INSTALL_HANDLER.call_once(|| {
                    GlobalHotKeyEvent::set_event_handler(Some(
                        dispatch_global as fn(GlobalHotKeyEvent),
                    ));
                });
                // KeyI and KeyM use Carbon, never an event tap or input injection.
                manager.register(hotkey).with_context(|| {
                    format!(
                        "Could not register system-wide {}; another app may own it. \
                         Free the shortcut or use the CLI/local shortcut",
                        shortcut.label()
                    )
                })
            })?;
            Ok(Self {
                manager,
                hotkey,
                handler,
            })
        }
    }

    impl Drop for Registration {
        fn drop(&mut self) {
            self.handler.handler.active.store(false, Ordering::Release);
            if let Err(error) = self.manager.unregister(self.hotkey) {
                tracing::warn!(%error, id = self.hotkey.id(), "Could not unregister global shortcut");
            }
            // Only the last Rc removes Carbon's handler. HandlerGuard detaches
            // only its own entry and releases its callback, even on OS error.
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::atomic::AtomicUsize;

        fn register_counter<'a>(
            registry: &'a Mutex<Registry>,
            shortcut: Shortcut,
            calls: &Arc<AtomicUsize>,
        ) -> HandlerGuard<'a> {
            let calls = Arc::clone(calls);
            HandlerGuard::register(
                registry,
                shortcut,
                move || {
                    calls.fetch_add(1, Ordering::Relaxed);
                },
                || Ok(()),
            )
            .unwrap()
        }

        fn event(registry: &Mutex<Registry>, shortcut: Shortcut, state: HotKeyState) {
            dispatch(
                registry,
                GlobalHotKeyEvent {
                    id: shortcut.hotkey().id(),
                    state,
                },
            );
        }

        #[test]
        fn both_keys_interleave_and_latch_independently() {
            let registry = Mutex::new(Registry([None, None]));
            let reveal = Arc::new(AtomicUsize::new(0));
            let mouse = Arc::new(AtomicUsize::new(0));
            let _reveal = register_counter(&registry, Shortcut::Reveal, &reveal);
            let _mouse = register_counter(&registry, Shortcut::GoToMouse, &mouse);
            assert_ne!(
                Shortcut::Reveal.hotkey().id(),
                Shortcut::GoToMouse.hotkey().id()
            );
            assert_eq!(Shortcut::Reveal.hotkey().key, Code::KeyI);
            assert_eq!(Shortcut::GoToMouse.hotkey().key, Code::KeyM);
            for state in [HotKeyState::Pressed, HotKeyState::Released] {
                dispatch(
                    &registry,
                    GlobalHotKeyEvent {
                        id: u32::MAX,
                        state,
                    },
                );
            }
            assert_eq!(reveal.load(Ordering::Relaxed), 0);
            assert_eq!(mouse.load(Ordering::Relaxed), 0);
            for _ in 0..100 {
                event(&registry, Shortcut::Reveal, HotKeyState::Pressed);
                event(&registry, Shortcut::GoToMouse, HotKeyState::Pressed);
            }
            assert_eq!(reveal.load(Ordering::Relaxed), 1);
            assert_eq!(mouse.load(Ordering::Relaxed), 1);
            dispatch(
                &registry,
                GlobalHotKeyEvent {
                    id: u32::MAX,
                    state: HotKeyState::Released,
                },
            );
            event(&registry, Shortcut::Reveal, HotKeyState::Pressed);
            event(&registry, Shortcut::GoToMouse, HotKeyState::Pressed);
            assert_eq!(reveal.load(Ordering::Relaxed), 1);
            assert_eq!(mouse.load(Ordering::Relaxed), 1);
            for _ in 0..2 {
                event(&registry, Shortcut::Reveal, HotKeyState::Released);
            }
            event(&registry, Shortcut::Reveal, HotKeyState::Pressed);
            event(&registry, Shortcut::GoToMouse, HotKeyState::Pressed);
            assert_eq!(reveal.load(Ordering::Relaxed), 2);
            assert_eq!(mouse.load(Ordering::Relaxed), 1);
            event(&registry, Shortcut::GoToMouse, HotKeyState::Released);
            event(&registry, Shortcut::GoToMouse, HotKeyState::Pressed);
            event(&registry, Shortcut::Reveal, HotKeyState::Pressed);
            assert_eq!(reveal.load(Ordering::Relaxed), 2);
            assert_eq!(mouse.load(Ordering::Relaxed), 2);
        }

        #[test]
        fn failed_reservation_and_registration_preserve_other_owner() {
            for first in [Shortcut::Reveal, Shortcut::GoToMouse] {
                let registry = Mutex::new(Registry([None, None]));
                let calls = Arc::new(AtomicUsize::new(0));
                let _owner = register_counter(&registry, first, &calls);
                assert!(
                    HandlerGuard::register(
                        &registry,
                        first,
                        || {},
                        || { panic!("duplicate reservation must not reach OS registration") }
                    )
                    .is_err()
                );
                let other = match first {
                    Shortcut::Reveal => Shortcut::GoToMouse,
                    Shortcut::GoToMouse => Shortcut::Reveal,
                };
                let captured = Arc::new(());
                let retained = Arc::clone(&captured);
                assert!(
                    HandlerGuard::register(
                        &registry,
                        other,
                        move || {
                            let _ = &retained;
                            panic!("pending registration must not dispatch");
                        },
                        || {
                            event(&registry, other, HotKeyState::Pressed);
                            bail!("simulated OS collision")
                        }
                    )
                    .is_err()
                );
                assert_eq!(Arc::strong_count(&captured), 1);
                assert!(
                    registry
                        .lock()
                        .unwrap()
                        .target(other.hotkey().id())
                        .is_none()
                );
                event(&registry, first, HotKeyState::Pressed);
                assert_eq!(calls.load(Ordering::Relaxed), 1);
                let _retry = register_counter(&registry, other, &calls);
                event(&registry, other, HotKeyState::Pressed);
                assert_eq!(calls.load(Ordering::Relaxed), 2);
            }
        }

        #[test]
        fn dropping_one_releases_callback_and_leaves_other_routing() {
            for first in [Shortcut::Reveal, Shortcut::GoToMouse] {
                let registry = Mutex::new(Registry([None, None]));
                let captured = Arc::new(());
                let retained = Arc::clone(&captured);
                let owner = HandlerGuard::register(
                    &registry,
                    first,
                    move || {
                        let _ = &retained;
                    },
                    || Ok(()),
                )
                .unwrap();
                let stale = Arc::downgrade(&owner.handler);
                let other = match first {
                    Shortcut::Reveal => Shortcut::GoToMouse,
                    Shortcut::GoToMouse => Shortcut::Reveal,
                };
                let calls = Arc::new(AtomicUsize::new(0));
                let _other = register_counter(&registry, other, &calls);
                drop(owner);
                assert!(stale.upgrade().is_none());
                assert_eq!(Arc::strong_count(&captured), 1);
                event(&registry, first, HotKeyState::Pressed);
                event(&registry, other, HotKeyState::Pressed);
                assert_eq!(calls.load(Ordering::Relaxed), 1);
            }
        }

        #[test]
        fn stale_cleanup_cannot_detach_replacement_and_dispatch_unlocks() {
            let registry = Mutex::new(Registry([None, None]));
            let calls = Arc::new(AtomicUsize::new(0));
            let old = register_counter(&registry, Shortcut::Reveal, &calls);
            registry
                .lock()
                .unwrap()
                .detach(Shortcut::Reveal, &old.handler);
            let replacement = register_counter(&registry, Shortcut::Reveal, &calls);
            drop(old);
            event(&registry, Shortcut::Reveal, HotKeyState::Pressed);
            assert_eq!(calls.load(Ordering::Relaxed), 1);
            let expired = Arc::downgrade(&replacement.handler);
            drop(replacement);
            assert!(registry.lock().unwrap().0.iter().all(Option::is_none));
            // An expired weak entry can be reclaimed without registry growth.
            registry.lock().unwrap().0[Shortcut::Reveal.index()] = Some(expired);
            let reclaimed = register_counter(&registry, Shortcut::Reveal, &calls);
            event(&registry, Shortcut::Reveal, HotKeyState::Pressed);
            assert_eq!(calls.load(Ordering::Relaxed), 2);
            drop(reclaimed);
            let registry = Arc::new(registry);
            let captured = Arc::clone(&registry);
            let _guard = HandlerGuard::register(
                &registry,
                Shortcut::GoToMouse,
                move || {
                    assert!(captured.try_lock().is_ok());
                },
                || Ok(()),
            )
            .unwrap();
            event(&registry, Shortcut::GoToMouse, HotKeyState::Pressed);
            let locked = registry.lock().unwrap();
            // Contention must return immediately, not block the OS event loop.
            event(&registry, Shortcut::GoToMouse, HotKeyState::Released);
            drop(locked);
        }
    }
}
