//! Standard-stream entry point for Rootlight's MCP bridge.

#![forbid(unsafe_code)]

use std::{env, ffi::OsString, path::PathBuf, process::ExitCode, sync::Arc};

use rootlight_client::{Client, ConnectPolicy};
use rootlight_mcp::{
    FirstSliceClientPort, FirstSliceToolExecutor, NativeFirstSliceClientPort, RequestHandler,
    Session, StdioLimits, ToolExecutorBuildError, ToolRegistryError, ToolRouter,
    UnavailableFirstSliceClientPort, serve,
};
use rootlight_mcp_contract::ExposureProfile;
use rootlight_runtime::RuntimePaths;
use tokio::io::{BufReader, BufWriter};
use tokio::sync::watch;

const STATE_DIR_ENV: &str = "ROOTLIGHT_STATE_DIR";
const RUNTIME_DIR_ENV: &str = "ROOTLIGHT_RUNTIME_DIR";
const PROFILE_CEILING_ENV: &str = "ROOTLIGHT_MCP_PROFILE_CEILING";
const PROFILE_ENV: &str = "ROOTLIGHT_MCP_PROFILE";

fn main() -> ExitCode {
    let mode = match bridge_mode() {
        Ok(mode) => mode,
        Err(()) => {
            eprintln!("rootlight-mcp terminated: arguments");
            return ExitCode::FAILURE;
        }
    };
    let (ceiling, default_profile) = match profile_policy_from_overrides(
        env::var_os(PROFILE_CEILING_ENV),
        env::var_os(PROFILE_ENV),
    ) {
        Ok(policy) => policy,
        Err(()) => {
            eprintln!("rootlight-mcp terminated: invalid {PROFILE_CEILING_ENV} or {PROFILE_ENV}");
            return ExitCode::FAILURE;
        }
    };
    let (profile_sender, profile_receiver) = watch::channel(default_profile);
    let handler = match request_handler(mode, profile_receiver, ceiling) {
        Ok(handler) => handler,
        Err(_) => {
            eprintln!("rootlight-mcp terminated: initialization");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(_) => {
            eprintln!("rootlight-mcp terminated: async_runtime");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(serve_stdio(handler, ceiling, profile_sender)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rootlight-mcp terminated: {}", error.category());
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BridgeMode {
    Production,
    TransportOnly,
}

fn bridge_mode() -> Result<BridgeMode, ()> {
    let mut arguments = env::args_os().skip(1);
    match (arguments.next(), arguments.next()) {
        (None, None) => Ok(BridgeMode::Production),
        (Some(argument), None) if argument == "--transport-only" => Ok(BridgeMode::TransportOnly),
        _ => Err(()),
    }
}

fn request_handler(
    mode: BridgeMode,
    profile: watch::Receiver<ExposureProfile>,
    ceiling: ExposureProfile,
) -> Result<Arc<dyn RequestHandler>, BridgeInitializationError> {
    match mode {
        BridgeMode::Production => match native_port() {
            Ok(port) => tool_handler(port, profile, ceiling),
            Err(()) => tool_handler(UnavailableFirstSliceClientPort, profile, ceiling),
        },
        BridgeMode::TransportOnly => {
            // Transport conformance must never attach to or launch a user's daemon.
            tool_handler(UnavailableFirstSliceClientPort, profile, ceiling)
        }
    }
}

fn native_port() -> Result<NativeFirstSliceClientPort, ()> {
    let paths = runtime_paths()?;
    let mut client_instance_id = [0_u8; 16];
    getrandom::fill(&mut client_instance_id).map_err(|_| ())?;
    let client =
        Client::connect_or_start(&paths, client_instance_id, ConnectPolicy::StartIfMissing)
            .map_err(|_| ())?;
    Ok(NativeFirstSliceClientPort::new(client))
}

fn runtime_paths() -> Result<RuntimePaths, ()> {
    runtime_paths_from_overrides(env::var_os(STATE_DIR_ENV), env::var_os(RUNTIME_DIR_ENV))
}

fn runtime_paths_from_overrides(
    state: Option<OsString>,
    runtime: Option<OsString>,
) -> Result<RuntimePaths, ()> {
    match (state, runtime) {
        (None, None) => RuntimePaths::resolve().map_err(|_| ()),
        (Some(state), Some(runtime)) if !state.is_empty() && !runtime.is_empty() => {
            RuntimePaths::new(PathBuf::from(state), PathBuf::from(runtime)).map_err(|_| ())
        }
        _ => Err(()),
    }
}

/// Resolves the server exposure-profile policy from environment overrides.
///
/// Returns `(ceiling, default_profile)`. The ceiling defaults to
/// [`ExposureProfile::Developer`]; the requested default defaults to the
/// ceiling and is always clamped below it. An unknown profile name in either
/// override is a hard configuration error so the bridge never guesses a
/// privilege level.
fn profile_policy_from_overrides(
    ceiling: Option<OsString>,
    requested: Option<OsString>,
) -> Result<(ExposureProfile, ExposureProfile), ()> {
    let ceiling = parse_profile_override(ceiling, ExposureProfile::Developer)?;
    let requested = parse_profile_override(requested, ceiling)?;
    Ok((ceiling, requested.clamped_to(ceiling)))
}

/// Parses one optional profile override, falling back to `default` when the
/// value is absent or empty.
fn parse_profile_override(
    raw: Option<OsString>,
    default: ExposureProfile,
) -> Result<ExposureProfile, ()> {
    let Some(raw) = raw else {
        return Ok(default);
    };
    if raw.is_empty() {
        return Ok(default);
    }
    raw.to_str().and_then(ExposureProfile::from_name).ok_or(())
}

fn tool_handler<P>(
    port: P,
    profile: watch::Receiver<ExposureProfile>,
    ceiling: ExposureProfile,
) -> Result<Arc<dyn RequestHandler>, BridgeInitializationError>
where
    P: FirstSliceClientPort,
{
    let executor = FirstSliceToolExecutor::new(port)?;
    Ok(Arc::new(ToolRouter::with_shared_profile(
        executor, profile, ceiling,
    )?))
}

async fn serve_stdio(
    handler: Arc<dyn RequestHandler>,
    ceiling: ExposureProfile,
    profile_sender: watch::Sender<ExposureProfile>,
) -> Result<(), rootlight_mcp::SessionError> {
    let input = BufReader::new(tokio::io::stdin());
    let output = BufWriter::new(tokio::io::stdout());
    let mut session = Session::with_profile(ceiling, profile_sender);
    serve(input, output, &mut session, handler, StdioLimits::default()).await
}

#[derive(Debug)]
struct BridgeInitializationError;

impl From<ToolExecutorBuildError> for BridgeInitializationError {
    fn from(_error: ToolExecutorBuildError) -> Self {
        Self
    }
}

impl From<ToolRegistryError> for BridgeInitializationError {
    fn from(_error: ToolRegistryError) -> Self {
        Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_runtime_path_override_selects_both_directories() {
        let temporary = tempfile::tempdir().expect("temporary directory is available");
        let state = temporary.path().join("state");
        let runtime = temporary.path().join("runtime");

        let paths = runtime_paths_from_overrides(
            Some(state.clone().into_os_string()),
            Some(runtime.clone().into_os_string()),
        )
        .expect("complete path overrides resolve");

        assert_eq!(paths.state_dir(), state);
        assert_eq!(paths.runtime_dir(), runtime);
    }

    #[test]
    fn absent_runtime_path_override_uses_default_directories() {
        let expected = RuntimePaths::resolve().expect("default runtime paths resolve");
        let paths =
            runtime_paths_from_overrides(None, None).expect("absent overrides use defaults");

        assert_eq!(paths.state_dir(), expected.state_dir());
        assert_eq!(paths.runtime_dir(), expected.runtime_dir());
    }

    #[test]
    fn incomplete_or_empty_runtime_path_override_is_rejected() {
        let path = std::env::temp_dir().join("rootlight-mcp-path-override");
        let value = path.into_os_string();
        let empty = OsString::new();

        for (state, runtime) in [
            (Some(value.clone()), None),
            (None, Some(value.clone())),
            (Some(empty.clone()), Some(value.clone())),
            (Some(value.clone()), Some(empty)),
        ] {
            assert!(runtime_paths_from_overrides(state, runtime).is_err());
        }
    }

    #[test]
    fn absent_profile_overrides_default_to_developer_ceiling() {
        let (ceiling, default_profile) =
            profile_policy_from_overrides(None, None).expect("absent overrides resolve");
        assert_eq!(ceiling, ExposureProfile::Developer);
        assert_eq!(default_profile, ExposureProfile::Developer);
    }

    #[test]
    fn empty_profile_overrides_fall_back_to_defaults() {
        let (ceiling, default_profile) =
            profile_policy_from_overrides(Some(OsString::new()), Some(OsString::new()))
                .expect("empty overrides resolve");
        assert_eq!(ceiling, ExposureProfile::Developer);
        assert_eq!(default_profile, ExposureProfile::Developer);
    }

    #[test]
    fn requested_profile_defaults_to_the_ceiling() {
        let (ceiling, default_profile) =
            profile_policy_from_overrides(Some("analysis".into()), None)
                .expect("ceiling-only override resolves");
        assert_eq!(ceiling, ExposureProfile::Analysis);
        assert_eq!(default_profile, ExposureProfile::Analysis);
    }

    #[test]
    fn requested_profile_is_clamped_to_the_ceiling() {
        let (ceiling, default_profile) =
            profile_policy_from_overrides(Some("scout".into()), Some("developer".into()))
                .expect("policy resolves");
        assert_eq!(ceiling, ExposureProfile::Scout);
        assert_eq!(default_profile, ExposureProfile::Scout);
    }

    #[test]
    fn a_lower_requested_profile_is_left_unclamped() {
        let (_ceiling, default_profile) =
            profile_policy_from_overrides(Some("developer".into()), Some("scout".into()))
                .expect("policy resolves");
        assert_eq!(default_profile, ExposureProfile::Scout);
    }

    #[test]
    fn unknown_profile_overrides_are_rejected() {
        assert!(profile_policy_from_overrides(Some("admin".into()), None).is_err());
        assert!(profile_policy_from_overrides(None, Some("admin".into())).is_err());
        // A valid-UTF-8 but undocumented name is still rejected.
        assert!(profile_policy_from_overrides(Some("\u{ff}".into()), None).is_err());
    }
}
