//! Keep a native canvas behind asynchronous webview frames during Dock restore.
//! macOS traffic lights belong to AppKit, not to the WebKit compositor.

#[cfg(target_os = "macos")]
pub fn paint_macos_canvas(window: &tauri::WebviewWindow, dark: bool) -> tauri::Result<()> {
    use objc2::{class, msg_send, runtime::AnyObject};
    let view = window.ns_view()?.cast::<AnyObject>();
    let (r, g, b) = if dark { (17.0, 40.0, 49.0) } else { (237.0, 246.0, 246.0) };
    // Called on the main thread. Keep NSWindow transparent for its rounded
    // edge, but retain an opaque, clipped native content layer underneath WKWebView.
    // Unlike delaying the controls, this also covers slow/minimized WebKit frames.
    unsafe {
        let _: () = msg_send![view, setWantsLayer: true];
        let layer: *mut AnyObject = msg_send![view, layer];
        if layer.is_null() {
            return Err(std::io::Error::other("Native window canvas is unavailable").into());
        }
        let color: *mut AnyObject = msg_send![class!(NSColor), colorWithSRGBRed: r / 255.0, green: g / 255.0, blue: b / 255.0, alpha: 1.0_f64];
        let cg_color: *const std::ffi::c_void = msg_send![color, CGColor];
        let _: () = msg_send![layer, setBackgroundColor: cg_color];
        let _: () = msg_send![layer, setCornerRadius: 12.0_f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
    }
    Ok(())
}

#[tauri::command]
pub async fn sync_window_surface(window: tauri::WebviewWindow, dark: bool) -> Result<(), String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    let native = window.clone();
    window.run_on_main_thread(move || {
        #[cfg(target_os = "macos")]
        let result = paint_macos_canvas(&native, dark);
        #[cfg(target_os = "windows")]
        let result = native.set_background_color(Some(if dark {
            (17, 40, 49, 255).into()
        } else {
            (237, 246, 246, 255).into()
        }));
        // GTK requires a clear gutter for its client-side shadow. Its controls
        // are HTML, so controls and content already share one compositor frame.
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        let result: tauri::Result<()> = { let _ = (native, dark); Ok(()) };
        let _ = tx.send(result.map_err(|error| error.to_string()));
    }).map_err(|error| error.to_string())?;
    rx.await.map_err(|error| error.to_string())?
}
