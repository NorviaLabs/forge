//! A host from the user's SSH config, and a folder on it.
//!
//! Forge runs the system `ssh` client, so `~/.ssh/config` applies in full
//! (`ProxyJump`, identities, the agent, `known_hosts`), over one multiplexed
//! connection per session (a `ControlMaster` whose socket is in the session's
//! private local directory). Some options are always forced, whatever the
//! config says: no prompts (`BatchMode`), a bounded connect and keepalive so
//! a dead link is an error rather than a hang, and no agent, X11 or port
//! forwarding. `ssh` itself starts with an almost empty environment, so
//! whatever `SendEnv` the config names, nothing of Forge's environment is
//! there to send.
//!
//! Connecting runs a preflight: the host is Linux or macOS with `bash`,
//! `realpath` and `mktemp`, its login shell reads POSIX quoting (every
//! command and path is sent quoted, see [`super::quote`]), the folder exists
//! and is a directory (its canonical path becomes the *remote root*), and a
//! private temp directory is made for the session (the *remote temp*, the
//! commands' `$TMPDIR`, removed on close).
//!
//! A command runs as `bash -c <wrapper>` over the connection. `sshd` starts
//! every session in a session of its own, so the wrapper leads a process
//! group holding everything the command starts. The wrapper records that
//! group and the command's exit status in the remote temp: the first so a
//! cancel can kill the whole group there, the second so an exit status of
//! 255 is told apart from a lost connection.

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::helper;
use super::quote::{quote, quote_argv};
use super::{
    Access, DirEntry, FileMeta, GrepHit, RemoteRun, RunOutput, Target, TargetPath, TargetShell,
};
use crate::registry::ToolContext;
use crate::shell_spawn::{ShellSpawnMode, ShellSpawnRequest, ShellSpawnSpec};
use crate::ToolError;

/// Connection settings. The defaults are what the app uses; tests shorten
/// the keepalive and name their own config file.
#[derive(Debug, Clone)]
pub struct SshOptions {
    /// `ssh -F <file>`; `None` is the user's own config.
    pub config_file: Option<PathBuf>,
    pub connect_timeout: Duration,
    /// `ServerAliveInterval`; with `keepalive_count` it bounds how long a
    /// dead connection can go unnoticed.
    pub keepalive_interval: Duration,
    pub keepalive_count: u32,
    /// The `ssh` program.
    pub program: PathBuf,
    /// How long a file helper beyond the first stays open unused: each is
    /// an SSH session, and a host allows only so many (`MaxSessions`).
    pub helper_idle: Duration,
}

impl Default for SshOptions {
    fn default() -> Self {
        Self {
            config_file: None,
            connect_timeout: Duration::from_secs(10),
            keepalive_interval: Duration::from_secs(15),
            keepalive_count: 3,
            program: PathBuf::from("ssh"),
            helper_idle: Duration::from_secs(10),
        }
    }
}

/// The variables the local `ssh` keeps: what it needs to find the user's
/// config, keys and agent, and the locale. Nothing else of Forge's
/// environment (provider keys, tokens) is in its environment to be sent.
const KEPT_ENV: &[&str] = &[
    "HOME",
    "USER",
    "LOGNAME",
    "PATH",
    "SHELL",
    "SSH_AUTH_SOCK",
    "KRB5CCNAME",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
];

/// How long an idle master connection outlives its last command. A Forge
/// that quits without closing the session leaves nothing running for longer.
const CONTROL_PERSIST_SECS: u64 = 600;

/// Output of Forge's own helper commands (file reads included) is capped
/// here, so a runaway host cannot exhaust memory.
const MAX_HELPER_OUTPUT: usize = 64 * 1024 * 1024;

/// How long one file operation may take.
const FS_TIMEOUT: Duration = Duration::from_secs(120);

/// Exit status the file helpers use for "nothing is there".
const NOT_FOUND: i32 = 3;

/// Checks the host and makes the remote temp. Fields are NUL-terminated
/// `key=value` records after a marker carrying this connection's one-time
/// token (`$3`), so output a shell startup file prints first, forged or
/// not, is not taken for the answer.
const PREFLIGHT: &str = r#"p=$1
printf 'FORGE-PREFLIGHT-%s\0probe=%s\0os=%s\0' "$3" "$2" "$(uname -s 2>/dev/null)"
m=
for t in realpath mktemp uname cat rm mkdir head tail dirname stat kill sleep; do
  command -v "$t" >/dev/null 2>&1 || m="$m $t"
done
if [ -n "$m" ]; then printf 'missing=%s\0' "${m# }"; exit 0; fi
if [ ! -e "$p" ]; then printf 'error=missing\0'; exit 0; fi
if [ ! -d "$p" ]; then printf 'error=notdir\0'; exit 0; fi
r=$(realpath -- "$p") || { printf 'error=realpath\0'; exit 0; }
t=$(mktemp -d "${TMPDIR:-/tmp}/forge-session.XXXXXXXX") || { printf 'error=mktemp\0'; exit 0; }
chmod 700 "$t"
printf 'root=%s\0temp=%s\0' "$r" "$t"
"#;

/// Runs one command: `$1` root, `$2` temp, `$3` run id, `$4` shell, `$5`
/// command. `$$` leads the process group `sshd` made for this session.
const RUN: &str = r#"t=$2
mkdir -p -- "$t" 2>/dev/null
printf '%s' "$$" > "$t/$3.pid" 2>/dev/null
cd -- "$1" 2>/dev/null || { printf 'forge: %s is not available on this host\n' "$1" >&2; exit 1; }
export TMPDIR="$t"
"$4" -c "$5"
s=$?
printf '%s' "$s" > "$t/$3.status" 2>/dev/null
rm -f -- "$t/$3.pid"
exit "$s"
"#;

/// Kills a run's process group: `$1` temp, `$2` run id. Waits briefly for a
/// run that has not written its pid yet.
const CANCEL: &str = r#"f=$1/$2.pid
i=0
while [ ! -s "$f" ] && [ ! -e "$1/$2.status" ] && [ "$i" -lt 20 ]; do sleep 0.1; i=$((i+1)); done
[ -s "$f" ] || exit 0
p=$(cat -- "$f")
kill -TERM -- "-$p" 2>/dev/null || exit 0
sleep 1
kill -KILL -- "-$p" 2>/dev/null
rm -f -- "$f"
"#;

/// The file operations. Run one-shot (`$1` is the operation), or as the
/// session's helper (`--serve <temp> <nonce>`): a loop reading requests in
/// [`super::helper`]'s format and running each in a subshell.
const FS: &str = r#"nl=$'\n'
# What is at `$1`, as `meta` prints it.
meta() {
  if [ ! -e "$1" ]; then printf 'none'
  elif [ -d "$1" ]; then printf 'dir 0'
  else printf 'file %s' "$(stat -c %s -- "$1" 2>/dev/null || stat -f %z -- "$1")"; fi
}
# `$1`'s folder, exactly (`$(...)` would drop trailing line breaks).
parent() { p=$(dirname -- "$1"; printf x); p=${p%x}; printf '%s' "${p%"$nl"}"; }
forge_fs() {
op=$1; shift
case $op in
resolve)
  # `$(...)` drops trailing line breaks, which can end a real name: each
  # answer is read up to a sentinel, then exactly one break is removed.
  f=$1; a=$f
  while [ ! -e "$a" ]; do
    if [ -L "$a" ]; then printf 'broken\0'; exit 0; fi
    n=$(parent "$a"; printf x); n=${n%x}
    [ "$n" = "$a" ] && break; a=$n
  done
  c=$(realpath -- "$a" 2>/dev/null && printf x)
  case $c in *x) ;; *) printf 'broken\0'; exit 0 ;; esac
  c=${c%x}; c=${c%"$nl"}
  # What is there comes along, so the tool need not ask again.
  if [ "$a" = "$f" ]; then printf 'exists\0%s\0%s\0' "$c" "$(meta "$f")"; else printf 'new\0%s\0' "$c"; fi ;;
meta)
  meta "$1" ;;
read)
  [ -e "$1" ] || exit 3
  if [ "$2" = all ]; then cat -- "$1"; else head -c "$2" -- "$1"; fi ;;
lines)
  [ -e "$1" ] || exit 3
  if [ "$3" = all ]; then tail -n "+$2" -- "$1"; else tail -n "+$2" -- "$1" | head -n "$3"; fi ;;
headlines)
  # The first `$2` bytes in hex, a NUL, then the lines (`lines` above).
  [ -e "$1" ] || exit 3
  head -c "$2" -- "$1" | od -An -v -tx1 || exit 1
  printf '\0'
  if [ "$4" = all ]; then tail -n "+$3" -- "$1"; else tail -n "+$3" -- "$1" | head -n "$4"; fi ;;
write)
  mkdir -p -- "$(parent "$1")" && cat > "$1" ;;
swap)
  # The old content on stdout (exit 4: there was none), then the new one
  # from stdin in its place.
  e=4
  if [ -f "$1" ]; then cat -- "$1" || exit 1; e=0; fi
  mkdir -p -- "$(parent "$1")" && cat > "$1" || exit 1
  exit "$e" ;;
remove)
  [ -e "$1" ] || [ -L "$1" ] || exit 3
  rm -- "$1" ;;
list)
  [ -d "$1" ] || exit 3
  shopt -s dotglob nullglob
  for e in "$1"/*; do
    n=${e##*/}
    if [ -d "$e" ] && [ ! -L "$e" ]; then printf 'd%s\0' "$n"; else printf 'f%s\0' "$n"; fi
  done ;;
find)
  # `$2` the folder to start in, `$3` a file-name pattern, `$4` a word the
  # path contains (either may be empty): narrowed here, matched exactly by
  # Forge.
  cd -- "$1" || exit 1
  [ -d "$2" ] || exit 0
  if command -v rg >/dev/null 2>&1; then
    a=(rg --no-config --files --hidden --glob '!.git' --null)
    [ -n "$3" ] && a+=(--glob "$3")
    [ -n "$4" ] && a+=(--iglob "*$4*" --iglob "*$4*/**")
    "${a[@]}" -- "$2" 2>/dev/null
  else
    a=(find "$2" -name .git -prune -o -type f)
    [ -n "$3" ] && a+=(-name "$3")
    [ -n "$4" ] && a+=(-ipath "*$4*")
    "${a[@]}" -print0 2>/dev/null
  fi | head -c 16777216 ;;
grep)
  cd -- "$1" || exit 1
  mode=$2; pat=$3; inc=$4; where=$5; max=$6
  if command -v rg >/dev/null 2>&1; then
    a=(rg --no-config -n --no-heading --with-filename --null --color never --hidden --glob '!.git' --max-columns 500)
    [ "$mode" = literal ] && a+=(-F)
    [ -n "$inc" ] && a+=(--glob "$inc")
  else
    a=(grep -r -n -I --null --exclude-dir=.git)
    if [ "$mode" = literal ]; then a+=(-F); else a+=(-E); fi
    [ -n "$inc" ] && a+=("--include=$inc")
  fi
  a+=(-e "$pat" -- "$where")
  e=$(mktemp) || exit 1
  "${a[@]}" 2>"$e" | head -n "$max"
  s=${PIPESTATUS[0]}
  case $s in 0|1|141) rm -f -- "$e"; exit 0 ;; esac
  cat -- "$e" >&2; rm -f -- "$e"; exit 2 ;;
*) exit 64 ;;
esac
}
# The helper: `$1` the session's remote temp, `$2` this helper's nonce,
# `$3` the most output it keeps for a reply (Forge refuses more).
serve() {
  t=$1; nonce=$2; cap=$3
  case $cap in ''|*[!0-9]*) exit 1 ;; esac
  had_lc=${LC_ALL+1}; old_lc=${LC_ALL-}
  export LC_ALL=C
  dec=
  for flag in -d --decode -D; do
    if [ "$(printf eA== | base64 "$flag" 2>/dev/null)" = x ]; then dec=$flag; break; fi
  done
  if [ -z "$dec" ]; then printf 'FORGE-HELPER-%s unavailable base64\n' "$nonce"; exit 0; fi
  # An argument is read by its length: `read -N` from bash 4.1, `dd` before
  # it (macOS ships bash 3.2).
  exact=
  if [ "${BASH_VERSINFO[0]}" -gt 4 ] || { [ "${BASH_VERSINFO[0]}" -eq 4 ] && [ "${BASH_VERSINFO[1]}" -ge 1 ]; }; then exact=1; fi
  mkdir -p -- "$t" || exit 1
  in=$t/helper.$$.in; out=$t/helper.$$.out; err=$t/helper.$$.err; head=$t/helper.$$.head
  trap 'rm -f -- "$in" "$out" "$err" "$head"' EXIT
  # Its process id leads its process group: what Forge stops if it abandons
  # a request.
  printf 'FORGE-HELPER-%s ready %s\n' "$nonce" "$$"
  while IFS=' ' read -r tag argc input; do
    [ "$tag" = F ] || exit 1
    case $argc in ''|*[!0-9]*) exit 1 ;; esac
    args=()
    i=0
    while [ "$i" -lt "$argc" ]; do
      IFS= read -r n || exit 1
      case $n in ''|*[!0-9]*) exit 1 ;; esac
      a=
      if [ "$n" -gt 0 ]; then
        if [ -n "$exact" ]; then IFS= read -r -N "$n" a || exit 1
        else
          # `read` takes stdin a byte at a time, so `dd` goes on from where
          # it stopped; the `x` keeps trailing line breaks.
          a=$(dd bs=1 count="$n" 2>/dev/null; printf x); a=${a%x}
          [ "${#a}" -eq "$n" ] || exit 1
        fi
      fi
      IFS= read -r rest || exit 1
      args+=("$a"); i=$((i + 1))
    done
    src=/dev/null
    if [ "$input" = 1 ]; then
      IFS= read -r b64 || exit 1
      printf '%s' "$b64" | base64 "$dec" > "$in" || exit 1
      src=$in
    fi
    # Output beyond the cap is not kept: Forge refuses it anyway, and the
    # host's temp must not fill up with it.
    (
      if [ -n "$had_lc" ]; then export LC_ALL=$old_lc; else unset LC_ALL; fi
      forge_fs "${args[@]}"
    ) < "$src" 2> "$err" | head -c "$cap" > "$out"
    s=${PIPESTATUS[0]}
    # One program for both sizes, and one to send the reply: each process
    # started costs more than the operation itself.
    { read -r o _; read -r e _; } < <(wc -c -- "$out" "$err")
    printf 'R %s %s %s\n' "$s" "$o" "$e" > "$head"
    cat -- "$head" "$out" "$err"
  done
}
if [ "$1" = --serve ]; then serve "$2" "$3" "$4"; else forge_fs "$@"; fi
"#;

/// Read the preflight's answer. `nonce` is the one-time token this
/// connection sent; only a marker carrying it is Forge's own.
fn parse_preflight(
    host: &str,
    path: &str,
    probe: &str,
    nonce: &str,
    out: &RunOutput,
) -> Result<Remote, ToolError> {
    let marker = format!("FORGE-PREFLIGHT-{nonce}\0");
    // The last one: anything printed before Forge's script ran (a shell
    // startup file) comes first.
    let Some(start) = rfind(&out.stdout, marker.as_bytes()) else {
        let stderr = String::from_utf8_lossy(&out.stderr);
        if out.status == Some(127) || stderr.contains("bash") {
            return Err(ToolError::Execution(format!(
                "{host} has no bash, which Forge needs to run commands there"
            )));
        }
        return Err(ToolError::Execution(format!(
            "Could not check {host}: {}",
            stderr.trim()
        )));
    };
    let body = &out.stdout[start + marker.len()..];
    let fields: Vec<(String, String)> = body
        .split(|byte| *byte == 0)
        .filter_map(|field| {
            let field = String::from_utf8_lossy(field);
            let (key, value) = field.split_once('=')?;
            Some((key.to_string(), value.to_string()))
        })
        .collect();
    let field = |name: &str| {
        fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    };
    if field("probe") != Some(probe) {
        return Err(ToolError::Execution(format!(
            "The login shell on {host} does not read POSIX shell quoting (fish or csh?), so \
             Forge cannot send it commands safely. Use bash, zsh or sh as the login shell \
             for this account."
        )));
    }
    match field("os") {
        Some("Linux") | Some("Darwin") => {}
        other => {
            return Err(ToolError::Execution(format!(
                "{host} runs {}; Forge runs sessions on Linux and macOS hosts",
                other
                    .filter(|os| !os.is_empty())
                    .unwrap_or("an unknown system")
            )))
        }
    }
    if let Some(missing) = field("missing") {
        return Err(ToolError::Execution(format!(
            "{host} is missing {}, which Forge needs to run commands there",
            missing.split(' ').collect::<Vec<_>>().join(", ")
        )));
    }
    match field("error") {
        Some("missing") => {
            return Err(ToolError::Execution(format!(
                "{path} does not exist on {host}"
            )))
        }
        Some("notdir") => {
            return Err(ToolError::Execution(format!(
                "{path} on {host} is not a folder"
            )))
        }
        Some(other) => {
            return Err(ToolError::Execution(format!(
                "Could not prepare {path} on {host} ({other} failed)"
            )))
        }
        None => {}
    }
    match (field("root"), field("temp")) {
        (Some(root), Some(temp)) if is_plain_absolute(root) && is_session_temp(temp) => {
            Ok(Remote {
                root: root.to_string(),
                temp: temp.to_string(),
            })
        }
        _ => Err(ToolError::Execution(format!(
            "Could not check {host}: its answer was incomplete"
        ))),
    }
}

/// A fresh private directory for one target's control socket:
/// `<temp>/forge-ssh-<uid>/<random>`, short enough for a socket path (about
/// 100 bytes at most). The per-user parent is checked to be this user's own,
/// private directory, so another user on the machine cannot own, or see
/// into, the place the socket is made.
pub fn private_control_dir() -> std::io::Result<PathBuf> {
    let probe = tempfile::tempfile()?;
    #[cfg(unix)]
    let uid = {
        use std::os::unix::fs::MetadataExt;
        probe.metadata()?.uid()
    };
    #[cfg(not(unix))]
    let uid = 0u32;
    drop(probe);
    let base = std::env::temp_dir().join(format!("forge-ssh-{uid}"));
    match std::fs::create_dir(&base) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = std::fs::symlink_metadata(&base)?;
        if !meta.is_dir() || meta.uid() != uid {
            return Err(std::io::Error::other(format!(
                "{} is not this user's directory; refusing to put an SSH socket there",
                base.display()
            )));
        }
        if meta.mode() & 0o077 != 0 {
            std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    Ok(tempfile::Builder::new()
        .prefix("s")
        .rand_bytes(8)
        .tempdir_in(&base)?
        .keep())
}

/// What the preflight found.
#[derive(Debug, Clone)]
struct Remote {
    root: String,
    temp: String,
}

#[derive(Debug)]
struct Shared {
    host: String,
    path: String,
    local_dir: PathBuf,
    control: PathBuf,
    options: SshOptions,
    remote: Mutex<Option<Remote>>,
    connecting: tokio::sync::Mutex<()>,
    next_run: AtomicU64,
    /// Long-lived file helpers on the host (see [`super::helper`]).
    helpers: helper::HelperPool,
    /// Itself, for work that outlives a call (stopping an abandoned helper,
    /// closing idle ones).
    me: std::sync::Weak<Shared>,
}

/// One remote command's identity on the host, for [`super::RemoteRun`].
#[derive(Debug)]
pub(crate) struct RunHandle {
    shared: Arc<Shared>,
    temp: String,
    pub(crate) id: String,
    /// What the command was, for the lost-connection message.
    command: String,
}

#[derive(Debug)]
pub struct SshTarget {
    shared: Arc<Shared>,
}

impl SshTarget {
    /// A target for `host` (an SSH config alias) and `path` (absolute, on
    /// the host). `local_dir` is a private local directory for the control
    /// socket; keep its path short (a socket path is limited to about 100
    /// bytes). Nothing connects until [`Self::connect`] or the first use.
    pub fn new(host: &str, path: &str, local_dir: &Path, options: SshOptions) -> Self {
        Self {
            shared: Arc::new_cyclic(|me| Shared {
                me: me.clone(),
                host: host.to_string(),
                path: path.to_string(),
                local_dir: local_dir.to_path_buf(),
                control: local_dir.join("cm"),
                options,
                remote: Mutex::new(None),
                connecting: tokio::sync::Mutex::new(()),
                next_run: AtomicU64::new(0),
                helpers: helper::HelperPool::default(),
            }),
        }
    }

    /// Open the connection and run the preflight. Idempotent; after a lost
    /// connection it reconnects.
    pub async fn connect(&self) -> Result<(), ToolError> {
        self.shared.ensure_connected().await.map(drop)
    }

    /// The canonical remote root, once connected.
    pub fn remote_root(&self) -> Option<String> {
        self.shared.known().map(|remote| remote.root)
    }

    /// The session's private remote temp directory, once connected.
    pub fn remote_temp(&self) -> Option<String> {
        self.shared.known().map(|remote| remote.temp)
    }

    /// The options every `ssh` invocation carries, before the host.
    pub fn forced_options(&self) -> Vec<String> {
        self.shared.client_options()
    }

    /// `ssh -G` with the forced options: the configuration `ssh` would
    /// actually use for this host, as `key value` lines.
    pub async fn effective_config(&self) -> Result<String, ToolError> {
        let mut command = self.shared.command();
        command.args(self.shared.client_options());
        command.args(["-G", "--", &self.shared.host]);
        let out = command
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|error| self.shared.spawn_error(error))?;
        if !out.status.success() {
            return Err(ToolError::Execution(format!(
                "ssh -G {}: {}",
                self.shared.host,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Remove the remote temp and stop the multiplexed connection.
    pub async fn close(&self) {
        // Their input closes; each ends on the host.
        self.shared.helpers.clear();
        if let Some(remote) = self.shared.known() {
            if self.shared.master_alive().await {
                let _ = self
                    .shared
                    .client(
                        &format!("rm -rf -- {}", quote(&remote.temp)),
                        None,
                        Duration::from_secs(15),
                    )
                    .await;
            }
        }
        self.shared.stop_master();
        *self.shared.remote.lock().expect("ssh state lock") = None;
        // Only what this target made: its socket and log, then the directory
        // if that leaves it empty.
        let _ = std::fs::remove_file(&self.shared.control);
        let _ = std::fs::remove_file(self.shared.local_dir.join("connect.log"));
        let _ = std::fs::remove_dir(&self.shared.local_dir);
    }
}

impl Shared {
    fn known(&self) -> Option<Remote> {
        self.remote.lock().expect("ssh state lock").clone()
    }

    fn spawn_error(&self, error: std::io::Error) -> ToolError {
        if error.kind() == std::io::ErrorKind::NotFound {
            ToolError::Execution(format!(
                "Could not connect to {}: no `ssh` client is installed on this machine",
                self.host
            ))
        } else {
            ToolError::Execution(format!("Could not start ssh for {}: {error}", self.host))
        }
    }

    /// `ssh` with the user's config (`-F` already given) and an environment
    /// of [`KEPT_ENV`].
    fn command(&self) -> tokio::process::Command {
        let mut command = tokio::process::Command::new(&self.options.program);
        command.env_clear();
        for (name, value) in self.kept_env() {
            command.env(name, value);
        }
        command.args(self.config_args());
        command.kill_on_drop(true);
        command
    }

    /// `-F <file>` when a config file is named; every invocation starts so.
    fn config_args(&self) -> Vec<String> {
        match &self.options.config_file {
            Some(config) => vec!["-F".into(), config.display().to_string()],
            None => Vec::new(),
        }
    }

    fn kept_env(&self) -> Vec<(String, OsString)> {
        KEPT_ENV
            .iter()
            .filter_map(|name| std::env::var_os(name).map(|value| (name.to_string(), value)))
            .collect()
    }

    /// Everything but the multiplexing role.
    fn common_options(&self) -> Vec<String> {
        let seconds = |duration: Duration| duration.as_secs().max(1);
        [
            "BatchMode=yes".to_string(),
            format!("ConnectTimeout={}", seconds(self.options.connect_timeout)),
            format!(
                "ServerAliveInterval={}",
                seconds(self.options.keepalive_interval)
            ),
            format!(
                "ServerAliveCountMax={}",
                self.options.keepalive_count.max(1)
            ),
            "ForwardAgent=no".into(),
            "ForwardX11=no".into(),
            "ClearAllForwardings=yes".into(),
            "PermitLocalCommand=no".into(),
            "EscapeChar=none".into(),
        ]
        .into_iter()
        .flat_map(|option| ["-o".to_string(), option])
        .collect()
    }

    fn control_path(&self) -> String {
        format!("ControlPath={}", self.control.display())
    }

    /// What every command carries: it uses the session's master connection.
    /// `ssh` itself stays quiet: when the host refuses another session on
    /// the connection, it makes a connection of its own for the command, and
    /// its complaint about that must not end up in the command's output.
    fn client_options(&self) -> Vec<String> {
        let mut options = vec![
            "-o".to_string(),
            "ControlMaster=no".into(),
            "-o".into(),
            self.control_path(),
            "-o".into(),
            "LogLevel=QUIET".into(),
        ];
        options.extend(self.common_options());
        options
    }

    /// Arguments for a command over the master, `-F` included.
    fn args_for(&self, extra: &[&str], remote_command: &str) -> Vec<String> {
        let mut args = self.config_args();
        args.extend(self.client_options());
        args.extend(extra.iter().map(|arg| arg.to_string()));
        args.push("--".into());
        args.push(self.host.clone());
        args.push(remote_command.to_string());
        args
    }

    async fn master_alive(&self) -> bool {
        if !self.control.exists() {
            return false;
        }
        let mut command = self.command();
        command.args(["-o", &self.control_path(), "-O", "check", "--", &self.host]);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        matches!(
            tokio::time::timeout(Duration::from_secs(3), command.status()).await,
            Ok(Ok(status)) if status.success()
        )
    }

    /// Stop the master connection. Synchronous: also used from `Drop`.
    fn stop_master(&self) {
        if !self.control.exists() {
            return;
        }
        let mut command = std::process::Command::new(&self.options.program);
        command.env_clear();
        for (name, value) in self.kept_env() {
            command.env(name, value);
        }
        command.args(self.config_args());
        let _ = command
            .args(["-o", &self.control_path(), "-O", "exit", "--", &self.host])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    async fn ensure_connected(&self) -> Result<Remote, ToolError> {
        let _connecting = self.connecting.lock().await;
        let known = self.known();
        if let Some(known) = known.as_ref().filter(|_| self.control.exists()) {
            return Ok(known.clone());
        }
        if !self.path.starts_with('/') {
            return Err(ToolError::Execution(format!(
                "The remote folder must be an absolute path on {}, like /srv/app (got `{}`)",
                self.host, self.path
            )));
        }
        self.start_master().await?;
        if let Some(known) = known {
            return Ok(known);
        }
        let remote = self.preflight().await?;
        *self.remote.lock().expect("ssh state lock") = Some(remote.clone());
        Ok(remote)
    }

    async fn start_master(&self) -> Result<(), ToolError> {
        // Helpers ran over the old connection.
        self.helpers.clear();
        make_private_dir(&self.local_dir)?;
        // A socket left by a master that died without cleaning up.
        let _ = std::fs::remove_file(&self.control);
        let log = self.local_dir.join("connect.log");
        let log_file = std::fs::File::create(&log)?;
        let mut command = self.command();
        command
            .args(["-o", "ControlMaster=yes", "-o", &self.control_path()])
            .args([
                "-o".to_string(),
                format!("ControlPersist={CONTROL_PERSIST_SECS}"),
            ])
            .args(self.common_options())
            .args(["-o", "LogLevel=ERROR"])
            .args(["-N", "-f", "--", &self.host])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::from(log_file))
            // The master outlives this command on purpose.
            .kill_on_drop(false);
        // `ProxyJump` chains can take a while; the connect timeout applies
        // per hop.
        let bound = self.options.connect_timeout * 3 + Duration::from_secs(10);
        let status = match tokio::time::timeout(bound, command.status()).await {
            Ok(status) => status.map_err(|error| self.spawn_error(error))?,
            Err(_) => {
                return Err(ToolError::Execution(format!(
                    "Could not connect to {}: the connection timed out",
                    self.host
                )))
            }
        };
        let stderr = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_file(&log);
        if !status.success() {
            return Err(ToolError::Execution(connect_failure(&self.host, &stderr)));
        }
        for _ in 0..100 {
            if self.control.exists() {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        Err(ToolError::Execution(format!(
            "Could not connect to {}: the connection did not come up ({})",
            self.host,
            stderr.trim()
        )))
    }

    async fn preflight(&self) -> Result<Remote, ToolError> {
        // A backslash pair and a single quote: a login shell that does not
        // read POSIX quoting (fish, csh) changes one of them.
        let probe = "a'b\\\\c\"$d `e` f";
        let nonce = one_time_token();
        let command = format!(
            "exec bash -c {} forge-preflight {} {} {}",
            quote(PREFLIGHT),
            quote(&self.path),
            quote(probe),
            quote(&nonce)
        );
        let out = self.client(&command, None, Duration::from_secs(60)).await?;
        parse_preflight(&self.host, &self.path, probe, &nonce, &out)
    }

    /// Run one of Forge's own commands over the master connection.
    async fn client(
        &self,
        remote_command: &str,
        stdin: Option<&[u8]>,
        timeout: Duration,
    ) -> Result<RunOutput, ToolError> {
        let mut command = self.command();
        let mut extra = vec!["-T"];
        if stdin.is_none() {
            extra.push("-n");
        }
        let mut args = self.client_options();
        args.extend(extra.iter().map(|arg| arg.to_string()));
        args.extend([
            "--".to_string(),
            self.host.clone(),
            remote_command.to_string(),
        ]);
        command.args(args);
        command
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().map_err(|error| self.spawn_error(error))?;
        let input = stdin.map(<[u8]>::to_vec);
        let mut child_stdin = child.stdin.take();
        let mut stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        let run = async {
            let feed = async {
                if let (Some(mut pipe), Some(bytes)) = (child_stdin.take(), input) {
                    let _ = pipe.write_all(&bytes).await;
                    let _ = pipe.shutdown().await;
                }
            };
            let read_out = read_capped(&mut stdout);
            let read_err = read_capped(&mut stderr);
            let ((), out, err) = tokio::join!(feed, read_out, read_err);
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((status, out?, err?))
        };
        match tokio::time::timeout(timeout, run).await {
            Ok(result) => {
                let (status, stdout, stderr) = result?;
                Ok(RunOutput {
                    status: status.code(),
                    stdout,
                    stderr,
                    timed_out: false,
                })
            }
            Err(_) => Ok(RunOutput {
                status: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
                timed_out: true,
            }),
        }
    }

    /// One file operation: `FS op args...`. A helper on the host serves it
    /// when it can (one network round trip); otherwise it is its own SSH
    /// command.
    async fn fs(
        &self,
        op: &str,
        args: &[&str],
        stdin: Option<&[u8]>,
    ) -> Result<RunOutput, ToolError> {
        let remote = self.ensure_connected().await?;
        let mut argv = vec![op];
        argv.extend_from_slice(args);
        let small = stdin.is_none_or(|input| input.len() <= helper::MAX_INPUT);
        if small && !self.helpers.is_disabled() {
            if let Some(out) = self.fs_via_helper(&remote, op, &argv, stdin).await? {
                return Ok(out);
            }
        }
        let command = format!("exec bash -c {} forge-fs {}", quote(FS), quote_argv(&argv));
        let out = self.client(&command, stdin, FS_TIMEOUT).await?;
        if out.timed_out {
            return Err(ToolError::Execution(format!(
                "{} did not answer in time",
                self.host
            )));
        }
        if out.status == Some(255) && !self.master_alive().await {
            return Err(ToolError::Execution(format!(
                "Lost the connection to {}. Forge will reconnect on the next command.",
                self.host
            )));
        }
        Ok(out)
    }

    /// `Ok(None)`: no helper could take it (the host cannot run one, or one
    /// would not start); the caller sends it as its own command.
    async fn fs_via_helper(
        &self,
        remote: &Remote,
        op: &str,
        argv: &[&str],
        input: Option<&[u8]>,
    ) -> Result<Option<RunOutput>, ToolError> {
        let _slot = self.helpers.slot().await;
        // A reused helper may have ended since it was last used; a request
        // that only reads is then safe to send once more to a new one.
        let reads_only = matches!(
            op,
            "resolve" | "meta" | "read" | "lines" | "headlines" | "list" | "find" | "grep"
        );
        for attempt in 0..2 {
            let (mut helper, fresh) = match self.helpers.take_idle() {
                Some(helper) => (helper, false),
                None => match self.start_helper(remote).await {
                    Some(helper) => (helper, true),
                    None => return Ok(None),
                },
            };
            // Dropped with this future (a cancelled tool call) or on any
            // failure below: its state is unknown, so it is never reused.
            let call = helper.call(argv, input, MAX_HELPER_OUTPUT);
            match tokio::time::timeout(FS_TIMEOUT, call).await {
                Ok(Ok(out)) => {
                    if self.helpers.give_back(helper) > 1 {
                        self.prune_helpers_later();
                    }
                    return Ok(Some(out));
                }
                Err(_) => {
                    return Err(ToolError::Execution(format!(
                        "{} did not answer in time",
                        self.host
                    )))
                }
                Ok(Err(error)) => {
                    drop(helper);
                    if !self.master_alive().await {
                        return Err(ToolError::Execution(format!(
                            "Lost the connection to {}. Forge will reconnect on the next command.",
                            self.host
                        )));
                    }
                    if fresh || !reads_only || attempt > 0 {
                        return Err(error);
                    }
                }
            }
        }
        Ok(None)
    }

    /// A new helper for `remote`, or `None` (this host cannot run one; after
    /// repeated failures to start, the session stops trying).
    async fn start_helper(&self, remote: &Remote) -> Option<helper::Helper> {
        let nonce = one_time_token();
        let mut command = self.command();
        command.args(self.client_options()).args([
            "-T".to_string(),
            "--".into(),
            self.host.clone(),
            format!(
                "exec bash -c {} forge-helper --serve {} {} {}",
                quote(FS),
                quote(&remote.temp),
                quote(&nonce),
                MAX_HELPER_OUTPUT + 1
            ),
        ]);
        let within = self.options.connect_timeout + Duration::from_secs(5);
        let me = self.me.clone();
        let on_abandon: helper::Abandon = Arc::new(move |pid| {
            if let Some(shared) = me.upgrade() {
                shared.stop_group(pid);
            }
        });
        match helper::Helper::start(
            command,
            &nonce,
            within,
            self.helpers.generation(),
            on_abandon,
        )
        .await
        {
            Ok(Ok(helper)) => {
                self.helpers.started();
                Some(helper)
            }
            Ok(Err(reason)) => {
                self.helpers.disable(&self.host, &reason);
                None
            }
            Err(error) => {
                self.helpers.failed_to_start(&self.host, &error);
                None
            }
        }
    }

    /// Stop a process group on the host (an abandoned helper and what it was
    /// running): TERM, then KILL. Detached and best effort.
    fn stop_group(&self, pid: u32) {
        // Under bash: the login shell may be one whose `kill` takes no `--`.
        let command = format!(
            "exec bash -c {} forge-stop {pid}",
            quote("kill -TERM -- \"-$1\" 2>/dev/null || exit 0; sleep 1; kill -KILL -- \"-$1\" 2>/dev/null; exit 0")
        );
        let me = self.me.clone();
        detach(async move {
            if let Some(shared) = me.upgrade() {
                let _ = shared.client(&command, None, Duration::from_secs(20)).await;
            }
        });
    }

    /// After the idle time, close the helpers beyond the most recently used
    /// one that have not been used since.
    fn prune_helpers_later(&self) {
        let me = self.me.clone();
        let after = self.options.helper_idle;
        detach(async move {
            tokio::time::sleep(after).await;
            if let Some(shared) = me.upgrade() {
                shared.helpers.prune(after);
            }
        });
    }

    fn lost(&self, command: &str) -> ToolError {
        let first = command.lines().next().unwrap_or("").trim();
        let shown: String = first.chars().take(80).collect();
        let shown = if shown.len() < first.len() || command.lines().nth(1).is_some() {
            format!("{shown}…")
        } else {
            shown
        };
        ToolError::Execution(format!(
            "Lost the connection to {} while running {shown}. It may still be running there. \
             Forge will reconnect on the next command.",
            self.host
        ))
    }

    fn next_run_id(&self) -> String {
        format!(
            "run-{}-{}",
            std::process::id(),
            self.next_run.fetch_add(1, Ordering::Relaxed)
        )
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        self.stop_master();
    }
}

impl RunHandle {
    pub(crate) async fn exit_status(&self, local: Option<i32>) -> Result<Option<i32>, ToolError> {
        match local {
            Some(255) => {}
            other => return Ok(other),
        }
        if !self.shared.master_alive().await {
            return Err(self.shared.lost(&self.command));
        }
        let status_file = format!("{}/{}.status", self.temp, self.id);
        let out = self
            .shared
            .client(
                &format!("cat -- {} 2>/dev/null", quote(&status_file)),
                None,
                self.shared.options.connect_timeout + Duration::from_secs(2),
            )
            .await;
        match out {
            Ok(out) if String::from_utf8_lossy(&out.stdout).trim() == "255" => Ok(Some(255)),
            _ => Err(self.shared.lost(&self.command)),
        }
    }

    /// Best effort and detached: returns at once.
    pub(crate) fn cancel(&self) {
        let shared = Arc::clone(&self.shared);
        let command = format!(
            "exec bash -c {} forge-cancel {} {}",
            quote(CANCEL),
            quote(&self.temp),
            quote(&self.id)
        );
        detach(async move {
            let _ = shared.client(&command, None, Duration::from_secs(20)).await;
        });
    }
}

#[async_trait]
impl Target for SshTarget {
    fn remote_host(&self) -> Option<&str> {
        Some(&self.shared.host)
    }

    fn label(&self) -> String {
        format!("{}:{}", self.shared.host, self.shared.path)
    }

    async fn close(&self) {
        SshTarget::close(self).await;
    }

    async fn shell(&self, request: ShellSpawnRequest<'_>) -> Result<TargetShell, ToolError> {
        let remote = self.shared.ensure_connected().await?;
        let id = self.shared.next_run_id();
        let remote_command = format!(
            "exec bash -c {} forge-run {}",
            quote(RUN),
            quote_argv(&[
                remote.root.as_str(),
                remote.temp.as_str(),
                id.as_str(),
                request.shell,
                request.command,
            ])
        );
        let extra: &[&str] = match request.mode {
            ShellSpawnMode::Run => &["-T", "-n"],
            ShellSpawnMode::Pipe => &["-T"],
            ShellSpawnMode::Pty => &["-tt"],
        };
        let args = self.shared.args_for(extra, &remote_command);
        let mut env = self.shared.kept_env();
        if request.mode == ShellSpawnMode::Pty {
            let term = std::env::var_os("TERM").unwrap_or_else(|| "xterm-256color".into());
            env.push(("TERM".into(), term));
        }
        let spec = ShellSpawnSpec::direct(
            self.shared.options.program.display().to_string(),
            args,
            request.workspace_root,
            env,
        );
        Ok(TargetShell {
            spec,
            remote: Some(RemoteRun::new(RunHandle {
                shared: Arc::clone(&self.shared),
                temp: remote.temp,
                id,
                command: request.command.to_string(),
            })),
        })
    }

    async fn resolve(
        &self,
        ctx: &ToolContext,
        path: &str,
        access: Access,
    ) -> Result<TargetPath, ToolError> {
        let _ = ctx;
        if path.trim().is_empty() {
            return Err(ToolError::Execution("path must not be empty".into()));
        }
        if path.contains('\0') {
            return Err(ToolError::Execution(format!(
                "path `{}` contains a NUL byte",
                path.escape_default()
            )));
        }
        let remote = self.shared.ensure_connected().await?;
        let root = Path::new(&remote.root);
        let requested = Path::new(path);
        let full = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            root.join(requested)
        };
        let escapes = || {
            ToolError::Execution(format!(
                "path `{path}` is outside the workspace and session temp"
            ))
        };
        if full.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(escapes());
        }
        if access == Access::Write {
            let below_root = full.strip_prefix(root).unwrap_or(&full);
            for dir in [".git", ".forge"] {
                if below_root
                    .components()
                    .any(|c| matches!(c, Component::Normal(name) if name == dir))
                {
                    return Err(ToolError::Execution(format!(
                        "refusing to write `{path}`: paths under `{dir}` are not writable by tools"
                    )));
                }
            }
        }
        let full_text = full.to_string_lossy().into_owned();
        let out = self.shared.fs("resolve", &[&full_text], None).await?;
        let fields: Vec<&[u8]> = out.stdout.split(|byte| *byte == 0).collect();
        let kind = fields.first().copied().unwrap_or_default();
        if kind == b"broken" {
            return Err(ToolError::Execution(format!(
                "path `{path}` resolves through a broken symlink"
            )));
        }
        let Some(canonical) = fields
            .get(1)
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        else {
            return Err(ToolError::Execution(format!(
                "could not resolve `{path}` on {}: {}",
                self.shared.host,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        };
        let canonical = PathBuf::from(canonical);
        let roots = [Path::new(&remote.root), Path::new(&remote.temp)];
        if !roots.iter().any(|root| canonical.starts_with(root)) {
            return Err(escapes());
        }
        // Where a link leads counts too: `.git` reached through a link is
        // still `.git`, and a hook planted there runs on the next commit.
        if access == Access::Write {
            let below_root = canonical.strip_prefix(root).unwrap_or(&canonical);
            for dir in [".git", ".forge"] {
                if below_root
                    .components()
                    .any(|c| matches!(c, Component::Normal(name) if name == dir))
                {
                    return Err(ToolError::Execution(format!(
                        "refusing to write `{path}`: it leads under `{dir}`, which tools do not write"
                    )));
                }
            }
        }
        if kind == b"exists" {
            let seen = fields
                .get(2)
                .and_then(|meta| parse_meta(meta).ok())
                .flatten();
            Ok(TargetPath::new(canonical).seen(seen))
        } else {
            Ok(TargetPath::new(full).seen(None))
        }
    }

    async fn metadata(&self, path: &TargetPath) -> Result<Option<FileMeta>, ToolError> {
        // Resolving it just now said what is there: no second trip.
        if let Some(seen) = path.seen_meta() {
            return Ok(seen);
        }
        let out = self.shared.fs("meta", &[&text(path)], None).await?;
        parse_meta(&out.stdout).map_err(|()| helper_error(&self.shared.host, "inspect", path, &out))
    }

    async fn read(&self, path: &TargetPath, max: u64) -> Result<Vec<u8>, ToolError> {
        let limit = if max == u64::MAX {
            "all".to_string()
        } else {
            max.to_string()
        };
        let out = self.shared.fs("read", &[&text(path), &limit], None).await?;
        match out.status {
            Some(0) => Ok(out.stdout),
            Some(NOT_FOUND) => Err(not_found(path)),
            _ => Err(helper_error(&self.shared.host, "read", path, &out)),
        }
    }

    async fn read_lines(
        &self,
        path: &TargetPath,
        offset: u64,
        limit: Option<u64>,
    ) -> Result<String, ToolError> {
        let offset = offset.max(1).to_string();
        let limit = limit.map_or_else(|| "all".to_string(), |limit| limit.to_string());
        let out = self
            .shared
            .fs("lines", &[&text(path), &offset, &limit], None)
            .await?;
        match out.status {
            Some(0) => {}
            Some(NOT_FOUND) => return Err(not_found(path)),
            _ => return Err(helper_error(&self.shared.host, "read", path, &out)),
        }
        lines_from(out.stdout)
    }

    async fn read_head_and_lines(
        &self,
        path: &TargetPath,
        head: u64,
        offset: u64,
        limit: Option<u64>,
    ) -> Result<(Vec<u8>, Result<String, ToolError>), ToolError> {
        let head = head.to_string();
        let offset = offset.max(1).to_string();
        let limit = limit.map_or_else(|| "all".to_string(), |limit| limit.to_string());
        let out = self
            .shared
            .fs("headlines", &[&text(path), &head, &offset, &limit], None)
            .await?;
        match out.status {
            Some(0) => {}
            Some(NOT_FOUND) => return Err(not_found(path)),
            _ => return Err(helper_error(&self.shared.host, "read", path, &out)),
        }
        let split = out
            .stdout
            .iter()
            .position(|byte| *byte == 0)
            .ok_or_else(|| helper_error(&self.shared.host, "read", path, &out))?;
        let hex = String::from_utf8_lossy(&out.stdout[..split]);
        let header = hex
            .split_whitespace()
            .map(|pair| u8::from_str_radix(pair, 16))
            .collect::<Result<Vec<u8>, _>>()
            .map_err(|_| helper_error(&self.shared.host, "read", path, &out))?;
        let lines = lines_from(out.stdout[split + 1..].to_vec());
        Ok((header, lines))
    }

    async fn replace(&self, path: &TargetPath, bytes: &[u8]) -> Result<Option<Vec<u8>>, ToolError> {
        let out = self.shared.fs("swap", &[&text(path)], Some(bytes)).await?;
        match out.status {
            Some(0) => Ok(Some(out.stdout)),
            Some(4) => Ok(None),
            _ => Err(helper_error(&self.shared.host, "write", path, &out)),
        }
    }

    async fn write(&self, path: &TargetPath, bytes: &[u8]) -> Result<(), ToolError> {
        let out = self.shared.fs("write", &[&text(path)], Some(bytes)).await?;
        if out.status == Some(0) {
            Ok(())
        } else {
            Err(helper_error(&self.shared.host, "write", path, &out))
        }
    }

    async fn remove(&self, path: &TargetPath) -> Result<(), ToolError> {
        let out = self.shared.fs("remove", &[&text(path)], None).await?;
        match out.status {
            Some(0) => Ok(()),
            Some(NOT_FOUND) => Err(not_found(path)),
            _ => Err(helper_error(&self.shared.host, "delete", path, &out)),
        }
    }

    async fn list(&self, path: &TargetPath) -> Result<Vec<DirEntry>, ToolError> {
        let out = self.shared.fs("list", &[&text(path)], None).await?;
        match out.status {
            Some(0) => {}
            Some(NOT_FOUND) => return Err(not_found(path)),
            _ => return Err(helper_error(&self.shared.host, "list", path, &out)),
        }
        let mut entries: Vec<DirEntry> = out
            .stdout
            .split(|byte| *byte == 0)
            .filter(|entry| entry.len() > 1)
            .map(|entry| DirEntry {
                name: String::from_utf8_lossy(&entry[1..]).into_owned(),
                is_dir: entry[0] == b'd',
            })
            .collect();
        entries.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(entries)
    }

    async fn run(&self, argv: &[String], timeout: Duration) -> Result<RunOutput, ToolError> {
        let command = quote_argv(argv);
        let local = self.shared.local_dir.clone();
        make_private_dir(&local)?;
        let TargetShell { spec, remote } = self
            .shell(ShellSpawnRequest {
                mode: ShellSpawnMode::Run,
                shell: "bash",
                command: &command,
                workspace_root: &local,
                egress: None,
                session_tmp: None,
                confined: false,
                egress_label: None,
                credential_env_names: &[],
            })
            .await?;
        let remote = remote.expect("an SSH run has a remote half");
        let mut child = spec
            .tokio_command()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| self.shared.spawn_error(error))?;
        let mut stdout = child.stdout.take().expect("piped");
        let mut stderr = child.stderr.take().expect("piped");
        let run = async {
            let (out, err) = tokio::join!(read_capped(&mut stdout), read_capped(&mut stderr));
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((status, out?, err?))
        };
        match tokio::time::timeout(timeout, run).await {
            Ok(result) => {
                let (status, stdout, stderr) = result?;
                let status = remote.exit_status(status.code()).await?;
                Ok(RunOutput {
                    status,
                    stdout,
                    stderr,
                    timed_out: false,
                })
            }
            // Dropping `remote` kills the command on the host.
            Err(_) => Ok(RunOutput {
                status: None,
                stdout: Vec::new(),
                stderr: Vec::new(),
                timed_out: true,
            }),
        }
    }

    async fn find(&self, pattern: &str, max: usize) -> Result<(Vec<String>, bool), ToolError> {
        let remote = self.shared.ensure_connected().await?;
        let matcher = FileMatcher::new(pattern)?;
        let plan = find_plan(pattern)?;
        let out = self
            .shared
            .fs(
                "find",
                &[
                    &remote.root,
                    &plan.start,
                    plan.name.as_deref().unwrap_or(""),
                    plan.word.as_deref().unwrap_or(""),
                ],
                None,
            )
            .await?;
        if out.status != Some(0) {
            return Err(ToolError::Execution(format!(
                "could not list files on {}: {}",
                self.shared.host,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let mut found: Vec<String> = out
            .stdout
            .split(|byte| *byte == 0)
            .filter(|path| !path.is_empty())
            .map(|path| {
                let path = String::from_utf8_lossy(path);
                path.strip_prefix("./").unwrap_or(&path).to_string()
            })
            .filter(|path| matcher.matches(path))
            .collect();
        found.sort();
        let more = found.len() > max;
        found.truncate(max);
        Ok((found, more))
    }

    async fn grep(
        &self,
        pattern: &str,
        literal: bool,
        path: Option<&TargetPath>,
        include: Option<&str>,
        max: usize,
    ) -> Result<(Vec<GrepHit>, bool), ToolError> {
        let remote = self.shared.ensure_connected().await?;
        let root = Path::new(&remote.root);
        let whereto = match path {
            None => ".".to_string(),
            Some(path) => match path.as_path().strip_prefix(root) {
                Ok(below) if below.as_os_str().is_empty() => ".".to_string(),
                Ok(below) => below.to_string_lossy().into_owned(),
                Err(_) => text(path),
            },
        };
        let limit = (max + 1).to_string();
        let out = self
            .shared
            .fs(
                "grep",
                &[
                    &remote.root,
                    if literal { "literal" } else { "regex" },
                    pattern,
                    include.unwrap_or(""),
                    &whereto,
                    &limit,
                ],
                None,
            )
            .await?;
        if out.status != Some(0) {
            return Err(ToolError::Execution(format!(
                "grep on {}: {}",
                self.shared.host,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        let mut hits: Vec<GrepHit> = out
            .stdout
            .split(|byte| *byte == b'\n')
            .filter_map(|line| {
                let split = line.iter().position(|byte| *byte == 0)?;
                let file = String::from_utf8_lossy(&line[..split]);
                let rest = String::from_utf8_lossy(&line[split + 1..]);
                let (number, text) = rest.split_once(':')?;
                let mut text: String = text.strip_suffix('\r').unwrap_or(text).to_string();
                if text.chars().count() > 500 {
                    text = text.chars().take(500).collect::<String>() + "…";
                }
                Some(GrepHit {
                    path: file.strip_prefix("./").unwrap_or(&file).to_string(),
                    line: number.parse().ok()?,
                    text,
                })
            })
            .collect();
        hits.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        let more = hits.len() > max;
        hits.truncate(max);
        Ok((hits, more))
    }
}

/// How the host narrows a `glob` listing before it is sent: the folder to
/// start in (the pattern's leading literal folders) and, when the last part
/// is a plain file-name pattern, that name (`find -name`); for a word, a
/// case-insensitive match on the path. The exact match is still made here.
#[derive(Debug, Clone, PartialEq, Eq)]
struct FindPlan {
    start: String,
    name: Option<String>,
    word: Option<String>,
}

fn find_plan(pattern: &str) -> Result<FindPlan, ToolError> {
    let pattern = pattern.trim().trim_start_matches("./");
    if Path::new(pattern).is_absolute() || pattern.split('/').any(|part| part == "..") {
        return Err(ToolError::Execution(format!(
            "glob: `{pattern}` must stay inside the workspace"
        )));
    }
    let has_meta = |part: &str| part.contains(['*', '?', '[', '{']);
    if !has_meta(pattern) {
        // Only a plain word is handed to the host's own pattern matching.
        let plain = !pattern.is_empty()
            && pattern
                .chars()
                .all(|c| c.is_alphanumeric() || "-_.@+ ".contains(c));
        return Ok(FindPlan {
            start: ".".into(),
            name: None,
            word: plain.then(|| pattern.to_string()),
        });
    }
    let parts: Vec<&str> = pattern.split('/').filter(|part| !part.is_empty()).collect();
    let literal = parts.iter().take_while(|part| !has_meta(part)).count();
    let start = if literal == 0 {
        ".".to_string()
    } else {
        parts[..literal].join("/")
    };
    let last = parts.last().copied().unwrap_or_default();
    let name =
        (literal < parts.len() && last != "**" && !last.contains('{')).then(|| last.to_string());
    Ok(FindPlan {
        start,
        name,
        word: None,
    })
}

/// `glob` on a remote target: a pattern with glob characters is matched
/// against the whole relative path (or, without a `/`, the file name); a
/// plain word matches any path containing it, ignoring case.
struct FileMatcher {
    glob: Option<globset::GlobMatcher>,
    on_name: bool,
    word: String,
}

impl FileMatcher {
    fn new(pattern: &str) -> Result<Self, ToolError> {
        let pattern = pattern.trim().trim_start_matches("./");
        let is_glob = pattern.contains(['*', '?', '[', '{']);
        if !is_glob {
            return Ok(Self {
                glob: None,
                on_name: false,
                word: pattern.to_lowercase(),
            });
        }
        let glob = globset::GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .map_err(|error| ToolError::Execution(format!("glob: {error}")))?
            .compile_matcher();
        Ok(Self {
            glob: Some(glob),
            on_name: !pattern.contains('/'),
            word: String::new(),
        })
    }

    fn matches(&self, path: &str) -> bool {
        match &self.glob {
            Some(glob) => {
                glob.is_match(path)
                    || (self.on_name && glob.is_match(path.rsplit('/').next().unwrap_or(path)))
            }
            None => path.to_lowercase().contains(&self.word),
        }
    }
}

/// Lines as `read_lines` returns them: without their endings, joined with
/// `\n`; an error when they are not UTF-8, as a local read would be.
fn lines_from(bytes: Vec<u8>) -> Result<String, ToolError> {
    let body = String::from_utf8(bytes).map_err(|_| {
        ToolError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "stream did not contain valid UTF-8",
        ))
    })?;
    let body = body.strip_suffix('\n').unwrap_or(&body);
    if body.is_empty() {
        return Ok(String::new());
    }
    Ok(body
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Run `work` on the current runtime, or on a thread of its own outside
/// one (from `Drop`). Nothing waits for it.
fn detach(work: impl std::future::Future<Output = ()> + Send + 'static) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(work);
        }
        Err(_) => {
            let _ = std::thread::Builder::new()
                .name("forge-ssh-detached".into())
                .spawn(move || {
                    if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    {
                        runtime.block_on(work);
                    }
                });
        }
    }
}

/// `meta`'s answer: `none`, `dir 0` or `file <size>`.
fn parse_meta(answer: &[u8]) -> Result<Option<FileMeta>, ()> {
    let answer = String::from_utf8_lossy(answer);
    let mut words = answer.split_whitespace();
    match (words.next(), words.next()) {
        (Some("none"), _) => Ok(None),
        (Some("dir"), _) => Ok(Some(FileMeta {
            is_dir: true,
            len: 0,
        })),
        (Some("file"), Some(len)) => Ok(Some(FileMeta {
            is_dir: false,
            len: len.parse().unwrap_or(0),
        })),
        _ => Err(()),
    }
}

fn text(path: &TargetPath) -> String {
    path.as_path().to_string_lossy().into_owned()
}

fn not_found(path: &TargetPath) -> ToolError {
    ToolError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        format!("{} does not exist", path.as_path().display()),
    ))
}

fn helper_error(host: &str, verb: &str, path: &TargetPath, out: &RunOutput) -> ToolError {
    let detail = String::from_utf8_lossy(&out.stderr);
    let detail = detail.trim();
    ToolError::Execution(if detail.is_empty() {
        format!(
            "could not {verb} {} on {host} (exit {})",
            path.as_path().display(),
            out.status
                .map_or_else(|| "signal".into(), |code| code.to_string())
        )
    } else {
        format!(
            "could not {verb} {} on {host}: {detail}",
            path.as_path().display()
        )
    })
}

/// Create `dir` readable by this user only.
fn make_private_dir(dir: &Path) -> Result<(), ToolError> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

async fn read_capped<R: tokio::io::AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(bytes);
        }
        if bytes.len() < MAX_HELPER_OUTPUT {
            let keep = count.min(MAX_HELPER_OUTPUT - bytes.len());
            bytes.extend_from_slice(&buffer[..keep]);
        }
    }
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|window| window == needle)
}

/// An absolute path with no `.` or `..` parts.
fn is_plain_absolute(path: &str) -> bool {
    let path = Path::new(path);
    path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

/// What the preflight's `mktemp -d .../forge-session.XXXXXXXX` makes, and
/// nothing else: closing the session deletes it.
fn is_session_temp(path: &str) -> bool {
    let name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let suffix = name.strip_prefix("forge-session.").unwrap_or_default();
    is_plain_absolute(path)
        && Path::new(path)
            .parent()
            .is_some_and(|parent| parent != Path::new("/"))
        && suffix.len() == 8
        && suffix.chars().all(|c| c.is_ascii_alphanumeric())
}

/// 128 bits from the standard library's randomly keyed hasher.
fn one_time_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let half = || {
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u128(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or_default(),
        );
        hasher.finish()
    };
    format!("{:016x}{:016x}", half(), half())
}

/// One clear sentence for why the connection did not come up.
fn connect_failure(host: &str, stderr: &str) -> String {
    let lower = stderr.to_lowercase();
    let reason = if lower.contains("remote host identification has changed") {
        "its host key has changed since you last connected. If you expected that, update \
         known_hosts in a terminal, then try again."
            .to_string()
    } else if lower.contains("host key verification failed") || lower.contains("host key is known")
    {
        format!(
            "the host key is not known. Connect once with ssh {host} in a terminal, then try \
             again."
        )
    } else if lower.contains("permission denied") {
        format!(
            "it did not accept your SSH keys. Forge uses your keys and agent without asking \
             for a password; check that ssh {host} works in a terminal."
        )
    } else if lower.contains("could not resolve hostname") {
        "the host name does not resolve.".to_string()
    } else if lower.contains("timed out") {
        "the connection timed out.".to_string()
    } else if lower.contains("connection refused") {
        "the connection was refused.".to_string()
    } else {
        let last = stderr
            .lines()
            .map(str::trim)
            .rfind(|line| !line.is_empty())
            .unwrap_or("ssh could not connect");
        format!("{last}.")
    };
    format!("Could not connect to {host}: {reason}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_ssh_failure_reads_as_one_sentence() {
        let unknown = connect_failure(
            "devbox",
            "No ED25519 host key is known for devbox and you have requested strict checking.\r\nHost key verification failed.\r\n",
        );
        assert!(unknown.starts_with("Could not connect to devbox: the host key is not known"));
        assert!(
            connect_failure("d", "ssh: connect to host d port 22: Connection refused")
                .ends_with("the connection was refused.")
        );
        assert!(connect_failure("d", "d: Permission denied (publickey).").contains("SSH keys"));
        assert!(connect_failure("d", "weird thing happened\n").ends_with("weird thing happened."));
    }

    const PROBE: &str = "a'b\\\\c\"$d `e` f";

    fn reply(nonce: &str, root: &str, temp: &str) -> Vec<u8> {
        format!("FORGE-PREFLIGHT-{nonce}\0probe={PROBE}\0os=Linux\0root={root}\0temp={temp}\0")
            .into_bytes()
    }

    fn answer(parts: &[&[u8]]) -> RunOutput {
        RunOutput {
            status: Some(0),
            stdout: parts.concat(),
            stderr: Vec::new(),
            timed_out: false,
        }
    }

    /// A startup file on the host prints before Forge's script runs: a
    /// forged answer there, with or without this connection's token, must
    /// not set the remote root or the temp directory closing deletes.
    #[test]
    fn only_this_connections_own_answer_counts() {
        let old_style = b"FORGE-PREFLIGHT\0probe=x\0os=Linux\0root=/\0temp=/home/me\0".to_vec();
        let guessed = reply("guessed", "/", "/tmp/forge-session.AAAAAAAA");
        let copied = reply("n0nce", "/", "/tmp/forge-session.BBBBBBBB");
        let real = reply("n0nce", "/srv/app", "/tmp/forge-session.k3Jd9sQa");
        let out = answer(&[&old_style, &guessed, &copied, b"motd\n", &real]);
        let remote = parse_preflight("devbox", "/srv/app", PROBE, "n0nce", &out).unwrap();
        assert_eq!(remote.root, "/srv/app");
        assert_eq!(remote.temp, "/tmp/forge-session.k3Jd9sQa");

        let forged_only = answer(&[&old_style, &guessed]);
        assert!(parse_preflight("devbox", "/srv/app", PROBE, "n0nce", &forged_only).is_err());
    }

    #[test]
    fn a_temp_directory_that_is_not_forges_own_is_refused() {
        for temp in [
            "/home/me",
            "/",
            "/tmp/forge-session.AAAAAAAA/..",
            "/tmp/forge-session.AAAAAAAA/../home",
            "/tmp/forge-session.short",
            "/tmp/other.k3Jd9sQa",
            "forge-session.k3Jd9sQa",
        ] {
            let out = answer(&[&reply("n0nce", "/srv/app", temp)]);
            assert!(
                parse_preflight("devbox", "/srv/app", PROBE, "n0nce", &out).is_err(),
                "{temp}"
            );
        }
        for temp in [
            "/tmp/forge-session.k3Jd9sQa",
            "/var/folders/xy/abc/T/forge-session.Zz09aaBB",
        ] {
            let out = answer(&[&reply("n0nce", "/srv/app", temp)]);
            assert!(
                parse_preflight("devbox", "/srv/app", PROBE, "n0nce", &out).is_ok(),
                "{temp}"
            );
        }
    }

    /// Closing removes what the target made in its directory, not whatever
    /// else a caller keeps there.
    #[tokio::test]
    async fn closing_leaves_other_files_in_the_local_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keep.txt"), "mine").unwrap();
        let target = SshTarget::new("devbox", "/srv/app", dir.path(), SshOptions::default());
        target.close().await;
        assert!(dir.path().join("keep.txt").exists());
        let empty = tempfile::tempdir().unwrap();
        let own = empty.path().join("s1");
        std::fs::create_dir(&own).unwrap();
        SshTarget::new("devbox", "/srv/app", &own, SshOptions::default())
            .close()
            .await;
        assert!(!own.exists(), "its own empty directory goes");
    }

    #[test]
    fn a_glob_is_narrowed_on_the_host_where_it_can_be() {
        let plan = |start: &str, name: Option<&str>, word: Option<&str>| FindPlan {
            start: start.into(),
            name: name.map(String::from),
            word: word.map(String::from),
        };
        for (pattern, expected) in [
            ("src/nested/*.rs", plan("src/nested", Some("*.rs"), None)),
            ("./src/*.rs", plan("src", Some("*.rs"), None)),
            ("**/*.md", plan(".", Some("*.md"), None)),
            ("*.md", plan(".", Some("*.md"), None)),
            ("src/**", plan("src", None, None)),
            ("src/**/mod.rs", plan("src", Some("mod.rs"), None)),
            ("src/{a,b}/*.rs", plan("src", Some("*.rs"), None)),
            ("{a,b}.rs", plan(".", None, None)),
            ("nested", plan(".", None, Some("nested"))),
            ("ui/button", plan(".", None, None)),
        ] {
            assert_eq!(find_plan(pattern).unwrap(), expected, "{pattern}");
        }
        for escaping in ["../x/*.rs", "src/../../*", "/etc/*.conf"] {
            assert!(find_plan(escaping).is_err(), "{escaping}");
        }
    }

    #[test]
    fn remote_globs_match_like_local_ones() {
        let rs = FileMatcher::new("**/*.rs").unwrap();
        assert!(rs.matches("src/lib.rs") && rs.matches("lib.rs") && !rs.matches("README.md"));
        let name = FileMatcher::new("*.md").unwrap();
        assert!(name.matches("docs/a.md") && name.matches("a.md"));
        let scoped = FileMatcher::new("src/*.rs").unwrap();
        assert!(scoped.matches("src/lib.rs") && !scoped.matches("src/nested/mod.rs"));
        let word = FileMatcher::new("Button").unwrap();
        assert!(word.matches("ui/button.tsx") && !word.matches("ui/input.tsx"));
    }
}
