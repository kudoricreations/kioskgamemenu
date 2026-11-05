//! Makes SDL's KMSDRM framebuffers scan out on planes without alpha formats.
//!
//! SDL 3.4 always tries `ARGB8888` first, and GBM accepts it, so SDL has no
//! reason to fall back. Some planes then refuse it: `virtio-gpu` takes only
//! `XRGB8888`, `drmModeSetCrtc` fails on every frame and the console stays on
//! screen. SDL offers no hint to choose the format.
//!
//! The alpha channel of a primary plane means nothing, so the two formats show
//! the same picture. This executable exports its own `ioctl`, which libdrm's
//! call resolves to ahead of libc's, and relabels the framebuffer as it is
//! registered. Unlike an `LD_PRELOAD` shim this lives and dies with this
//! process image, so games never inherit it. `build.rs` exports the symbol.

use std::ffi::{c_int, c_long, c_ulong, c_void};

/// `_IOWR('d', 0xB8, struct drm_mode_fb_cmd2)`, a 104 byte structure.
const DRM_IOCTL_MODE_ADDFB2: c_ulong = 0xC068_64B8;
/// `pixel_format` follows `fb_id`, `width` and `height`.
const PIXEL_FORMAT_INDEX: usize = 3;
const FORMAT_ARGB8888: u32 = u32::from_le_bytes(*b"AR24");
const FORMAT_XRGB8888: u32 = u32::from_le_bytes(*b"XR24");

#[cfg(target_arch = "x86_64")]
const SYS_IOCTL: c_long = 16;
#[cfg(target_arch = "aarch64")]
const SYS_IOCTL: c_long = 29;

unsafe extern "C" {
    fn syscall(number: c_long, ...) -> c_long;
}

/// Replaces libc's `ioctl`. The real one is variadic, but on the supported
/// targets its third argument travels in the same register as here.
///
/// # Safety
/// Same contract as `ioctl(2)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn ioctl(fd: c_int, request: c_ulong, arg: *mut c_void) -> c_int {
    if request == DRM_IOCTL_MODE_ADDFB2 && !arg.is_null() {
        // SAFETY: this request's argument is a drm_mode_fb_cmd2 owned by the
        // caller, and the kernel is about to access the same memory.
        unsafe {
            let format = arg.cast::<u32>().add(PIXEL_FORMAT_INDEX);
            if *format == FORMAT_ARGB8888 {
                *format = FORMAT_XRGB8888;
            }
        }
    }
    // SAFETY: the arguments are the caller's, passed through unchanged.
    unsafe { syscall(SYS_IOCTL, fd as c_long, request, arg) as c_int }
}

/// Stops the optimizer discarding the export, which nothing here calls.
#[used]
static KEEP_IOCTL: unsafe extern "C" fn(c_int, c_ulong, *mut c_void) -> c_int = ioctl;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_codes_match_drm_fourcc() {
        assert_eq!(FORMAT_ARGB8888, 0x3432_5241);
        assert_eq!(FORMAT_XRGB8888, 0x3432_5258);
    }
}
