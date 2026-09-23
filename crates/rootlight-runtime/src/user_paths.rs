//! Current-user application paths shared by the daemon and web UI.
//! The platform locations preserve existing Rootlight state across upgrades.

#[cfg(all(unix, not(target_os = "macos")))]
use std::path::Path;
use std::path::PathBuf;

/// Returns the current account's home directory when the platform can resolve it.
#[must_use]
pub fn user_home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use rootlight_vfs::platform::{KnownUserFolder, known_user_folder};
        known_user_folder(KnownUserFolder::Profile).ok()
    }
    #[cfg(unix)]
    {
        std::env::var_os("HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                nix::unistd::User::from_uid(nix::unistd::getuid())
                    .ok()
                    .flatten()
                    .map(|user| user.dir)
            })
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

pub(crate) fn default_state_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        use rootlight_vfs::platform::{KnownUserFolder, known_user_folder};
        let local = known_user_folder(KnownUserFolder::LocalAppData).ok()?;
        // The previous platform resolver required both known folders, even
        // though Rootlight stores durable state only under LocalAppData.
        known_user_folder(KnownUserFolder::RoamingAppData).ok()?;
        Some(local.join("tomasmarekk").join("rootlight").join("data"))
    }
    #[cfg(target_os = "macos")]
    {
        Some(
            user_home_dir()?
                .join("Library/Application Support")
                .join("dev.tomasmarekk.rootlight"),
        )
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let home = user_home_dir()?;
        Some(linux_state_dir(
            &home,
            std::env::var_os("XDG_STATE_HOME").map(PathBuf::from),
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
pub(crate) fn default_runtime_dir() -> Option<PathBuf> {
    linux_runtime_dir(std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn linux_state_dir(home: &Path, state_home: Option<PathBuf>) -> PathBuf {
    state_home
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| home.join(".local/state"))
        .join("rootlight")
}

#[cfg(all(unix, not(target_os = "macos")))]
fn linux_runtime_dir(runtime_home: Option<PathBuf>) -> Option<PathBuf> {
    runtime_home
        .filter(|path| path.is_absolute())
        .map(|path| path.join("rootlight"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_paths_keep_the_existing_known_folder_layout() {
        use rootlight_vfs::platform::{KnownUserFolder, known_user_folder};

        let local = known_user_folder(KnownUserFolder::LocalAppData)
            .expect("local application data resolves");
        let profile = known_user_folder(KnownUserFolder::Profile).expect("profile resolves");
        assert!(local.is_absolute());
        assert!(profile.is_absolute());
        assert_eq!(
            default_state_dir(),
            Some(local.join("tomasmarekk").join("rootlight").join("data"))
        );
        assert_eq!(user_home_dir(), Some(profile));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_state_keeps_the_existing_bundle_location() {
        assert_eq!(
            default_state_dir(),
            Some(
                user_home_dir()
                    .expect("home resolves")
                    .join("Library/Application Support/dev.tomasmarekk.rootlight")
            )
        );
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn linux_paths_reject_relative_xdg_values_without_losing_existing_state() {
        let home = Path::new("/home/account");
        let fallback = home.join(".local/state/rootlight");
        assert_eq!(linux_state_dir(home, None), fallback);
        assert_eq!(linux_state_dir(home, Some("relative".into())), fallback);
        assert_eq!(
            linux_state_dir(home, Some("/private/state".into())),
            PathBuf::from("/private/state/rootlight")
        );
        assert_eq!(linux_runtime_dir(None), None);
        assert_eq!(linux_runtime_dir(Some("relative".into())), None);
        assert_eq!(
            linux_runtime_dir(Some("/private/runtime".into())),
            Some(PathBuf::from("/private/runtime/rootlight"))
        );
    }
}
