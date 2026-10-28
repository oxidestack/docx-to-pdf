//! The server's side of an instance: start it in its own process group with its own
//! temporary profile, wait for its ready line, send it conversions, log its stderr, kill its whole process
//! group and delete its profile directory.
//!
//! What this does not do is the pool's: no start timeout, no
//! conversion timeout, no replacement, no retry. A caller bounds [`Instance::start`] and [`Instance::convert`]
//! with `tokio::time::timeout`; a cancelled call leaves the instance unusable and the caller kills it
//! ([`Instance::kill`]) or drops it. Dropping an instance that is still running kills its process group and
//! deletes its profile, so nothing is left behind whichever way it ends.

use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

use super::child::MAX_IMAGE_DPI_FLAG;
use super::protocol::{Answer, ProtocolError, Ready, Request};
use super::{INSTANCE_MODE, START_FAILED_PREFIX};

/// Where tracing events carrying an instance's stderr go ([`log_stderr_line`]).
pub const STDERR_TARGET: &str = "docx_to_pdf::backend";

/// Logs one line of an instance's stderr under [`STDERR_TARGET`]. Text the backend wrote or gave is a field
/// under the service's own message, never the message, so a log does not reveal which backend runs: the
/// start-failure line is "the backend did not start" with the backend's error text in `backend_error`; the
/// instance's other own lines (`instance: …`) carry no backend text and are their own message; any other line is
/// the backend's, "the backend wrote to stderr" with the line in `backend_output`.
fn log_stderr_line(pid: u32, line: &str) {
    if let Some(error) = line.strip_prefix(START_FAILED_PREFIX) {
        tracing::error!(
            target: STDERR_TARGET,
            instance_pid = pid,
            backend_error = error,
            "the backend did not start"
        );
    } else if line.starts_with("instance: ") {
        tracing::info!(target: STDERR_TARGET, instance_pid = pid, "{line}");
    } else {
        tracing::info!(
            target: STDERR_TARGET,
            instance_pid = pid,
            backend_output = line,
            "the backend wrote to stderr"
        );
    }
}

/// How instances are started: `<program> <leading args…> <profile dir>`, each profile a new directory under
/// `profiles`.
#[derive(Debug, Clone)]
pub struct Launcher {
    program: PathBuf,
    leading_args: Vec<OsString>,
    profiles: PathBuf,
}

impl Launcher {
    /// This binary in instance mode (`docx-to-pdf instance <profile dir>`), profiles under `profiles`.
    pub fn current_exe(profiles: impl Into<PathBuf>) -> std::io::Result<Self> {
        Ok(Self::new(std::env::current_exe()?, profiles))
    }

    /// `program` in instance mode (`<program> instance <profile dir>`), profiles under `profiles`.
    pub fn new(program: impl Into<PathBuf>, profiles: impl Into<PathBuf>) -> Self {
        Self::with_args(program, [INSTANCE_MODE], profiles)
    }

    /// Instances that reduce a picture above `max_image_dpi` to it (`DOCX_TO_PDF_PDF_MAX_IMAGE_DPI`); `None`
    /// embeds pictures as they are.
    #[must_use]
    pub fn with_max_image_dpi(mut self, max_image_dpi: Option<u32>) -> Self {
        if let Some(dpi) = max_image_dpi {
            self.leading_args
                .push(format!("{MAX_IMAGE_DPI_FLAG}{dpi}").into());
        }
        self
    }

    /// `<program> <leading_args…> <profile dir>`: a stand-in instance, for tests.
    pub fn with_args<I, S>(
        program: impl Into<PathBuf>,
        leading_args: I,
        profiles: impl Into<PathBuf>,
    ) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            program: program.into(),
            leading_args: leading_args.into_iter().map(Into::into).collect(),
            profiles: profiles.into(),
        }
    }
}

/// Why an instance did not become ready. Whatever was started is killed and its profile deleted.
#[derive(Debug)]
pub enum StartError {
    /// The profile directory could not be made.
    Profile(std::io::Error),
    /// The process could not be started.
    Spawn(std::io::Error),
    /// The process ended, or closed its stdout, before its ready line.
    Exited(Option<ExitStatus>),
    /// Its first line was not a ready line.
    NotReady(ProtocolError),
}

impl fmt::Display for StartError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Profile(e) => write!(f, "making the instance's profile: {e}"),
            Self::Spawn(e) => write!(f, "starting the instance: {e}"),
            Self::Exited(Some(status)) => {
                write!(f, "the instance ended before it was ready: {status}")
            }
            Self::Exited(None) => write!(f, "the instance ended before it was ready"),
            Self::NotReady(e) => write!(f, "the instance did not report ready: {e}"),
        }
    }
}

impl std::error::Error for StartError {}

/// Why a conversion got no answer.
#[derive(Debug)]
pub enum ConvertError {
    /// The request cannot be sent (a path that cannot travel in a request line). The instance is unaffected.
    Request(ProtocolError),
    /// A dead instance: its pipe closed or it exited during the conversion. Its process group has been killed
    /// and its profile deleted; the status is how it ended.
    Dead(Option<ExitStatus>),
    /// It answered something that is not an answer. It has been killed and its profile deleted.
    BadAnswer(ProtocolError),
    /// It cannot take a conversion: an earlier one was interrupted (its answer would be read as this one's),
    /// or it is dead.
    Unusable,
}

impl fmt::Display for ConvertError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Request(e) => write!(f, "the request cannot be sent: {e}"),
            Self::Dead(Some(status)) => write!(f, "the instance died: {status}"),
            Self::Dead(None) => write!(f, "the instance died"),
            Self::BadAnswer(e) => write!(f, "the instance answered garbage: {e}"),
            Self::Unusable => write!(f, "the instance cannot take a conversion"),
        }
    }
}

impl std::error::Error for ConvertError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Ready for a conversion.
    Idle,
    /// A request was sent and its answer not read: a conversion running, or one that was interrupted.
    Busy,
    /// Killed or ended, reaped, its profile deleted.
    Gone,
}

/// One running instance.
#[derive(Debug)]
pub struct Instance {
    child: Child,
    pid: u32,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    profile: PathBuf,
    ready: Option<Ready>,
    spawn_to_ready: Duration,
    state: State,
}

impl Instance {
    /// Starts an instance and waits for its ready line. Its stderr is logged under [`STDERR_TARGET`].
    pub async fn start(launcher: &Launcher) -> Result<Self, StartError> {
        let started = Instant::now();
        let profile = tempfile::Builder::new()
            .prefix("docx-to-pdf-instance-")
            .tempdir_in(&launcher.profiles)
            .map_err(StartError::Profile)?
            .keep();
        let spawned = Command::new(&launcher.program)
            .args(&launcher.leading_args)
            .arg(&profile)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(child) => child,
            Err(e) => {
                remove_profile(&profile);
                return Err(StartError::Spawn(e));
            }
        };
        let pid = child.id().expect("a child that was just spawned has a pid");
        let stdin = child.stdin.take().expect("stdin is piped");
        let stdout = BufReader::new(child.stdout.take().expect("stdout is piped"));
        let stderr = child.stderr.take().expect("stderr is piped");
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                log_stderr_line(pid, &line);
            }
        });
        // From here on, Drop kills the group and deletes the profile, should this future be cancelled.
        let mut instance = Self {
            child,
            pid,
            stdin: Some(stdin),
            stdout,
            profile,
            ready: None,
            spawn_to_ready: Duration::ZERO,
            state: State::Busy,
        };
        let line = match instance.next_line().await {
            Ok(line) => line,
            Err(status) => return Err(StartError::Exited(status)),
        };
        match Ready::parse(&line) {
            Ok(ready) => {
                instance.ready = Some(ready);
                instance.spawn_to_ready = started.elapsed();
                instance.state = State::Idle;
                Ok(instance)
            }
            Err(e) => {
                instance.reap().await;
                Err(StartError::NotReady(e))
            }
        }
    }

    /// The instance's process id, which is also its process group id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Its profile directory (deleted once it is gone).
    pub fn profile_dir(&self) -> &Path {
        &self.profile
    }

    /// Its ready line.
    pub fn ready(&self) -> Ready {
        self.ready.expect("a started instance has reported ready")
    }

    /// From spawning it to reading its ready line.
    pub fn spawn_to_ready(&self) -> Duration {
        self.spawn_to_ready
    }

    /// Whether it can take a conversion.
    pub fn is_usable(&self) -> bool {
        self.state == State::Idle
    }

    /// Converts `input` to a PDF at `output` (absolute paths). `Ok` is the instance's answer, including its
    /// typed failures; `Err` is a conversion that got no answer.
    pub async fn convert(&mut self, input: &Path, output: &Path) -> Result<Answer, ConvertError> {
        if self.state != State::Idle {
            return Err(ConvertError::Unusable);
        }
        let request = Request::Convert {
            input: input.to_path_buf(),
            output: output.to_path_buf(),
        }
        .encode()
        .map_err(ConvertError::Request)?;
        self.state = State::Busy;
        let stdin = self.stdin.as_mut().expect("an idle instance has its stdin");
        let sent = async {
            stdin.write_all(format!("{request}\n").as_bytes()).await?;
            stdin.flush().await
        }
        .await;
        if sent.is_err() {
            return Err(ConvertError::Dead(self.reap().await));
        }
        let line = self.next_line().await.map_err(ConvertError::Dead)?;
        match Answer::parse(&line) {
            Ok(answer) => {
                self.state = State::Idle;
                Ok(answer)
            }
            Err(e) => {
                self.reap().await;
                Err(ConvertError::BadAnswer(e))
            }
        }
    }

    /// `SIGKILL` to its whole process group, then waits for it and deletes its profile. The status is how it
    /// ended (`None` if it was already gone).
    pub async fn kill(mut self) -> Option<ExitStatus> {
        if self.state == State::Gone {
            return None;
        }
        self.reap().await
    }

    /// Closes its stdin, which ends it, waits for it and deletes its profile. Not bounded: a caller that cannot
    /// wait wraps it in a timeout, and a cancelled stop kills the group on drop.
    pub async fn stop(mut self) -> Option<ExitStatus> {
        if self.state == State::Gone {
            return None;
        }
        self.state = State::Busy;
        drop(self.stdin.take());
        let status = self.child.wait().await.ok();
        // Anything else left in its group goes too, while the group id still cannot be reused.
        kill_group(self.pid);
        remove_profile(&self.profile);
        self.state = State::Gone;
        status
    }

    /// The next line on its stdout. Its stdout closing or the process exiting, whichever comes first, is a
    /// death: the group is killed, the profile deleted, and the error is how it ended. The exit is watched as
    /// well as the pipe because anything left in its group would hold the pipe open.
    async fn next_line(&mut self) -> Result<String, Option<ExitStatus>> {
        let mut line = String::new();
        let exited = tokio::select! {
            read = self.stdout.read_line(&mut line) => match read {
                Ok(0) | Err(_) => None,
                Ok(_) => return Ok(line),
            },
            status = self.child.wait() => Some(status.ok()),
        };
        match exited {
            // Reaped already: kill whatever is left of its group (its id stays taken while members remain).
            Some(status) => {
                kill_group(self.pid);
                remove_profile(&self.profile);
                self.state = State::Gone;
                self.stdin = None;
                Err(status)
            }
            None => Err(self.reap().await),
        }
    }

    /// Kills the group (still unreaped, so its id cannot have been reused), reaps the process, deletes the
    /// profile.
    async fn reap(&mut self) -> Option<ExitStatus> {
        kill_group(self.pid);
        let status = self.child.wait().await.ok();
        // Again, for a member forked while the first signal was delivered (seen on macOS under load). The
        // group id cannot be reused while such a member lives.
        kill_group(self.pid);
        remove_profile(&self.profile);
        self.state = State::Gone;
        self.stdin = None;
        status
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if self.state != State::Gone {
            kill_group(self.pid);
            // The runtime reaps a dropped child in the background.
            remove_profile(&self.profile);
        }
    }
}

#[allow(
    unsafe_code,
    reason = "signals and process groups, which std has no form of"
)]
fn kill_group(pgid: u32) {
    let Ok(pgid) = i32::try_from(pgid) else {
        return;
    };
    // SAFETY: kill(2) with a negative pid signals that process group; ESRCH (already gone) is fine.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
}

fn remove_profile(profile: &Path) {
    match std::fs::remove_dir_all(profile) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            tracing::warn!(profile = %profile.display(), error = %e, "cannot delete an instance's profile");
        }
    }
}

#[cfg(test)]
mod tests {
    //! The driver against a stand-in instance: `sh -c <script> sh <profile dir>`. The script speaks the
    //! protocol; the real instance is tested in `tests/instance_lok.rs`.

    use std::os::unix::process::ExitStatusExt;

    use super::*;

    /// Answers `ok` to every request, `not_loaded` for an input path containing `corrupt`, and leaves a
    /// `sleep` in its process group, so a group kill can be told from a kill of the leader alone.
    const SERVES: &str = r#"
        test -d "$1" || exit 9
        echo started > "$1/marker"
        sleep 600 </dev/null >/dev/null 2>&1 &
        printf 'ready\t5\t%s\n' "$$"
        while IFS="$(printf '\t')" read -r cmd input output; do
            case "$input" in
                *corrupt*) printf 'not_loaded\tloadComponentFromURL returned an empty reference\n' ;;
                *slow*) sleep 600 ;;
                *garbage*) echo 'what?' ;;
                *exit*) exit 7 ;;
                *) printf 'ok\t1\t2\n' ;;
            esac
        done
        kill "$!" 2>/dev/null
    "#;

    fn launcher(script: &str, profiles: &Path) -> Launcher {
        Launcher::with_args("/bin/sh", ["-c", script, "sh"], profiles)
    }

    async fn start(profiles: &Path) -> Instance {
        Instance::start(&launcher(SERVES, profiles)).await.unwrap()
    }

    async fn group_gone(pgid: u32) -> bool {
        for _ in 0..100 {
            if !crate::instance::group::group_alive(pgid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    fn input(name: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/conversion/{name}.docx"))
    }

    fn output() -> PathBuf {
        PathBuf::from("/tmp/conversion/output.pdf")
    }

    #[tokio::test]
    #[allow(
        unsafe_code,
        reason = "signals and process groups, which std has no form of"
    )]
    async fn it_starts_in_its_own_group_with_its_own_profile() {
        let profiles = tempfile::tempdir().unwrap();
        let instance = start(profiles.path()).await;
        assert_eq!(instance.ready().pid, instance.pid());
        assert!(instance.profile_dir().starts_with(profiles.path()));
        assert!(instance.profile_dir().join("marker").exists());
        // SAFETY: getpgid reads another process's group id.
        let pgid = unsafe { libc::getpgid(instance.pid().cast_signed()) };
        assert_eq!(pgid, instance.pid().cast_signed());
        // SAFETY: as above, for this process.
        assert_ne!(pgid, unsafe { libc::getpgid(0) });
        instance.kill().await;
    }

    #[tokio::test]
    async fn it_answers_conversions_with_typed_answers_and_stays_usable() {
        let profiles = tempfile::tempdir().unwrap();
        let mut instance = start(profiles.path()).await;
        let answer = instance.convert(&input("good"), &output()).await.unwrap();
        assert_eq!(
            answer,
            Answer::Converted {
                load: Duration::from_millis(1),
                export: Duration::from_millis(2)
            }
        );
        let answer = instance
            .convert(&input("corrupt"), &output())
            .await
            .unwrap();
        assert!(matches!(answer, Answer::NotLoaded(_)), "{answer:?}");
        assert!(instance.is_usable());
        let answer = instance.convert(&input("good"), &output()).await.unwrap();
        assert!(matches!(answer, Answer::Converted { .. }));
        instance.kill().await;
    }

    #[tokio::test]
    async fn a_kill_takes_the_whole_group_and_deletes_the_profile() {
        let profiles = tempfile::tempdir().unwrap();
        let instance = start(profiles.path()).await;
        let (pid, profile) = (instance.pid(), instance.profile_dir().to_path_buf());
        let status = instance.kill().await.unwrap();
        assert_eq!(status.signal(), Some(libc::SIGKILL));
        assert!(group_gone(pid).await, "something of group {pid} is left");
        assert!(!profile.exists());
    }

    #[tokio::test]
    async fn a_kill_from_outside_mid_conversion_is_a_dead_instance() {
        let profiles = tempfile::tempdir().unwrap();
        let mut instance = start(profiles.path()).await;
        let (pid, profile) = (instance.pid(), instance.profile_dir().to_path_buf());
        let killer = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            kill_group(pid);
        });
        let died = instance.convert(&input("slow"), &output()).await;
        killer.await.unwrap();
        match died {
            Err(ConvertError::Dead(Some(status))) => {
                assert_eq!(status.signal(), Some(libc::SIGKILL));
            }
            other => panic!("expected a dead instance, got {other:?}"),
        }
        assert!(!instance.is_usable());
        assert!(group_gone(pid).await);
        assert!(!profile.exists());
        assert!(matches!(
            instance.convert(&input("good"), &output()).await,
            Err(ConvertError::Unusable)
        ));
    }

    #[tokio::test]
    async fn an_exit_during_a_conversion_is_a_dead_instance() {
        let profiles = tempfile::tempdir().unwrap();
        let mut instance = start(profiles.path()).await;
        let (pid, profile) = (instance.pid(), instance.profile_dir().to_path_buf());
        match instance.convert(&input("exit"), &output()).await {
            Err(ConvertError::Dead(Some(status))) => assert_eq!(status.code(), Some(7)),
            other => panic!("expected a dead instance, got {other:?}"),
        }
        assert!(group_gone(pid).await, "the sleeper of group {pid} is left");
        assert!(!profile.exists());
    }

    #[tokio::test]
    async fn an_exit_is_noticed_while_something_in_its_group_holds_the_pipe() {
        let profiles = tempfile::tempdir().unwrap();
        let holds_the_pipe = r#"
            sleep 600 &
            printf 'ready\t5\t%s\n' "$$"
            read -r request
            exit 5
        "#;
        let mut instance = Instance::start(&launcher(holds_the_pipe, profiles.path()))
            .await
            .unwrap();
        let (pid, profile) = (instance.pid(), instance.profile_dir().to_path_buf());
        let died = tokio::time::timeout(
            Duration::from_secs(10),
            instance.convert(&input("good"), &output()),
        )
        .await
        .expect("the exit was not noticed");
        match died {
            Err(ConvertError::Dead(Some(status))) => assert_eq!(status.code(), Some(5)),
            other => panic!("expected a dead instance, got {other:?}"),
        }
        assert!(group_gone(pid).await, "the sleeper of group {pid} is left");
        assert!(!profile.exists());
    }

    #[tokio::test]
    async fn a_garbled_answer_kills_the_instance() {
        let profiles = tempfile::tempdir().unwrap();
        let mut instance = start(profiles.path()).await;
        let (pid, profile) = (instance.pid(), instance.profile_dir().to_path_buf());
        let answer = instance.convert(&input("garbage"), &output()).await;
        assert!(
            matches!(answer, Err(ConvertError::BadAnswer(_))),
            "{answer:?}"
        );
        assert!(group_gone(pid).await);
        assert!(!profile.exists());
    }

    #[tokio::test]
    async fn an_interrupted_conversion_leaves_it_unusable() {
        let profiles = tempfile::tempdir().unwrap();
        let mut instance = start(profiles.path()).await;
        let timed_out = tokio::time::timeout(
            Duration::from_millis(100),
            instance.convert(&input("slow"), &output()),
        )
        .await;
        assert!(timed_out.is_err());
        assert!(!instance.is_usable());
        assert!(matches!(
            instance.convert(&input("good"), &output()).await,
            Err(ConvertError::Unusable)
        ));
        instance.kill().await;
    }

    #[tokio::test]
    async fn a_path_that_cannot_travel_leaves_it_usable() {
        let profiles = tempfile::tempdir().unwrap();
        let mut instance = start(profiles.path()).await;
        let refused = instance
            .convert(Path::new("/tmp/a\nb.docx"), &output())
            .await;
        assert!(matches!(refused, Err(ConvertError::Request(_))));
        assert!(instance.is_usable());
        instance.kill().await;
    }

    #[tokio::test]
    async fn stop_closes_stdin_and_leaves_nothing() {
        let profiles = tempfile::tempdir().unwrap();
        let instance = start(profiles.path()).await;
        let (pid, profile) = (instance.pid(), instance.profile_dir().to_path_buf());
        let status = instance.stop().await.unwrap();
        assert!(status.success(), "{status}");
        assert!(group_gone(pid).await);
        assert!(!profile.exists());
    }

    #[tokio::test]
    async fn dropping_it_kills_the_group_and_deletes_the_profile() {
        let profiles = tempfile::tempdir().unwrap();
        let instance = start(profiles.path()).await;
        let (pid, profile) = (instance.pid(), instance.profile_dir().to_path_buf());
        drop(instance);
        assert!(group_gone(pid).await);
        assert!(!profile.exists());
    }

    #[tokio::test]
    async fn an_exit_before_the_ready_line_is_a_failed_start() {
        let profiles = tempfile::tempdir().unwrap();
        let failed = Instance::start(&launcher("exit 3", profiles.path())).await;
        match failed {
            Err(StartError::Exited(Some(status))) => assert_eq!(status.code(), Some(3)),
            other => panic!("expected a failed start, got {other:?}"),
        }
        assert_eq!(std::fs::read_dir(profiles.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_first_line_that_is_not_ready_is_a_failed_start() {
        let profiles = tempfile::tempdir().unwrap();
        let failed = Instance::start(&launcher("echo hello; sleep 600", profiles.path())).await;
        assert!(matches!(failed, Err(StartError::NotReady(_))), "{failed:?}");
        assert_eq!(std::fs::read_dir(profiles.path()).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn a_program_that_cannot_start_leaves_no_profile() {
        let profiles = tempfile::tempdir().unwrap();
        let failed =
            Instance::start(&Launcher::new("/nonexistent/docx-to-pdf", profiles.path())).await;
        assert!(matches!(failed, Err(StartError::Spawn(_))), "{failed:?}");
        assert_eq!(std::fs::read_dir(profiles.path()).unwrap().count(), 0);
    }
}
