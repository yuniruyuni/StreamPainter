//! Pointer device metadata normalization independent of Win32.
//!
//! Windows reports pen pressure as 0..=1024 and tilt angles as -90..=90
//! degrees. Drivers are still treated as untrusted input: out-of-range values
//! are clamped, while a missing field keeps the historic mouse-like fallback.

pub const FALLBACK_PRESSURE: f64 = 1.0;
const WINDOWS_PRESSURE_MAX: f64 = 1024.0;
const WINDOWS_TILT_MAX_DEGREES: f64 = 90.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerDevice {
    Mouse,
    Touch,
    Pen,
    Touchpad,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerDynamics {
    pub pressure: f64,
    pub tilt_x: f64,
    pub tilt_y: f64,
}

impl PointerDynamics {
    pub const FALLBACK: Self = Self {
        pressure: FALLBACK_PRESSURE,
        tilt_x: 0.0,
        tilt_y: 0.0,
    };
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PointerSample {
    pub device: PointerDevice,
    pub dynamics: PointerDynamics,
}

/// Optional values after the Win32 `penMask` validity bits have been applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RawPenData {
    pub pressure: Option<u32>,
    pub tilt_x: Option<i32>,
    pub tilt_y: Option<i32>,
}

pub fn normalize_pointer_sample(device: PointerDevice, pen: Option<RawPenData>) -> PointerSample {
    let dynamics = if device == PointerDevice::Pen {
        pen.map_or(PointerDynamics::FALLBACK, |pen| PointerDynamics {
            pressure: pen.pressure.map_or(FALLBACK_PRESSURE, |value| {
                f64::from(value.min(WINDOWS_PRESSURE_MAX as u32)) / WINDOWS_PRESSURE_MAX
            }),
            tilt_x: normalize_tilt(pen.tilt_x),
            tilt_y: normalize_tilt(pen.tilt_y),
        })
    } else {
        PointerDynamics::FALLBACK
    };
    PointerSample { device, dynamics }
}

fn normalize_tilt(value: Option<i32>) -> f64 {
    value.map_or(0.0, |value| {
        f64::from(value.clamp(
            -(WINDOWS_TILT_MAX_DEGREES as i32),
            WINDOWS_TILT_MAX_DEGREES as i32,
        )) / WINDOWS_TILT_MAX_DEGREES
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pen_values_are_normalized_and_clamped() {
        let sample = normalize_pointer_sample(
            PointerDevice::Pen,
            Some(RawPenData {
                pressure: Some(512),
                tilt_x: Some(45),
                tilt_y: Some(-90),
            }),
        );
        assert_eq!(sample.device, PointerDevice::Pen);
        assert_eq!(sample.dynamics.pressure, 0.5);
        assert_eq!(sample.dynamics.tilt_x, 0.5);
        assert_eq!(sample.dynamics.tilt_y, -1.0);

        let clamped = normalize_pointer_sample(
            PointerDevice::Pen,
            Some(RawPenData {
                pressure: Some(u32::MAX),
                tilt_x: Some(i32::MAX),
                tilt_y: Some(i32::MIN),
            }),
        );
        assert_eq!(clamped.dynamics.pressure, 1.0);
        assert_eq!(clamped.dynamics.tilt_x, 1.0);
        assert_eq!(clamped.dynamics.tilt_y, -1.0);
    }

    #[test]
    fn missing_pen_capabilities_keep_constant_width_fallbacks() {
        let missing_api = normalize_pointer_sample(PointerDevice::Pen, None);
        assert_eq!(missing_api.dynamics, PointerDynamics::FALLBACK);

        let missing_fields = normalize_pointer_sample(
            PointerDevice::Pen,
            Some(RawPenData {
                pressure: None,
                tilt_x: None,
                tilt_y: None,
            }),
        );
        assert_eq!(missing_fields.dynamics, PointerDynamics::FALLBACK);
    }

    #[test]
    fn zero_pressure_is_preserved_when_the_device_reports_it() {
        let sample = normalize_pointer_sample(
            PointerDevice::Pen,
            Some(RawPenData {
                pressure: Some(0),
                tilt_x: None,
                tilt_y: None,
            }),
        );
        assert_eq!(sample.dynamics.pressure, 0.0);
    }

    #[test]
    fn non_pen_devices_ignore_accidental_pen_metadata() {
        let raw = Some(RawPenData {
            pressure: Some(1),
            tilt_x: Some(90),
            tilt_y: Some(-90),
        });
        for device in [
            PointerDevice::Mouse,
            PointerDevice::Touch,
            PointerDevice::Touchpad,
            PointerDevice::Unknown,
        ] {
            let sample = normalize_pointer_sample(device, raw);
            assert_eq!(sample.device, device);
            assert_eq!(sample.dynamics, PointerDynamics::FALLBACK);
        }
    }
}
