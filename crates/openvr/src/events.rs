//! Decoding of `VREvent_t` into the small set of events the app reacts to.

use crate::ffi::{self, VREvent};
use crate::OverlayHandle;

/// Mouse button reported by overlay mouse events (`EVRMouseButton`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u32),
}

impl MouseButton {
    fn from_raw(raw: u32) -> Self {
        match raw {
            ffi::VR_MOUSE_BUTTON_LEFT => Self::Left,
            ffi::VR_MOUSE_BUTTON_RIGHT => Self::Right,
            ffi::VR_MOUSE_BUTTON_MIDDLE => Self::Middle,
            other => Self::Other(other),
        }
    }
}

/// An OpenVR event relevant to the app. Everything else is dropped during decoding.
///
/// Mouse coordinates are pixels in the overlay's mouse-scale space with the origin at the
/// **top-left** (OpenVR reports GL-style bottom-left coordinates; they are flipped using the
/// height passed to [`crate::Vr::set_mouse_scale`]).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Event {
    /// `VREvent_Quit`. [`crate::Vr::poll_events`] has already called `AcknowledgeQuit_Exiting`;
    /// the app must tear down ([`crate::Vr::shutdown`]) and return to its retry state.
    Quit,
    MouseMove {
        overlay: OverlayHandle,
        x: f32,
        y: f32,
    },
    MouseDown {
        overlay: OverlayHandle,
        x: f32,
        y: f32,
        button: MouseButton,
    },
    MouseUp {
        overlay: OverlayHandle,
        x: f32,
        y: f32,
        button: MouseButton,
    },
    /// Smooth or discrete scroll; deltas exactly as OpenVR reports them.
    Scroll {
        overlay: OverlayHandle,
        dx: f32,
        dy: f32,
    },
    /// The overlay became visible (e.g. its dashboard tab was selected).
    OverlayShown(OverlayHandle),
    /// The overlay was hidden (e.g. another dashboard tab was selected or the dashboard closed).
    OverlayHidden(OverlayHandle),
    /// The user closed the overlay's dashboard tab with the control bar's close (×) button
    /// (`VREvent_OverlayClosed`; needs [`crate::OverlayFlag::EnableControlBarClose`]).
    OverlayClosed(OverlayHandle),
    DashboardActivated,
    DashboardDeactivated,
    /// `VREvent_TrackedDeviceActivated` for the given device index.
    DeviceConnected(u32),
    /// `VREvent_TrackedDeviceDeactivated` for the given device index.
    DeviceDisconnected(u32),
    /// `VREvent_TrackedDeviceRoleChanged`: controller roles must be re-resolved.
    DeviceRoleChanged(u32),
}

/// Which queue an event came from.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Source {
    /// `IVRSystem::PollNextEvent`.
    System,
    /// `IVROverlay::PollNextOverlayEvent` for `overlay`, whose mouse-scale height is `mouse_height`.
    Overlay {
        overlay: OverlayHandle,
        mouse_height: f32,
    },
}

/// Decodes one raw event. Overlay-specific events (mouse, scroll, shown/hidden) are only accepted
/// from an overlay queue, since they need the overlay handle; global events are accepted from
/// either queue (the runtime may deliver dashboard events on both).
pub(crate) fn decode(raw: &VREvent, source: Source) -> Option<Event> {
    let event_type = raw.eventType;
    let device = raw.trackedDeviceIndex;
    let global = match event_type {
        ffi::VR_EVENT_QUIT => Some(Event::Quit),
        ffi::VR_EVENT_DASHBOARD_ACTIVATED => Some(Event::DashboardActivated),
        ffi::VR_EVENT_DASHBOARD_DEACTIVATED => Some(Event::DashboardDeactivated),
        ffi::VR_EVENT_TRACKED_DEVICE_ACTIVATED => Some(Event::DeviceConnected(device)),
        ffi::VR_EVENT_TRACKED_DEVICE_DEACTIVATED => Some(Event::DeviceDisconnected(device)),
        ffi::VR_EVENT_TRACKED_DEVICE_ROLE_CHANGED => Some(Event::DeviceRoleChanged(device)),
        _ => None,
    };
    if global.is_some() {
        return global;
    }
    let Source::Overlay {
        overlay,
        mouse_height,
    } = source
    else {
        return None;
    };
    let mouse_xy = || {
        (
            raw.data_f32(ffi::MOUSE_X),
            mouse_height - raw.data_f32(ffi::MOUSE_Y),
        )
    };
    let button = || MouseButton::from_raw(raw.data_u32(ffi::MOUSE_BUTTON));
    match event_type {
        ffi::VR_EVENT_MOUSE_MOVE => {
            let (x, y) = mouse_xy();
            Some(Event::MouseMove { overlay, x, y })
        }
        ffi::VR_EVENT_MOUSE_BUTTON_DOWN => {
            let (x, y) = mouse_xy();
            Some(Event::MouseDown {
                overlay,
                x,
                y,
                button: button(),
            })
        }
        ffi::VR_EVENT_MOUSE_BUTTON_UP => {
            let (x, y) = mouse_xy();
            Some(Event::MouseUp {
                overlay,
                x,
                y,
                button: button(),
            })
        }
        ffi::VR_EVENT_SCROLL_SMOOTH | ffi::VR_EVENT_SCROLL_DISCRETE => Some(Event::Scroll {
            overlay,
            dx: raw.data_f32(ffi::SCROLL_XDELTA),
            dy: raw.data_f32(ffi::SCROLL_YDELTA),
        }),
        ffi::VR_EVENT_OVERLAY_SHOWN => Some(Event::OverlayShown(overlay)),
        ffi::VR_EVENT_OVERLAY_HIDDEN => Some(Event::OverlayHidden(overlay)),
        ffi::VR_EVENT_OVERLAY_CLOSED => Some(Event::OverlayClosed(overlay)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(event_type: u32, device: u32, words: &[u32]) -> VREvent {
        let mut ev = VREvent::ZERO;
        ev.eventType = event_type;
        ev.trackedDeviceIndex = device;
        let mut data = [0u8; ffi::VR_EVENT_DATA_SIZE];
        for (i, w) in words.iter().enumerate() {
            data[i * 4..i * 4 + 4].copy_from_slice(&w.to_ne_bytes());
        }
        ev.data = data;
        ev
    }

    const H: OverlayHandle = OverlayHandle(0x1234);
    const OV: Source = Source::Overlay {
        overlay: H,
        mouse_height: 800.0,
    };

    #[test]
    fn global_events_from_system_queue() {
        assert_eq!(decode(&raw(700, 0, &[]), Source::System), Some(Event::Quit));
        assert_eq!(
            decode(&raw(502, 0, &[]), Source::System),
            Some(Event::DashboardActivated)
        );
        assert_eq!(
            decode(&raw(503, 0, &[]), Source::System),
            Some(Event::DashboardDeactivated)
        );
        assert_eq!(
            decode(&raw(100, 3, &[]), Source::System),
            Some(Event::DeviceConnected(3))
        );
        assert_eq!(
            decode(&raw(101, 4, &[]), Source::System),
            Some(Event::DeviceDisconnected(4))
        );
        assert_eq!(
            decode(&raw(108, 5, &[]), Source::System),
            Some(Event::DeviceRoleChanged(5))
        );
        // Global events are also accepted from an overlay queue.
        assert_eq!(
            decode(&raw(502, 0, &[]), OV),
            Some(Event::DashboardActivated)
        );
    }

    #[test]
    fn mouse_events_flip_y_to_top_left() {
        let e = raw(300, 0, &[100.0f32.to_bits(), 700.0f32.to_bits(), 0, 0]);
        assert_eq!(
            decode(&e, OV),
            Some(Event::MouseMove {
                overlay: H,
                x: 100.0,
                y: 100.0
            })
        );
        let e = raw(301, 0, &[1200.0f32.to_bits(), 0.0f32.to_bits(), 1, 0]);
        assert_eq!(
            decode(&e, OV),
            Some(Event::MouseDown {
                overlay: H,
                x: 1200.0,
                y: 800.0,
                button: MouseButton::Left
            })
        );
        let e = raw(302, 0, &[5.0f32.to_bits(), 800.0f32.to_bits(), 2, 0]);
        assert_eq!(
            decode(&e, OV),
            Some(Event::MouseUp {
                overlay: H,
                x: 5.0,
                y: 0.0,
                button: MouseButton::Right
            })
        );
        let e = raw(302, 0, &[0, 0, 4, 0]);
        assert!(matches!(
            decode(&e, OV),
            Some(Event::MouseUp {
                button: MouseButton::Middle,
                ..
            })
        ));
        let e = raw(302, 0, &[0, 0, 9, 0]);
        assert!(matches!(
            decode(&e, OV),
            Some(Event::MouseUp {
                button: MouseButton::Other(9),
                ..
            })
        ));
    }

    #[test]
    fn scroll_events() {
        let e = raw(
            309,
            0,
            &[
                0.25f32.to_bits(),
                (-1.5f32).to_bits(),
                0,
                1.0f32.to_bits(),
                0,
            ],
        );
        assert_eq!(
            decode(&e, OV),
            Some(Event::Scroll {
                overlay: H,
                dx: 0.25,
                dy: -1.5
            })
        );
        let e = raw(305, 0, &[0, 1.0f32.to_bits()]);
        assert_eq!(
            decode(&e, OV),
            Some(Event::Scroll {
                overlay: H,
                dx: 0.0,
                dy: 1.0
            })
        );
    }

    #[test]
    fn overlay_visibility_events() {
        assert_eq!(decode(&raw(500, 0, &[]), OV), Some(Event::OverlayShown(H)));
        assert_eq!(decode(&raw(501, 0, &[]), OV), Some(Event::OverlayHidden(H)));
        assert_eq!(decode(&raw(534, 0, &[]), OV), Some(Event::OverlayClosed(H)));
        assert_eq!(decode(&raw(534, 0, &[]), Source::System), None);
    }

    #[test]
    fn overlay_events_need_overlay_queue() {
        assert_eq!(decode(&raw(300, 0, &[]), Source::System), None);
        assert_eq!(decode(&raw(309, 0, &[]), Source::System), None);
        assert_eq!(decode(&raw(500, 0, &[]), Source::System), None);
    }

    #[test]
    fn unrelated_events_are_ignored() {
        for t in [0, 200, 303, 304, 307, 506, 508, 701, 704, 1_000_000] {
            assert_eq!(decode(&raw(t, 0, &[]), Source::System), None, "{t}");
            assert_eq!(decode(&raw(t, 0, &[]), OV), None, "{t}");
        }
    }
}
