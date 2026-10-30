//! Per-instance frame policy through a process-wide method ADDITION on WinitWindow.
//!
//! Audited against winit 0.30.13 and objc2 0.5.3. No existing method is replaced,
//! no isa is changed, and neither NSWindow nor Foundation's KVO class is modified.
//! KVO inherits the added base method; observer removal cannot discard the policy.
//!
//! Native smoke (main-thread AppKit harness): create two hidden borderless winit
//! windows; configure one twice, checking both actual classes and frames remain
//! unchanged. Compare constraint results with direct NSWindow super dispatch:
//! ordinary windows must match it, opted-in windows must preserve the proposal.
//! Verify titled/unknown-class rejection, extra KVO observer add/remove, appearance
//! notifications, incremental upward screen crossing, and normal window teardown.

/// Allow this borderless familiar's frame to cross the screen's visible bounds.
///
/// Call on the main thread after creation, before positioning. This is idempotent
/// and does not move/show the window. Non-macOS platforms do nothing. macOS accepts
/// only WinitWindow and its standard direct NSKVONotifying_WinitWindow child.
/// Other windows retain AppKit's constraints unless explicitly opted in here.
pub fn configure_unrestricted_familiar_window(
    window: &winit::window::Window,
) -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    return native::configure(window);
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod native {
    use std::{mem, ptr, sync::OnceLock};

    use anyhow::{Context as _, Result, ensure};
    use objc2::{
        ClassType, ffi, msg_send,
        runtime::{AnyClass, AnyObject, Imp, Sel},
        sel,
    };
    use objc2_app_kit::{NSScreen, NSView, NSWindow, NSWindowStyleMask};
    use objc2_foundation::{MainThreadMarker, NSObject, NSRect};
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    static INSTALLED: OnceLock<Result<&'static AnyClass, String>> = OnceLock::new();
    // A non-zero-sized static gives this module a unique, process-lifetime key.
    static OPT_IN_KEY: u8 = 0;

    fn opted_in(object: &AnyObject) -> bool {
        // SAFETY: Valid borrowed object and stable key. The value is used only
        // for presence; no unretained pointer escapes the runtime lookup.
        unsafe {
            !ffi::objc_getAssociatedObject(
                (object as *const AnyObject).cast(),
                ptr::addr_of!(OPT_IN_KEY).cast(),
            )
            .is_null()
        }
    }

    fn opt_in(object: &AnyObject) {
        if opted_in(object) {
            return;
        }
        let token = NSObject::new();
        // SAFETY: The runtime retains the independent token, releasing it when
        // the window dies. It never retains the window, so there is no cycle.
        // Production access is main-thread-only; no pointer-address registry or
        // dealloc hook is needed, and reused window addresses cannot inherit it.
        unsafe {
            ffi::objc_setAssociatedObject(
                (object as *const AnyObject).cast_mut().cast(),
                ptr::addr_of!(OPT_IN_KEY).cast(),
                (&*token as *const NSObject).cast_mut().cast(),
                ffi::OBJC_ASSOCIATION_RETAIN_NONATOMIC,
            );
        }
    }

    type ConstraintImp = unsafe extern "C" fn(&NSWindow, Sel, NSRect, *mut NSScreen) -> NSRect;

    unsafe extern "C" fn constrain_frame(
        this: &NSWindow,
        _cmd: Sel,
        proposed: NSRect,
        screen: *mut NSScreen,
    ) -> NSRect {
        // SAFETY: NSWindow operations, including this AppKit callback, require
        // the main thread. The receiver is a live NSWindow subclass; screen is
        // nullable. No application state is borrowed and there are no assertions.
        if opted_in(this) && !this.styleMask().contains(NSWindowStyleMask::Titled) {
            proposed
        } else {
            // Start at NSWindow, NOT this's dynamic superclass (which may be
            // WinitWindow under KVO); dynamic super dispatch would recurse.
            unsafe {
                msg_send![super(this, NSWindow::class()),
                    constrainFrameRect: proposed, toScreen: screen]
            }
        }
    }

    fn owns_constraint(class: &AnyClass) -> bool {
        class
            .instance_methods()
            .iter()
            .any(|method| method.name() == sel!(constrainFrameRect:toScreen:))
    }

    // Production calls this only for the validated WinitWindow base. Tests use
    // isolated runtime fixtures, never Foundation's own classes.
    fn add_constraint_method(base: &AnyClass) -> Result<()> {
        ensure!(
            base.superclass()
                .is_some_and(|parent| ptr::eq(parent, NSWindow::class())),
            "Native window base must directly inherit NSWindow"
        );
        ensure!(
            !owns_constraint(base),
            "Native window base already implements constrainFrameRect:toScreen:"
        );
        let selector = sel!(constrainFrameRect:toScreen:);
        NSWindow::class()
            .verify_sel::<(NSRect, *mut NSScreen), NSRect>(selector)
            .map_err(|error| {
                anyhow::anyhow!("Incompatible AppKit constraint signature: {error}")
            })?;
        // SAFETY: Public runtime APIs on live registered classes. Use AppKit's
        // EXACT encoding, including offsets, not a hand-built signature. The
        // typed IMP uses objc2's NSRect ABI on both arm64 and x86_64. class_addMethod
        // atomically refuses an existing direct implementation; it never replaces
        // one even if another installer races the earlier ownership check.
        unsafe {
            let original = ffi::class_getInstanceMethod(
                (NSWindow::class() as *const AnyClass).cast(),
                selector.as_ptr(),
            );
            ensure!(!original.is_null(), "AppKit constraint method is missing");
            let encoding = ffi::method_getTypeEncoding(original);
            ensure!(!encoding.is_null(), "AppKit constraint encoding is missing");
            let imp = mem::transmute::<ConstraintImp, Imp>(constrain_frame as ConstraintImp);
            ensure!(
                ffi::class_addMethod(
                    (base as *const AnyClass).cast_mut().cast(),
                    selector.as_ptr(),
                    Some(imp),
                    encoding,
                ) != ffi::NO,
                "Could not add native familiar constraint method"
            );
        }
        Ok(())
    }

    pub(super) fn configure(window: &winit::window::Window) -> Result<()> {
        MainThreadMarker::new().context("Native window configuration requires the main thread")?;
        let handle = window.window_handle()?;
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            anyhow::bail!("Native familiar requires an AppKit window");
        };
        // SAFETY: The winit borrow keeps its NSView alive; AppKit access is on
        // the main thread. Retain its NSWindow for the duration of configuration.
        let view = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
        let window = view
            .window()
            .context("Native familiar view has no window")?;
        ensure!(
            !window.styleMask().contains(NSWindowStyleMask::Titled),
            "Unrestricted familiar must be borderless"
        );
        let base = AnyClass::get("WinitWindow").context("WinitWindow is not registered")?;
        ensure!(
            base.superclass()
                .is_some_and(|parent| ptr::eq(parent, NSWindow::class())),
            "Unsupported WinitWindow superclass"
        );
        let object: &AnyObject = window.as_ref();
        let current = object.class(); // object_getClass, not KVO's overridable -class.
        ensure!(
            ptr::eq(current, base)
                || (current.name() == "NSKVONotifying_WinitWindow"
                    && current
                        .superclass()
                        .is_some_and(|parent| ptr::eq(parent, base))
                    && !owns_constraint(current)),
            "Unsupported native familiar class: {}",
            current.name()
        );
        let installed = INSTALLED
            .get_or_init(|| {
                add_constraint_method(base)
                    .map(|()| base)
                    .map_err(|error| error.to_string())
            })
            .as_ref()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        ensure!(ptr::eq(*installed, base), "Native familiar base changed");
        opt_in(object);
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use objc2::runtime::ClassBuilder;
        use std::ffi::CStr;

        #[test]
        fn opt_in_token_is_retained_per_object_and_idempotent() {
            let familiar = NSObject::new();
            let ordinary = NSObject::new();
            assert!(!opted_in(&familiar));
            assert!(!opted_in(&ordinary));
            opt_in(&familiar);
            assert!(opted_in(&familiar)); // The local token owner has been dropped.
            opt_in(&familiar);
            assert!(opted_in(&familiar));
            assert!(!opted_in(&ordinary));
        }

        #[test]
        #[ignore = "opt-in Objective-C method addition on an isolated test class; no windows"]
        fn method_addition_preserves_layout_encoding_and_ordinary_dispatch() {
            let parent = NSWindow::class();
            let fixture = ClassBuilder::new("NumenConstraintAdditionTest", parent)
                .unwrap()
                .register();
            let selector = sel!(constrainFrameRect:toScreen:);
            let original = parent.instance_method(selector).unwrap();
            let size = fixture.instance_size();
            assert!(ptr::eq(
                fixture.instance_method(selector).unwrap(),
                original
            ));
            add_constraint_method(fixture).unwrap();
            assert_eq!(fixture.instance_size(), size);
            assert_eq!(fixture.instance_methods().len(), 1);
            assert!(ptr::eq(parent.instance_method(selector).unwrap(), original));
            for selector in [sel!(dealloc), sel!(close), sel!(class)] {
                assert!(ptr::eq(
                    fixture.instance_method(selector).unwrap(),
                    parent.instance_method(selector).unwrap(),
                ));
            }
            // Sibling (ordinary) NSWindow subclasses retain the original dispatch.
            let ordinary = ClassBuilder::new("NumenConstraintOrdinaryTest", parent)
                .unwrap()
                .register();
            assert!(ptr::eq(
                ordinary.instance_method(selector).unwrap(),
                original
            ));
            unsafe {
                let encoding = |class: &AnyClass| {
                    let method = ffi::class_getInstanceMethod(
                        (class as *const AnyClass).cast(),
                        selector.as_ptr(),
                    );
                    CStr::from_ptr(ffi::method_getTypeEncoding(method))
                };
                assert_eq!(encoding(fixture), encoding(parent));
            }
            let installed = fixture.instance_method(selector).unwrap();
            assert!(add_constraint_method(fixture).is_err());
            assert!(ptr::eq(
                fixture.instance_method(selector).unwrap(),
                installed
            ));
        }
    }
}
