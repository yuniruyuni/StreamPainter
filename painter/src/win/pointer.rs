//! Small Win32 pointer metadata boundary.
//!
//! The pointer id is only queried synchronously while handling its
//! `WM_POINTER*` message. No Win32-owned pointer or reference escapes this
//! module; copied scalar fields are normalized by the platform-neutral engine.

use windows::Win32::UI::Input::Pointer::{GetPointerPenInfo, GetPointerType, POINTER_PEN_INFO};
use windows::Win32::UI::WindowsAndMessaging::{
    PEN_MASK_PRESSURE, PEN_MASK_TILT_X, PEN_MASK_TILT_Y, POINTER_INPUT_TYPE, PT_MOUSE, PT_PEN,
    PT_TOUCH, PT_TOUCHPAD,
};

use crate::engine::pointer_input::{
    normalize_pointer_sample, PointerDevice, PointerSample, RawPenData,
};

pub fn sample(pointer_id: u32) -> PointerSample {
    let mut pointer_type = POINTER_INPUT_TYPE::default();
    // SAFETY: both output pointers refer to initialized, writable stack values
    // for the duration of the synchronous User32 call. `pointer_id` comes from
    // the current WM_POINTER message and is not retained after this function.
    if unsafe { GetPointerType(pointer_id, &mut pointer_type) }.is_err() {
        return normalize_pointer_sample(PointerDevice::Unknown, None);
    }

    let device = pointer_device(pointer_type);
    if device != PointerDevice::Pen {
        return normalize_pointer_sample(device, None);
    }

    let mut pen_info = POINTER_PEN_INFO::default();
    if unsafe { GetPointerPenInfo(pointer_id, &mut pen_info) }.is_err() {
        return normalize_pointer_sample(device, None);
    }
    normalize_pointer_sample(device, Some(raw_pen_data(&pen_info)))
}

fn pointer_device(pointer_type: POINTER_INPUT_TYPE) -> PointerDevice {
    match pointer_type {
        PT_MOUSE => PointerDevice::Mouse,
        PT_TOUCH => PointerDevice::Touch,
        PT_PEN => PointerDevice::Pen,
        PT_TOUCHPAD => PointerDevice::Touchpad,
        _ => PointerDevice::Unknown,
    }
}

fn raw_pen_data(info: &POINTER_PEN_INFO) -> RawPenData {
    RawPenData {
        pressure: (info.penMask & PEN_MASK_PRESSURE != 0).then_some(info.pressure),
        tilt_x: (info.penMask & PEN_MASK_TILT_X != 0).then_some(info.tiltX),
        tilt_y: (info.penMask & PEN_MASK_TILT_Y != 0).then_some(info.tiltY),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_pointer_types_are_kept_distinct() {
        assert_eq!(pointer_device(PT_MOUSE), PointerDevice::Mouse);
        assert_eq!(pointer_device(PT_TOUCH), PointerDevice::Touch);
        assert_eq!(pointer_device(PT_PEN), PointerDevice::Pen);
        assert_eq!(pointer_device(PT_TOUCHPAD), PointerDevice::Touchpad);
        assert_eq!(
            pointer_device(POINTER_INPUT_TYPE(999)),
            PointerDevice::Unknown
        );
    }

    #[test]
    fn pen_mask_is_the_only_authority_for_optional_fields() {
        let info = POINTER_PEN_INFO {
            penMask: PEN_MASK_PRESSURE | PEN_MASK_TILT_Y,
            pressure: 768,
            tiltX: 45,
            tiltY: -30,
            ..Default::default()
        };
        assert_eq!(
            raw_pen_data(&info),
            RawPenData {
                pressure: Some(768),
                tilt_x: None,
                tilt_y: Some(-30),
            }
        );
    }
}
