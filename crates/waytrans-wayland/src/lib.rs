#[cfg(target_os = "linux")]
pub mod capture;
#[cfg(target_os = "linux")]
pub mod desktop;
#[cfg(target_os = "linux")]
pub mod overlay;

#[cfg(target_os = "linux")]
pub use capture::{
    check_capture_dependencies, get_active_monitor_geometry, select_region_slurp, GrimCapture,
};
#[cfg(target_os = "linux")]
pub use desktop::{copy_to_clipboard, notify_fallback};
#[cfg(target_os = "linux")]
pub use overlay::WaylandLayerOverlay;
