use objc2::MainThreadMarker;

unsafe extern "C" {
    fn spaceman_show_accessibility_guide();
}

pub(crate) fn show() {
    if MainThreadMarker::new().is_some() {
        unsafe { spaceman_show_accessibility_guide() };
    }
}
