//! The app icon: the bell on its dark disc, drawn in code so no image files or
//! decoders are needed. Same geometry as the widget's bell (`ui::paint_bell`).

use eframe::egui::IconData;

const DISC: [u8; 3] = [28, 30, 38];
const BODY: [u8; 3] = [228, 190, 82];
const SHADE: [u8; 3] = [178, 140, 50];

/// Which colour (if any) covers the point `(x, y)` of a unit square.
fn sample(x: f32, y: f32) -> Option<[u8; 3]> {
    let in_circle = |cx: f32, cy: f32, r: f32| (x - cx).powi(2) + (y - cy).powi(2) <= r * r;
    // Bell coordinates: centre (0.5, 0.53), scale 0.6 of the icon.
    let (bx, by) = ((x - 0.5) / 0.6, (y - 0.53) / 0.6);
    let in_bell_circle = |cx: f32, cy: f32, r: f32| (bx - cx).powi(2) + (by - cy).powi(2) <= r * r;

    let rim = (0.19..=0.31).contains(&by) && bx.abs() <= 0.56;
    let rim_ends = in_bell_circle(-0.56, 0.25, 0.06) || in_bell_circle(0.56, 0.25, 0.06);
    let body = (-0.24..=0.24).contains(&by) && bx.abs() <= 0.34 + (by + 0.24) / 0.48 * 0.13;
    let dome = by <= -0.22 && (bx / 0.34).powi(2) + ((by + 0.22) / 0.36).powi(2) <= 1.0;
    let knob = in_bell_circle(0.0, -0.64, 0.08);
    let clapper = in_bell_circle(0.0, 0.31, 0.10);

    if rim || rim_ends {
        Some(SHADE)
    } else if body || dome {
        Some(BODY)
    } else if knob || clapper {
        Some(SHADE)
    } else if in_circle(0.5, 0.5, 0.5) {
        Some(DISC)
    } else {
        None
    }
}

/// Straight (non-premultiplied) RGBA pixels of a `size`×`size` icon.
pub fn rgba(size: u32) -> Vec<u8> {
    const SS: u32 = 4; // supersampling per axis
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let mut acc = [0u32; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let x = (px as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    if let Some(c) = sample(x, y) {
                        for i in 0..3 {
                            acc[i] += c[i] as u32;
                        }
                        acc[3] += 1;
                    }
                }
            }
            if acc[3] == 0 {
                out.extend([0, 0, 0, 0]);
            } else {
                let n = acc[3];
                out.extend([
                    (acc[0] / n) as u8,
                    (acc[1] / n) as u8,
                    (acc[2] / n) as u8,
                    (n * 255 / (SS * SS)) as u8,
                ]);
            }
        }
    }
    out
}

pub fn icon_data(size: u32) -> IconData {
    IconData {
        rgba: rgba(size),
        width: size,
        height: size,
    }
}

/// Windows: give the window titled `title` (one of ours) the icon in both sizes.
/// egui only sets the small (title bar) icon on secondary windows, so without
/// this the taskbar shows a generic one. Returns whether the window was found.
#[cfg(windows)]
pub fn set_window_icons(title: &str) -> bool {
    use windows_sys::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
        DeleteObject,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateIconIndirect, FindWindowW, GetSystemMetrics, GetWindowThreadProcessId, ICON_BIG,
        ICON_SMALL, ICONINFO, SM_CXICON, SM_CXSMICON, SendMessageW, WM_SETICON,
    };

    let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let hwnd = unsafe { FindWindowW(std::ptr::null(), wide.as_ptr()) };
    if hwnd.is_null() {
        return false;
    }
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, &mut pid) };
    if pid != std::process::id() {
        return false;
    }

    let make_icon = |size: i32| unsafe {
        let size = size.clamp(16, 256);
        let mut info: BITMAPINFO = std::mem::zeroed();
        info.bmiHeader = BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: size,
            biHeight: -size, // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..std::mem::zeroed()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(
            std::ptr::null_mut(),
            &info,
            DIB_RGB_COLORS,
            &mut bits,
            std::ptr::null_mut(),
            0,
        );
        if color.is_null() || bits.is_null() {
            return std::ptr::null_mut();
        }
        // RGBA -> BGRA
        let px = rgba(size as u32);
        let dst = std::slice::from_raw_parts_mut(bits as *mut u8, px.len());
        for (d, s) in dst
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(px.as_chunks::<4>().0)
        {
            *d = [s[2], s[1], s[0], s[3]];
        }
        let mask = CreateBitmap(size, size, 1, 1, std::ptr::null());
        let ii = ICONINFO {
            fIcon: 1,
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color,
        };
        let icon = CreateIconIndirect(&ii);
        DeleteObject(color);
        DeleteObject(mask);
        icon
    };
    unsafe {
        let big = make_icon(GetSystemMetrics(SM_CXICON).max(32) * 2);
        let small = make_icon(GetSystemMetrics(SM_CXSMICON).max(16) * 2);
        if !big.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, big as isize);
        }
        if !small.is_null() {
            SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, small as isize);
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_bell_and_transparent_corners() {
        let size = 32;
        let px = rgba(size);
        assert_eq!(px.len(), (size * size * 4) as usize);
        assert_eq!(px[3], 0, "top-left corner is transparent");
        let centre = ((size / 2 * size + size / 2) * 4) as usize;
        assert_eq!(px[centre + 3], 255, "centre is opaque");
        assert_eq!(&px[centre..centre + 3], &BODY, "centre is the bell");
    }
}
