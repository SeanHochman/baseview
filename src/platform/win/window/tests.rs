//! Native resize transactions. Run these on a Windows desktop, without a frame loop.
use super::*;
use crate::host::HostCallbacks;
use crate::{WindowSettings, WindowSize};
use std::sync::{Arc, Mutex};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetClientRect, SendMessageW};

struct Observations {
    handler: Mutex<Vec<WindowSize>>,
    host: Mutex<Vec<WindowSize>>,
}

struct Handler(Arc<Observations>);
impl WindowHandler for Handler {
    fn on_frame(&self) -> core::result::Result<(), HandlerError> {
        panic!("resize completion must not depend on a later frame");
    }

    fn resized(&self, size: WindowSize) -> core::result::Result<(), HandlerError> {
        self.0.handler.lock().unwrap().push(size);
        Ok(())
    }

    fn on_event(&self, _event: Event) -> EventStatus {
        EventStatus::Ignored
    }
}

struct RefusingHost(Arc<Observations>);
impl HostCallbacks for RefusingHost {
    fn request_resize(&mut self, size: WindowSize) -> core::result::Result<(), HandlerError> {
        self.0.host.lock().unwrap().push(size);
        Err(std::io::Error::other("test host refuses resize").into())
    }

    fn destroyed(&mut self) {}
}

fn fixture() -> (WindowHandle, Arc<Observations>) {
    let observations =
        Arc::new(Observations { handler: Mutex::new(Vec::new()), host: Mutex::new(Vec::new()) });
    let handler = Arc::clone(&observations);
    let window = WindowHandle::create_window(WindowInitializer {
        settings: WindowSettings::new()
            .with_title("Baseview resize rollback regression")
            .with_size(PhysicalSize::new(400, 300))
            .with_resizable(true),
        builder: WindowHandlerBuilder::new(move |_| Ok(Handler(handler))),
        host: Host::new().with_callbacks(RefusingHost(Arc::clone(&observations))),
    })
    .unwrap();
    assert!(window.is_open());
    assert!(observations.handler.lock().unwrap().is_empty());
    assert!(observations.host.lock().unwrap().is_empty());
    (window, observations)
}

fn assert_restored(
    window: &WindowHandle, observations: &Observations, previous: PhysicalSize<u32>,
    requested: PhysicalSize<u32>,
) {
    let actual = window.size();
    assert_eq!(actual.physical, previous);
    let notifications = observations.handler.lock().unwrap();
    assert_eq!(notifications.len(), 2);
    assert_eq!(notifications[0].physical, requested);
    assert_eq!(notifications[1].physical, previous);
    // A DPI change remains in force even though the host refused new dimensions.
    assert_eq!(notifications[1].scale_factor, actual.scale_factor);
    assert_eq!(notifications[1].logical, previous.to_logical::<f64>(actual.scale_factor));
    let requests = observations.host.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].physical, requested);

    let hwnd = window.hwnd.get().unwrap();
    let _dpi =
        DpiAwarenessGuard::new(&window.state.user32, window.state.dpi_scaling_strategy.get())
            .unwrap();
    let mut client = RECT::default();
    // SAFETY: This window remains owned and open on its creating thread; client
    // is valid writable RECT storage for the duration of the synchronous call.
    assert_ne!(unsafe { GetClientRect(hwnd.as_raw(), &mut client) }, 0);
    assert_eq!(client.right - client.left, previous.width as i32);
    assert_eq!(client.bottom - client.top, previous.height as i32);
}

#[test]
#[ignore = "requires a Windows desktop; exercises real synchronous WM_SIZE rollback"]
fn host_refused_wm_size_restores_handler_and_native_geometry_without_a_frame() {
    let (window, observations) = fixture();
    let previous = window.size().physical;
    let requested = PhysicalSize::new(previous.width + 120, previous.height + 80);
    let dpi = DpiAwarenessGuard::new(&window.state.user32, window.state.dpi_scaling_strategy.get())
        .unwrap();
    // Resize through the native window, as a user/window-manager resize does.
    // WindowHandle::resize deliberately bypasses the host request and cannot
    // exercise this refusal branch.
    window
        .hwnd
        .get()
        .unwrap()
        .resize_and_activate(requested, window.state.current_dpi.get(), &dpi)
        .unwrap();
    assert_restored(&window, &observations, previous, requested);
}

#[test]
#[ignore = "requires a Windows desktop; exercises real synchronous WM_DPICHANGED rollback"]
fn host_refused_dpi_resize_restores_dimensions_and_preserves_current_dpi_without_a_frame() {
    let (window, observations) = fixture();
    let previous = window.size().physical;
    let requested = PhysicalSize::new(previous.width + 120, previous.height + 80);
    // Keep the HWND's actual OS DPI: a synthetic message cannot move the
    // window to another monitor or change Windows' nonclient DPI. A changed
    // recommended size still exercises WM_DPICHANGED's adaptation transaction.
    let dpi = window.state.current_dpi.get();
    let guard =
        DpiAwarenessGuard::new(&window.state.user32, window.state.dpi_scaling_strategy.get())
            .unwrap();
    let hwnd = window.hwnd.get().unwrap();
    let suggested = guard
        .client_area_to_nc_area(Rect::from(requested), hwnd.get_style().unwrap(), Some(dpi))
        .unwrap();
    let wparam = dpi.0.get() as usize | ((dpi.0.get() as usize) << 16);
    // SAFETY: This live owned HWND was created with Baseview's window procedure.
    // WM_DPICHANGED synchronously reads the valid RECT before SendMessageW
    // returns. The test and the window run on the same thread.
    unsafe {
        SendMessageW(
            hwnd.as_raw(),
            WM_DPICHANGED,
            wparam,
            std::ptr::from_ref(&suggested.0) as LPARAM,
        );
    }
    assert_eq!(window.state.current_dpi.get(), dpi);
    assert_restored(&window, &observations, previous, requested);
}
