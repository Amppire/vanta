//! Explicit platform capabilities keep OS differences out of editor semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DesktopPlatform {
    MacOs,
    Windows,
    Linux,
    Unsupported,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capabilities {
    pub native_dialogs: bool,
    pub accessibility: bool,
    pub atomic_replace_existing: bool,
}
#[must_use]
pub const fn current() -> DesktopPlatform {
    if cfg!(target_os = "macos") {
        DesktopPlatform::MacOs
    } else if cfg!(target_os = "windows") {
        DesktopPlatform::Windows
    } else if cfg!(target_os = "linux") {
        DesktopPlatform::Linux
    } else {
        DesktopPlatform::Unsupported
    }
}
#[must_use]
pub const fn capabilities(platform: DesktopPlatform) -> Capabilities {
    match platform {
        DesktopPlatform::MacOs | DesktopPlatform::Linux => Capabilities {
            native_dialogs: true,
            accessibility: true,
            atomic_replace_existing: true,
        },
        DesktopPlatform::Windows => Capabilities {
            native_dialogs: true,
            accessibility: true,
            atomic_replace_existing: false,
        },
        DesktopPlatform::Unsupported => Capabilities {
            native_dialogs: false,
            accessibility: false,
            atomic_replace_existing: false,
        },
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn supported_desktops_publish_capabilities() {
        for platform in [
            DesktopPlatform::MacOs,
            DesktopPlatform::Windows,
            DesktopPlatform::Linux,
        ] {
            let caps = capabilities(platform);
            assert!(caps.native_dialogs && caps.accessibility);
        }
    }
}
