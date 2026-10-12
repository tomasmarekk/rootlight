//! Process entry point for the Rootlight loopback web host.

#![forbid(unsafe_code)]

use std::{env, ffi::OsString, process::ExitCode};

use rootlight_web::WebError;

#[tokio::main]
async fn main() -> ExitCode {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    match rootlight_web::run(arguments.iter().cloned()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("rootlight-web: {error}");
            ExitCode::from(failure_status(&arguments, &error))
        }
    }
}

fn failure_status(arguments: &[OsString], error: &WebError) -> u8 {
    // Only the CLI-owned exact service invocation participates in the private
    // startup contract; an option value named `--service` is not an opt-in.
    if matches!(arguments, [argument] if argument == "--service") {
        error
            .service_startup_failure()
            .map_or(1, |failure| failure.exit_code())
    } else {
        1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_client::ClientError;

    #[test]
    fn typed_timeout_survives_an_actual_service_child_exit() {
        let status = std::process::Command::new(env::current_exe().expect("test binary resolves"))
            .args(["--exact", "tests::service_timeout_exit_fixture"])
            .env("ROOTLIGHT_TEST_WEB_TIMEOUT_EXIT", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("service startup fixture exits");
        let failure = status
            .code()
            .and_then(rootlight_runtime::WebServiceStartupFailure::from_exit_code)
            .expect("exact child preserves its typed startup failure");
        assert_eq!(failure.diagnostic_reason(), "daemon_start_timeout");
    }

    #[test]
    fn service_timeout_exit_fixture() {
        if env::var_os("ROOTLIGHT_TEST_WEB_TIMEOUT_EXIT").as_deref()
            == Some(std::ffi::OsStr::new("1"))
        {
            let error = WebError::from_daemon_start(ClientError::DaemonStartTimedOut);
            std::process::exit(i32::from(failure_status(
                &[OsString::from("--service")],
                &error,
            )));
        }
    }

    #[test]
    fn only_exact_service_startup_reports_the_typed_daemon_timeout() {
        let timeout = WebError::from_daemon_start(ClientError::DaemonStartTimedOut);
        assert_eq!(failure_status(&[OsString::from("--service")], &timeout), 80);
        assert_eq!(
            failure_status(&[OsString::from("--service")], &WebError::DaemonUnavailable),
            81
        );
        for arguments in [
            vec![],
            vec![OsString::from("--no-open")],
            vec![OsString::from("--asset-dir"), OsString::from("--service")],
            vec![OsString::from("--service"), OsString::from("--unknown")],
        ] {
            assert_eq!(failure_status(&arguments, &timeout), 1);
            assert_eq!(failure_status(&arguments, &WebError::DaemonUnavailable), 1);
        }
        assert_eq!(
            failure_status(
                &[OsString::from("--service")],
                &WebError::ListenerUnavailable
            ),
            1
        );
    }
}
