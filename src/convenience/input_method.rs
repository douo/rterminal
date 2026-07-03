#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) enum InputMode {
    Latin,
    Cjk,
    #[default]
    Unknown,
}

pub(crate) fn current_input_mode() -> InputMode {
    platform::current_input_mode()
}

pub(crate) fn sync_text_input_context_to_current_source(
    window: &gpui::Window,
    discard_marked_text: bool,
) {
    platform::sync_text_input_context_to_current_source(window, discard_marked_text);
}

pub(crate) fn discard_text_input_context_marked_text(window: &gpui::Window) {
    platform::discard_text_input_context_marked_text(window);
}

pub(crate) struct InputModeChangeListener {
    receiver: async_channel::Receiver<()>,
    _platform_listener: platform::InputModeChangeListener,
}

impl InputModeChangeListener {
    pub(crate) fn start() -> Option<Self> {
        let (sender, receiver) = async_channel::bounded(1);
        let platform_listener = platform::InputModeChangeListener::new(sender)?;
        Some(Self {
            receiver,
            _platform_listener: platform_listener,
        })
    }

    pub(crate) async fn changed(&self) -> bool {
        self.receiver.recv().await.is_ok()
    }
}

fn classify_input_source(
    source_id: Option<&str>,
    mode_id: Option<&str>,
    languages: &[String],
) -> InputMode {
    let haystack = [source_id.unwrap_or_default(), mode_id.unwrap_or_default()]
        .join(" ")
        .to_ascii_lowercase();

    if languages.iter().any(|language| {
        let language = language.to_ascii_lowercase();
        language.starts_with("zh")
            || language.starts_with("ja")
            || language.starts_with("ko")
            || language.contains("hans")
            || language.contains("hant")
    }) {
        return InputMode::Cjk;
    }

    if [
        "zh",
        "hans",
        "hant",
        "pinyin",
        "shuangpin",
        "wubi",
        "stroke",
        "zhuyin",
        "cangjie",
        "scim",
        "tcim",
        "rime",
        "sogou",
        "baidu",
        "qq",
        "wetype",
        "doubao",
    ]
    .iter()
    .any(|needle| haystack.contains(needle))
    {
        return InputMode::Cjk;
    }

    if languages.iter().any(|language| {
        let language = language.to_ascii_lowercase();
        language.starts_with("en") || language.starts_with("fr") || language.starts_with("de")
    }) || haystack.contains("keylayout")
        || haystack.contains("abc")
        || haystack.contains("roman")
        || source_id.is_some()
        || mode_id.is_some()
    {
        return InputMode::Latin;
    }

    InputMode::Unknown
}

#[cfg(target_os = "macos")]
mod platform {
    use super::{InputMode, classify_input_source};
    use async_channel::Sender;
    use cocoa::{
        base::{id, nil},
        foundation::NSString,
    };
    use objc::{msg_send, sel, sel_impl};
    use raw_window_handle::RawWindowHandle;
    use std::ffi::{CStr, c_char, c_void};
    use std::ptr;
    use std::sync::Arc;

    type CFArrayRef = *const c_void;
    type CFDictionaryRef = *const c_void;
    type CFIndex = isize;
    type CFNotificationCenterRef = *const c_void;
    type CFNotificationSuspensionBehavior = i32;
    type CFStringRef = *const c_void;
    type CFTypeRef = *const c_void;
    type TISInputSourceRef = *const c_void;
    type CFNotificationCallback = extern "C" fn(
        center: CFNotificationCenterRef,
        observer: *mut c_void,
        name: CFStringRef,
        object: *const c_void,
        user_info: CFDictionaryRef,
    );

    const K_CFSTRING_ENCODING_UTF8: u32 = 0x0800_0100;
    const CF_NOTIFICATION_SUSPENSION_BEHAVIOR_DELIVER_IMMEDIATELY:
        CFNotificationSuspensionBehavior = 4;

    #[link(name = "Carbon", kind = "framework")]
    unsafe extern "C" {
        static kTISPropertyInputModeID: CFStringRef;
        static kTISPropertyInputSourceID: CFStringRef;
        static kTISPropertyInputSourceLanguages: CFStringRef;
        static kTISNotifySelectedKeyboardInputSourceChanged: CFStringRef;

        fn TISCopyCurrentKeyboardInputSource() -> TISInputSourceRef;
        fn TISGetInputSourceProperty(
            input_source: TISInputSourceRef,
            property_key: CFStringRef,
        ) -> CFTypeRef;
    }

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFNotificationCenterAddObserver(
            center: CFNotificationCenterRef,
            observer: *const c_void,
            call_back: CFNotificationCallback,
            name: CFStringRef,
            object: *const c_void,
            suspension_behavior: CFNotificationSuspensionBehavior,
        );
        fn CFNotificationCenterGetDistributedCenter() -> CFNotificationCenterRef;
        fn CFNotificationCenterRemoveObserver(
            center: CFNotificationCenterRef,
            observer: *const c_void,
            name: CFStringRef,
            object: *const c_void,
        );
        fn CFArrayGetCount(the_array: CFArrayRef) -> CFIndex;
        fn CFArrayGetValueAtIndex(the_array: CFArrayRef, idx: CFIndex) -> *const c_void;
        fn CFRelease(cf: CFTypeRef);
        fn CFStringGetCString(
            the_string: CFStringRef,
            buffer: *mut c_char,
            buffer_size: CFIndex,
            encoding: u32,
        ) -> u8;
        fn CFStringGetCStringPtr(the_string: CFStringRef, encoding: u32) -> *const c_char;
        fn CFStringGetLength(the_string: CFStringRef) -> CFIndex;
        fn CFStringGetMaximumSizeForEncoding(length: CFIndex, encoding: u32) -> CFIndex;
    }

    pub(super) fn current_input_mode() -> InputMode {
        unsafe {
            current_input_source_snapshot()
                .map(|snapshot| {
                    classify_input_source(
                        (!snapshot.source_id.is_empty()).then_some(snapshot.source_id.as_str()),
                        (!snapshot.mode_id.is_empty()).then_some(snapshot.mode_id.as_str()),
                        &snapshot.languages,
                    )
                })
                .unwrap_or(InputMode::Unknown)
        }
    }

    pub(super) fn sync_text_input_context_to_current_source(
        window: &gpui::Window,
        discard_marked_text: bool,
    ) {
        unsafe {
            let Some(input_context) = text_input_context_for_window(window) else {
                return;
            };
            if discard_marked_text {
                let _: () = msg_send![input_context, discardMarkedText];
            }
            let Some(snapshot) = current_input_source_snapshot() else {
                return;
            };
            if snapshot.source_id.is_empty() {
                return;
            }

            let source_id = autoreleased_nsstring(&snapshot.source_id);
            let _: () = msg_send![input_context, setSelectedKeyboardInputSource: source_id];
            let _: () = msg_send![input_context, invalidateCharacterCoordinates];
        }
    }

    pub(super) fn discard_text_input_context_marked_text(window: &gpui::Window) {
        unsafe {
            let Some(input_context) = text_input_context_for_window(window) else {
                return;
            };
            let _: () = msg_send![input_context, discardMarkedText];
            let _: () = msg_send![input_context, invalidateCharacterCoordinates];
        }
    }

    pub(super) struct InputModeChangeListener {
        center: CFNotificationCenterRef,
        observer: *const NotificationObserver,
    }

    struct NotificationObserver {
        sender: Sender<()>,
    }

    impl InputModeChangeListener {
        pub(super) fn new(sender: Sender<()>) -> Option<Self> {
            unsafe {
                let center = CFNotificationCenterGetDistributedCenter();
                if center.is_null() {
                    return None;
                }

                let observer = Arc::into_raw(Arc::new(NotificationObserver { sender }));
                CFNotificationCenterAddObserver(
                    center,
                    observer.cast(),
                    input_source_changed,
                    kTISNotifySelectedKeyboardInputSourceChanged,
                    ptr::null(),
                    CF_NOTIFICATION_SUSPENSION_BEHAVIOR_DELIVER_IMMEDIATELY,
                );

                Some(Self { center, observer })
            }
        }
    }

    impl Drop for InputModeChangeListener {
        fn drop(&mut self) {
            unsafe {
                CFNotificationCenterRemoveObserver(
                    self.center,
                    self.observer.cast(),
                    kTISNotifySelectedKeyboardInputSourceChanged,
                    ptr::null(),
                );
                drop(Arc::from_raw(self.observer));
            }
        }
    }

    extern "C" fn input_source_changed(
        _center: CFNotificationCenterRef,
        observer: *mut c_void,
        _name: CFStringRef,
        _object: *const c_void,
        _user_info: CFDictionaryRef,
    ) {
        if observer.is_null() {
            return;
        }

        let observer = observer.cast::<NotificationObserver>();
        unsafe {
            Arc::increment_strong_count(observer);
            let observer = Arc::from_raw(observer);
            let _ = observer.sender.try_send(());
        }
    }

    struct InputSourceSnapshot {
        source_id: String,
        mode_id: String,
        languages: Vec<String>,
    }

    unsafe fn current_input_source_snapshot() -> Option<InputSourceSnapshot> {
        let source = unsafe { TISCopyCurrentKeyboardInputSource() };
        if source.is_null() {
            return None;
        }

        let source_id =
            unsafe { input_source_string_property(source, kTISPropertyInputSourceID) }
                .unwrap_or_default();
        let mode_id = unsafe { input_source_string_property(source, kTISPropertyInputModeID) }
            .unwrap_or_default();
        let languages = unsafe { input_source_languages(source) };
        unsafe { CFRelease(source as CFTypeRef) };

        Some(InputSourceSnapshot {
            source_id,
            mode_id,
            languages,
        })
    }

    unsafe fn text_input_context_for_window(window: &gpui::Window) -> Option<id> {
        let window_handle = raw_window_handle::HasWindowHandle::window_handle(window).ok()?;
        let RawWindowHandle::AppKit(handle) = window_handle.as_raw() else {
            return None;
        };

        let ns_view = handle.ns_view.as_ptr() as id;
        if ns_view == nil {
            return None;
        }

        let input_context: id = unsafe { msg_send![ns_view, inputContext] };
        (input_context != nil).then_some(input_context)
    }

    #[allow(unexpected_cfgs)]
    unsafe fn autoreleased_nsstring(value: &str) -> id {
        let string = unsafe { NSString::alloc(nil).init_str(value) };
        unsafe { msg_send![string, autorelease] }
    }

    unsafe fn input_source_string_property(
        source: TISInputSourceRef,
        property_key: CFStringRef,
    ) -> Option<String> {
        let value = unsafe { TISGetInputSourceProperty(source, property_key) } as CFStringRef;
        unsafe { cf_string_to_string(value) }
    }

    unsafe fn input_source_languages(source: TISInputSourceRef) -> Vec<String> {
        let array = unsafe { TISGetInputSourceProperty(source, kTISPropertyInputSourceLanguages) }
            as CFArrayRef;
        if array.is_null() {
            return Vec::new();
        }

        let count = unsafe { CFArrayGetCount(array) };
        let mut languages = Vec::with_capacity(count.max(0) as usize);
        for index in 0..count {
            let value = unsafe { CFArrayGetValueAtIndex(array, index) } as CFStringRef;
            if let Some(language) = unsafe { cf_string_to_string(value) } {
                languages.push(language);
            }
        }
        languages
    }

    unsafe fn cf_string_to_string(value: CFStringRef) -> Option<String> {
        if value.is_null() {
            return None;
        }

        let direct = unsafe { CFStringGetCStringPtr(value, K_CFSTRING_ENCODING_UTF8) };
        if !direct.is_null() {
            return Some(
                unsafe { CStr::from_ptr(direct) }
                    .to_string_lossy()
                    .into_owned(),
            );
        }

        let length = unsafe { CFStringGetLength(value) };
        let max_size =
            unsafe { CFStringGetMaximumSizeForEncoding(length, K_CFSTRING_ENCODING_UTF8) };
        if max_size < 0 {
            return None;
        }

        let mut buffer = vec![0u8; max_size as usize + 1];
        let ok = unsafe {
            CFStringGetCString(
                value,
                buffer.as_mut_ptr().cast(),
                buffer.len() as CFIndex,
                K_CFSTRING_ENCODING_UTF8,
            )
        };
        if ok == 0 {
            return None;
        }

        let nul = buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(buffer.len());
        Some(String::from_utf8_lossy(&buffer[..nul]).into_owned())
    }
}

#[cfg(not(target_os = "macos"))]
mod platform {
    use async_channel::Sender;

    use super::InputMode;

    pub(super) fn current_input_mode() -> InputMode {
        InputMode::Unknown
    }

    pub(super) fn sync_text_input_context_to_current_source(
        _window: &gpui::Window,
        _discard_marked_text: bool,
    ) {
    }

    pub(super) fn discard_text_input_context_marked_text(_window: &gpui::Window) {}

    pub(super) struct InputModeChangeListener;

    impl InputModeChangeListener {
        pub(super) fn new(_sender: Sender<()>) -> Option<Self> {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{InputMode, classify_input_source};

    fn languages(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn classifies_apple_abc_as_latin() {
        assert_eq!(
            classify_input_source(Some("com.apple.keylayout.ABC"), None, &languages(&["en"])),
            InputMode::Latin
        );
    }

    #[test]
    fn classifies_chinese_language_as_cjk() {
        assert_eq!(
            classify_input_source(
                Some("com.apple.inputmethod.SCIM.ITABC"),
                None,
                &languages(&["zh-Hans"])
            ),
            InputMode::Cjk
        );
    }

    #[test]
    fn classifies_third_party_pinyin_as_cjk() {
        assert_eq!(
            classify_input_source(Some("com.vendor.inputmethod.pinyin"), None, &languages(&[])),
            InputMode::Cjk
        );
    }

    #[test]
    fn missing_source_is_unknown() {
        assert_eq!(classify_input_source(None, None, &[]), InputMode::Unknown);
    }
}
