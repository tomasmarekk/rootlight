//! Opt-in, source-free timing records for one daemon startup.
//!
//! Records survive an interrupted startup without granting recovery authority or
//! changing readiness. Private publication and bounded writes are best effort.

use std::{
    ffi::OsStr,
    io::Write as _,
    sync::{Mutex, OnceLock},
    time::Instant,
};

use serde::Serialize;

use crate::{PrivateOutputFile, RuntimePaths};

const ENVIRONMENT_FLAG: &str = "ROOTLIGHT_STARTUP_DIAGNOSTICS";
const SCHEMA: &str = "rootlight.startup-diagnostics/1";
const MAX_RECORDS: usize = 64;
const MAX_BYTES: usize = 8192;
static RECORDER: OnceLock<Mutex<Option<Recorder>>> = OnceLock::new();

/// Closed, ordered startup boundaries; no caller-supplied text enters a record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StartupStage {
    /// Private runtime paths have been prepared and validated.
    RuntimeReady,
    /// Opening and initializing the durable operation journal is about to begin.
    JournalOpen,
    /// Opening and initializing the durable operation journal has completed.
    JournalReady,
    /// Reading the installed adapter's complete executable identity is about to begin.
    AdapterIdentityOpen,
    /// The complete installed adapter identity has been authenticated.
    AdapterIdentityReady,
    /// Opening the durable analysis service is about to begin.
    ServiceOpen,
    /// The durable analysis service has opened, before recovery and worker setup.
    ServiceReady,
    /// The local daemon listener has been bound.
    ListenerReady,
    /// The authenticated daemon discovery record has been published.
    DiscoveryPublished,
}

/// Initializes this process's recorder only when the exact opt-in flag is `1`.
///
/// Call after owner preparation. The only destination is a newly created private
/// `startup-diagnostics-<pid>.jsonl` in the validated state directory. Unset or
/// other flag values perform no file access. Every setup failure is ignored.
pub fn initialize(paths: &RuntimePaths) {
    let flag = std::env::var_os(ENVIRONMENT_FLAG);
    if !enabled(flag.as_deref()) {
        return;
    }
    RECORDER.get_or_init(|| Mutex::new(Recorder::open(paths)));
    record(StartupStage::RuntimeReady);
}

/// Appends one static phase and monotonic microsecond offset when opted in.
///
/// Duplicate and out-of-order phases are ignored. Writes are unbuffered so a
/// killed child can leave a useful prefix; no fsync or retry is added to startup.
/// Contention, serialization, limits, and I/O failures never propagate
/// into daemon behavior. The file contains at most 64 records and 8192 bytes.
pub fn record(stage: StartupStage) {
    let Some(recorder) = RECORDER.get() else {
        return;
    };
    let Ok(mut guard) = recorder.try_lock() else {
        return;
    };
    if let Some(recorder) = guard.as_mut() {
        let elapsed_us = u64::try_from(recorder.started.elapsed().as_micros()).unwrap_or(u64::MAX);
        if !recorder.append(stage, elapsed_us) {
            *guard = None;
        }
    }
}

fn enabled(flag: Option<&OsStr>) -> bool {
    flag == Some(OsStr::new("1"))
}

fn filename() -> String {
    format!("startup-diagnostics-{}.jsonl", std::process::id())
}

#[derive(Debug)]
struct Recorder {
    file: PrivateOutputFile,
    started: Instant,
    previous: Option<StartupStage>,
    elapsed_us: u64,
    records: usize,
    bytes: usize,
}

impl Recorder {
    fn open(paths: &RuntimePaths) -> Option<Self> {
        let started = Instant::now();
        paths.validate_client().ok()?;
        let file = PrivateOutputFile::create(&paths.state_dir().join(filename())).ok()?;
        Some(Self {
            file,
            started,
            previous: None,
            elapsed_us: 0,
            records: 0,
            bytes: 0,
        })
    }

    fn append(&mut self, stage: StartupStage, elapsed_us: u64) -> bool {
        if self.previous.is_some_and(|previous| stage <= previous) {
            return true;
        }
        if self.records >= MAX_RECORDS || elapsed_us < self.elapsed_us {
            return false;
        }
        #[derive(Serialize)]
        struct Record {
            schema: &'static str,
            stage: StartupStage,
            elapsed_us: u64,
        }
        let Ok(mut bytes) = serde_json::to_vec(&Record {
            schema: SCHEMA,
            stage,
            elapsed_us,
        }) else {
            return false;
        };
        bytes.push(b'\n');
        if bytes.len() > MAX_BYTES.saturating_sub(self.bytes) {
            return false;
        }
        if self.file.write_all(&bytes).is_err() {
            return false;
        }
        self.records += 1;
        self.bytes += bytes.len();
        self.previous = Some(stage);
        self.elapsed_us = elapsed_us;
        true
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use super::*;

    fn prepared_paths() -> (tempfile::TempDir, RuntimePaths) {
        let root = tempfile::tempdir().expect("temporary root");
        let canonical = root
            .path()
            .canonicalize()
            .expect("canonical temporary root");
        let paths = RuntimePaths::new(canonical.join("state"), canonical.join("runtime"))
            .expect("valid private paths");
        paths.prepare_owner().expect("prepared private paths");
        (root, paths)
    }

    fn records(paths: &RuntimePaths) -> Vec<serde_json::Value> {
        fs::read_to_string(paths.state_dir().join(filename()))
            .expect("retained startup prefix")
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid source-free record"))
            .collect()
    }

    #[test]
    fn diagnostics_subprocess_entry() {
        let Some(root) = std::env::var_os("ROOTLIGHT_STARTUP_DIAGNOSTICS_TEST_ROOT") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let paths = RuntimePaths::new(root.join("state"), root.join("runtime"))
            .expect("private child paths");
        initialize(&paths);
        record(StartupStage::JournalOpen);
        record(StartupStage::JournalReady);
        record(StartupStage::JournalOpen);
    }

    #[test]
    fn public_recorder_is_opt_in_and_preserves_a_prefix_after_process_exit() {
        for flag in [None, Some("0"), Some("1/private-path"), Some("1")] {
            let (_root, paths) = prepared_paths();
            let mut command = Command::new(std::env::current_exe().expect("test executable"));
            command
                .args([
                    "--exact",
                    "startup_diagnostics::tests::diagnostics_subprocess_entry",
                    "--test-threads=1",
                ])
                .env(
                    "ROOTLIGHT_STARTUP_DIAGNOSTICS_TEST_ROOT",
                    paths.state_dir().parent().expect("fixture root"),
                )
                .env_remove(ENVIRONMENT_FLAG);
            if let Some(flag) = flag {
                command.env(ENVIRONMENT_FLAG, flag);
            }
            let child = command.spawn().expect("isolated observer process");
            let child_pid = child.id();
            let output = child.wait_with_output().expect("observer process exits");
            assert!(output.status.success());
            let output_path = paths
                .state_dir()
                .join(format!("startup-diagnostics-{child_pid}.jsonl"));
            if flag != Some("1") {
                assert!(!output_path.exists());
                assert_eq!(
                    fs::read_dir(paths.state_dir()).expect("state root").count(),
                    0
                );
                continue;
            }
            let text = fs::read_to_string(output_path).expect("child prefix survives exit");
            let values: Vec<serde_json::Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).expect("child record"))
                .collect();
            assert_eq!(values.len(), 3);
            let mut previous = 0;
            for (value, stage) in
                values
                    .iter()
                    .zip(["runtime_ready", "journal_open", "journal_ready"])
            {
                assert_eq!(value.as_object().expect("object").len(), 3);
                assert_eq!(value["schema"], SCHEMA);
                assert_eq!(value["stage"], stage);
                let elapsed = value["elapsed_us"].as_u64().expect("u64 elapsed");
                assert!(elapsed >= previous);
                previous = elapsed;
            }
        }
    }

    #[test]
    fn diagnostics_require_exact_opt_in_and_do_not_prepare_missing_paths() {
        for flag in [
            None,
            Some(""),
            Some("0"),
            Some("true"),
            Some("1/private-path"),
        ] {
            assert!(!enabled(flag.map(OsStr::new)));
        }
        assert!(enabled(Some(OsStr::new("1"))));
        let root = tempfile::tempdir().expect("temporary root");
        let canonical = root
            .path()
            .canonicalize()
            .expect("canonical temporary root");
        let paths = RuntimePaths::new(canonical.join("state"), canonical.join("runtime"))
            .expect("valid paths");
        assert!(Recorder::open(&paths).is_none());
        assert!(!paths.state_dir().exists());
        assert!(!paths.runtime_dir().exists());
    }

    #[test]
    fn interrupted_startup_retains_only_exact_static_fields_and_real_offsets() {
        let (_root, paths) = prepared_paths();
        let mut recorder = Recorder::open(&paths).expect("private recorder");
        assert!(recorder.append(StartupStage::RuntimeReady, 0));
        assert!(recorder.append(StartupStage::JournalOpen, 23));
        assert!(recorder.append(StartupStage::JournalReady, 24));
        assert!(recorder.append(StartupStage::AdapterIdentityOpen, 1234));
        drop(recorder);
        let values = records(&paths);
        assert_eq!(values.len(), 4);
        for (value, (stage, elapsed)) in values.iter().zip([
            ("runtime_ready", 0),
            ("journal_open", 23),
            ("journal_ready", 24),
            ("adapter_identity_open", 1234),
        ]) {
            assert_eq!(value.as_object().expect("object").len(), 3);
            assert_eq!(value["schema"], SCHEMA);
            assert_eq!(value["stage"], stage);
            assert_eq!(value["elapsed_us"], elapsed);
        }
        let text = fs::read_to_string(paths.state_dir().join(filename())).expect("prefix");
        assert!(!text.contains(&paths.state_dir().to_string_lossy().to_string()));
        assert!(text.len() <= MAX_BYTES);
    }

    #[test]
    fn duplicate_out_of_order_and_backward_clock_records_do_not_enter_output() {
        let (_root, paths) = prepared_paths();
        let mut recorder = Recorder::open(&paths).expect("private recorder");
        assert!(recorder.append(StartupStage::RuntimeReady, 10));
        assert!(recorder.append(StartupStage::JournalReady, 10));
        assert!(recorder.append(StartupStage::JournalReady, 20));
        assert!(recorder.append(StartupStage::JournalOpen, 20));
        assert!(!recorder.append(StartupStage::ServiceOpen, 9));
        drop(recorder);
        assert_eq!(records(&paths).len(), 2);
    }

    #[test]
    fn record_and_byte_limits_reject_writes_before_exceeding_the_bound() {
        for record_limit in [false, true] {
            let (_root, paths) = prepared_paths();
            let mut recorder = Recorder::open(&paths).expect("private recorder");
            assert!(recorder.append(StartupStage::RuntimeReady, 0));
            if record_limit {
                recorder.records = MAX_RECORDS;
            } else {
                recorder.bytes = MAX_BYTES - 1;
            }
            assert!(!recorder.append(StartupStage::JournalOpen, 1));
            drop(recorder);
            assert_eq!(records(&paths).len(), 1);
        }
    }

    #[test]
    fn existing_output_is_never_replaced_or_appended() {
        let (_root, paths) = prepared_paths();
        let output = paths.state_dir().join(filename());
        fs::write(&output, b"existing evidence").expect("existing output");
        assert!(Recorder::open(&paths).is_none());
        assert_eq!(
            fs::read(output).expect("preserved output"),
            b"existing evidence"
        );
    }

    #[cfg(unix)]
    #[test]
    fn output_rejects_symlinks_and_nonregular_files_without_touching_targets() {
        use std::os::unix::fs::symlink;

        let (_root, paths) = prepared_paths();
        let target = paths.state_dir().join("preserved");
        fs::write(&target, b"untouched").expect("target");
        let output = paths.state_dir().join(filename());
        symlink(&target, &output).expect("output symlink");
        assert!(Recorder::open(&paths).is_none());
        assert_eq!(fs::read(&target).expect("preserved target"), b"untouched");
        fs::remove_file(&output).expect("remove fixture symlink");
        fs::create_dir(&output).expect("directory at output name");
        assert!(Recorder::open(&paths).is_none());
    }

    #[cfg(unix)]
    #[test]
    fn output_is_owner_private_and_insecure_or_aliased_state_roots_are_rejected() {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _, symlink};

        let (_root, paths) = prepared_paths();
        drop(Recorder::open(&paths).expect("private recorder"));
        let metadata = fs::metadata(paths.state_dir().join(filename())).expect("private output");
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);
        assert_eq!(metadata.uid(), crate::effective_user_id());
        let renamed = paths.state_dir().with_file_name("other-state");
        fs::rename(paths.state_dir(), &renamed).expect("move private state fixture");
        symlink(&renamed, paths.state_dir()).expect("aliased state fixture");
        assert!(Recorder::open(&paths).is_none());
        fs::remove_file(paths.state_dir()).expect("remove fixture symlink");
        fs::rename(&renamed, paths.state_dir()).expect("restore state fixture");
        fs::set_permissions(paths.state_dir(), fs::Permissions::from_mode(0o755))
            .expect("insecure fixture directory");
        assert!(Recorder::open(&paths).is_none());
    }
}
