//! Closed startup configuration and trusted asset-root resolution.

use std::{
    env,
    ffi::{OsStr, OsString},
    path::PathBuf,
};

use rootlight_runtime::{RuntimePaths, WEB_UI_PORT};

use crate::error::WebError;

/// Validated process configuration for one loopback web-host instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebConfig {
    open_browser: bool,
    service_mode: bool,
    listen_port: u16,
    asset_root: PathBuf,
}

impl WebConfig {
    /// Parses the closed `rootlight-web` argument grammar.
    ///
    /// Developer-only address and asset overrides are rejected in release
    /// builds so installed binaries cannot silently serve mutable content.
    ///
    /// # Errors
    ///
    /// Returns [`WebError::InvalidArguments`] for an unknown, repeated,
    /// incomplete, non-Unicode, or release-forbidden argument.
    pub(crate) fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Self, WebError> {
        let mut open_browser = true;
        let mut service_mode = false;
        let mut listen_port = None;
        let mut asset_root = None;
        let mut arguments = arguments.into_iter();
        while let Some(argument) = arguments.next() {
            if argument == OsStr::new("--no-open") {
                open_browser = false;
            } else if argument == OsStr::new("--service") {
                if service_mode {
                    return Err(WebError::InvalidArguments);
                }
                service_mode = true;
                open_browser = false;
            } else if argument == OsStr::new("--listen-port") {
                if listen_port.is_some() || !cfg!(debug_assertions) {
                    return Err(WebError::InvalidArguments);
                }
                let value = arguments.next().ok_or(WebError::InvalidArguments)?;
                let value = value.to_str().ok_or(WebError::InvalidArguments)?;
                listen_port = Some(
                    value
                        .parse::<u16>()
                        .ok()
                        .filter(|port| *port != 0)
                        .ok_or(WebError::InvalidArguments)?,
                );
            } else if argument == OsStr::new("--asset-dir") {
                if asset_root.is_some() || !cfg!(debug_assertions) {
                    return Err(WebError::InvalidArguments);
                }
                let value = arguments.next().ok_or(WebError::InvalidArguments)?;
                if value.is_empty() {
                    return Err(WebError::InvalidArguments);
                }
                asset_root = Some(PathBuf::from(value));
            } else {
                return Err(WebError::InvalidArguments);
            }
        }
        let asset_root = match asset_root {
            Some(root) => root,
            None => default_asset_root()?,
        };
        if service_mode && listen_port.is_some() {
            return Err(WebError::InvalidArguments);
        }
        Ok(Self {
            open_browser,
            service_mode,
            listen_port: listen_port.unwrap_or(WEB_UI_PORT),
            asset_root,
        })
    }

    /// Returns whether startup should ask the operating system to open a browser.
    #[must_use]
    pub(crate) const fn open_browser(&self) -> bool {
        self.open_browser
    }

    /// Returns whether this process owns the persistent installed service lifecycle.
    #[must_use]
    pub(crate) const fn service_mode(&self) -> bool {
        self.service_mode
    }

    /// Returns the validated stable loopback port.
    #[must_use]
    pub(crate) const fn listen_port(&self) -> u16 {
        self.listen_port
    }

    /// Returns the trusted immutable asset inventory root.
    #[must_use]
    pub(crate) fn asset_root(&self) -> &std::path::Path {
        &self.asset_root
    }
}

/// Resolves account-private daemon runtime paths with the same paired override
/// contract as the other thin clients.
///
/// # Errors
///
/// Returns [`WebError::RuntimeUnavailable`] when resolution fails or only one
/// of the two required override variables is present.
pub(crate) fn runtime_paths() -> Result<RuntimePaths, WebError> {
    match (
        env::var_os("ROOTLIGHT_STATE_DIR"),
        env::var_os("ROOTLIGHT_RUNTIME_DIR"),
    ) {
        (None, None) => RuntimePaths::resolve().map_err(|_| WebError::RuntimeUnavailable),
        (Some(state), Some(runtime)) if !state.is_empty() && !runtime.is_empty() => {
            RuntimePaths::new(PathBuf::from(state), PathBuf::from(runtime))
                .map_err(|_| WebError::RuntimeUnavailable)
        }
        _ => Err(WebError::RuntimeUnavailable),
    }
}

fn default_asset_root() -> Result<PathBuf, WebError> {
    #[cfg(debug_assertions)]
    {
        Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("frontend")
            .join("dist"))
    }
    #[cfg(not(debug_assertions))]
    {
        let executable = env::current_exe().map_err(|_| WebError::AssetsUnavailable)?;
        let bin = executable
            .parent()
            .filter(|path| path.file_name().is_some_and(|name| name == "bin"))
            .ok_or(WebError::AssetsUnavailable)?;
        let version_root = bin.parent().ok_or(WebError::AssetsUnavailable)?;
        Ok(version_root.join("share").join("rootlight").join("web"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_only_closed_developer_overrides() {
        let config =
            WebConfig::parse([OsString::from("--no-open")]).expect("no-open config validates");
        assert!(!config.open_browser());
        assert_eq!(config.listen_port(), WEB_UI_PORT);

        let service =
            WebConfig::parse([OsString::from("--service")]).expect("service config validates");
        assert!(service.service_mode());
        assert!(!service.open_browser());
        assert_eq!(service.listen_port(), WEB_UI_PORT);

        if cfg!(debug_assertions) {
            let config = WebConfig::parse([
                OsString::from("--listen-port"),
                OsString::from("43127"),
                OsString::from("--asset-dir"),
                OsString::from("fixture-assets"),
            ])
            .expect("developer overrides validate");
            assert_eq!(config.listen_port(), 43_127);
            assert_eq!(config.asset_root(), std::path::Path::new("fixture-assets"));
        }

        for arguments in [
            vec![OsString::from("--listen-port")],
            vec![OsString::from("--listen-port"), OsString::from("0")],
            vec![OsString::from("--asset-dir"), OsString::new()],
            vec![OsString::from("--unknown")],
            vec![OsString::from("--service"), OsString::from("--service")],
            vec![
                OsString::from("--service"),
                OsString::from("--listen-port"),
                OsString::from("43128"),
            ],
        ] {
            assert_eq!(WebConfig::parse(arguments), Err(WebError::InvalidArguments));
        }
    }
}
