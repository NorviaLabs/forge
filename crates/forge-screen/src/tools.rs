//! The `desktop_*` tools.
//!
//! Every call goes through the same gate, in order: the feature is on and
//! the display supported; the model can see images; the user consented to
//! screen control this session; this session holds the screen; the user has
//! not taken the mouse back; the target is not Forge's own window. Only then
//! does the action run, followed by a fresh screenshot.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use forge_config::DesktopConfig;
use forge_tools::{Tool, ToolContext, ToolError};
use forge_types::{ConsentKind, ConsentRequest, ImageRef, SessionId, SideEffectClass, ToolOutput};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::backend::{BackendError, Button, Point, Rect, ScreenBackend, Unsupported, WindowInfo};
use crate::keys::KeyChord;
use crate::lease::ScreenController;
use crate::scale::{encode_png, Scaling, MAX_SHOT_HEIGHT, MAX_SHOT_WIDTH};
use crate::takeover::TakeoverGuard;

/// Every desktop tool, in the order the model sees them.
pub const DESKTOP_TOOL_NAMES: [&str; 6] = [
    "desktop_screenshot",
    "desktop_click",
    "desktop_move",
    "desktop_scroll",
    "desktop_type",
    "desktop_key",
];

/// The scope of the screen-control consent.
pub const SCREEN_SCOPE: &str = "screen";

/// Connects to the display on first use.
pub type Connector = Box<dyn Fn() -> Result<Arc<dyn ScreenBackend>, Unsupported> + Send + Sync>;

/// The display, or why there is none.
type Connected = Result<Arc<dyn ScreenBackend>, Unsupported>;

/// How long a failed connection is remembered before trying again.
const RETRY_CONNECT_AFTER: Duration = Duration::from_secs(30);

/// Desktop control for one opened session (and its children).
pub struct Desktop {
    config: Mutex<DesktopConfig>,
    controller: Arc<ScreenController>,
    connect: Connector,
    backend: Mutex<Option<(Instant, Connected)>>,
    own_pid: u32,
    settle_ms: AtomicU64,
    guard: TakeoverGuard,
    /// Sessions that used the screen through this desktop, released when
    /// the tools shut down.
    served: Mutex<HashSet<SessionId>>,
    /// Where screenshots go: a private directory, never the session's
    /// temp dir (which sandboxed commands can write).
    shots: forge_tools::screenshots::ScreenshotStore,
}

impl Desktop {
    /// `connect` is called on first use and its result kept. `own_pid` is
    /// Forge's process, whose windows the agent may never act on.
    pub fn new(
        config: DesktopConfig,
        controller: Arc<ScreenController>,
        connect: Connector,
        own_pid: u32,
    ) -> Arc<Self> {
        Arc::new(Self {
            config: Mutex::new(config),
            controller,
            connect,
            backend: Mutex::new(None),
            own_pid,
            settle_ms: AtomicU64::new(400),
            guard: TakeoverGuard::default(),
            served: Mutex::new(HashSet::new()),
            shots: Default::default(),
        })
    }

    /// How long to let the screen settle after an action before the
    /// screenshot. Tests set zero.
    pub fn with_settle(self: Arc<Self>, settle: Duration) -> Arc<Self> {
        self.settle_ms
            .store(settle.as_millis() as u64, Ordering::Relaxed);
        self
    }

    pub fn set_config(&self, config: DesktopConfig) {
        *lock(&self.config) = config;
    }

    pub fn config(&self) -> DesktopConfig {
        lock(&self.config).clone()
    }

    fn backend(&self) -> Result<Arc<dyn ScreenBackend>, Unsupported> {
        let mut cached = lock(&self.backend);
        if let Some((at, result)) = cached.as_ref() {
            if result.is_ok() || at.elapsed() < RETRY_CONNECT_AFTER {
                return result.clone();
            }
        }
        let result = (self.connect)();
        *cached = Some((Instant::now(), result.clone()));
        result
    }

    /// Whether this machine's display can be controlled, and why not.
    pub fn support(&self) -> Result<(), Unsupported> {
        self.backend().map(drop)
    }

    /// Turned on and supported.
    pub fn is_available(&self) -> bool {
        self.config().enabled && self.support().is_ok()
    }

    /// Whether `session` holds the screen right now.
    pub fn is_controlling(&self, session: SessionId) -> bool {
        self.controller
            .holder()
            .is_some_and(|holder| holder.session == session)
    }

    /// End `session`'s control: release the screen and revoke its consent
    /// (Stop in the app, a cancelled turn). `false` when it held nothing.
    pub fn stop(&self, session: SessionId) -> bool {
        let released = self.controller.release(session);
        if released {
            self.guard.reset();
        }
        released
    }

    async fn run(&self, ctx: &ToolContext, action: Action) -> Result<ToolOutput, ToolError> {
        if !ctx.image_input {
            return Ok(ToolOutput::denied(
                "desktop control works through screenshots, and the active model cannot see images",
            ));
        }
        let backend = match self.backend() {
            Ok(backend) => backend,
            Err(reason) => {
                return Ok(ToolOutput::failed_exit(
                    format!("desktop control is not available here: {reason}"),
                    None,
                ))
            }
        };
        let session = ctx.session_id.unwrap_or_default();
        let area = blocking({
            let backend = backend.clone();
            move || backend.primary_area()
        })
        .await?;

        if !ctx.consents.check(
            ConsentKind::ScreenControl,
            SCREEN_SCOPE,
            ctx.tool_call_id.as_deref(),
        ) {
            return Err(ToolError::ConsentRequired(Box::new(consent_request(
                ctx,
                area,
                action.purpose(),
            ))));
        }
        if let Err(refused) = self.controller.acquire(session, ctx.consents.clone()) {
            return Ok(ToolOutput::failed_exit(format!("refused: {refused}"), None));
        }
        lock(&self.served).insert(session);

        let pointer = blocking({
            let backend = backend.clone();
            move || backend.pointer()
        })
        .await?;
        if self.guard.check(pointer).is_err() {
            self.stop(session);
            return Ok(ToolOutput::failed_exit(
                "stopped: you moved the mouse, so the agent gave the screen back. It must ask \
                 again before it uses the screen.",
                None,
            ));
        }

        let scaling = Scaling::fit(area, MAX_SHOT_WIDTH, MAX_SHOT_HEIGHT);
        let own_pid = self.own_pid;
        let acted = blocking({
            let backend = backend.clone();
            move || act(backend.as_ref(), &scaling, own_pid, &action)
        })
        .await?;
        let summary = match acted {
            Acted::Done { summary, pointer } => {
                if let Some(pointer) = pointer {
                    self.guard.expect(pointer);
                }
                summary
            }
            Acted::Refused(message) => return Ok(ToolOutput::denied(message)),
            Acted::Failed(message) => return Ok(ToolOutput::failed_exit(message, None)),
        };

        let settle = Duration::from_millis(self.settle_ms.load(Ordering::Relaxed));
        if !settle.is_zero() && summary.is_some() {
            tokio::time::sleep(settle).await;
        }
        let shot = blocking({
            let backend = backend.clone();
            move || {
                let small = backend.capture_scaled(area, &scaling)?;
                Ok(encode_png(&small))
            }
        })
        .await?;
        let path = self.shots.save("desktop", "png", &shot).map_err(|error| {
            ToolError::Execution(format!("could not save a screenshot: {error}"))
        })?;
        let size = format!(
            "{} × {} → {} × {}",
            area.width, area.height, scaling.shot_width, scaling.shot_height
        );
        let content = match summary {
            Some(summary) => format!("{summary}\nScreenshot after: {size}"),
            None => size,
        };
        let mut output = ToolOutput::success(content);
        output.attachments =
            vec![
                ImageRef::new(path.display().to_string(), "image/png", shot.len() as u64)
                    .with_dimensions(Some(scaling.shot_width), Some(scaling.shot_height)),
            ];
        Ok(output)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Run blocking display work off the async runtime.
async fn blocking<T, F>(work: F) -> Result<T, ToolError>
where
    F: FnOnce() -> Result<T, BackendError> + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| ToolError::Execution(format!("desktop control stopped: {error}")))?
        .map_err(|error| ToolError::Execution(error.to_string()))
}

fn consent_request(ctx: &ToolContext, area: Rect, purpose: Option<&str>) -> ConsentRequest {
    let model = if ctx.active_model.is_empty() {
        "the model".to_string()
    } else {
        ctx.active_model.clone()
    };
    let mut request = ConsentRequest::new(
        ConsentKind::ScreenControl,
        SCREEN_SCOPE,
        "Control your screen, mouse and keyboard for this session",
    )
    .with_fact(
        "Sees",
        format!(
            "Your primary display ({} × {}), including other apps and notifications",
            area.width, area.height
        ),
    )
    .with_fact("Sends", format!("Screenshots to {model}"))
    .with_fact(
        "Controls",
        "Your mouse and keyboard, as you, outside the sandbox",
    )
    .with_fact("Never", "Forge's own window")
    .with_fact(
        "Ends",
        "When this turn ends, you press Stop, or you move the mouse",
    );
    if let Some(purpose) = purpose.map(str::trim).filter(|p| !p.is_empty()) {
        request = request.with_reason(purpose.chars().take(300).collect::<String>());
    }
    request
}

/// One desktop action, in screenshot coordinates.
#[derive(Debug, Clone)]
enum Action {
    Screenshot {
        purpose: Option<String>,
    },
    Click {
        x: i32,
        y: i32,
        button: Button,
        double: bool,
        purpose: Option<String>,
    },
    Move {
        x: i32,
        y: i32,
        purpose: Option<String>,
    },
    Scroll {
        x: i32,
        y: i32,
        dx: i32,
        dy: i32,
        purpose: Option<String>,
    },
    Type {
        text: String,
        purpose: Option<String>,
    },
    Key {
        keys: String,
        purpose: Option<String>,
    },
}

impl Action {
    fn purpose(&self) -> Option<&str> {
        match self {
            Action::Screenshot { purpose }
            | Action::Click { purpose, .. }
            | Action::Move { purpose, .. }
            | Action::Scroll { purpose, .. }
            | Action::Type { purpose, .. }
            | Action::Key { purpose, .. } => purpose.as_deref(),
        }
    }
}

enum Acted {
    /// Done; `summary` is `None` for a bare screenshot. `pointer` is where
    /// the agent left the pointer, when it moved it.
    Done {
        summary: Option<String>,
        pointer: Option<Point>,
    },
    Refused(String),
    Failed(String),
}

const FORGE_WINDOW: &str = "refused: that is Forge's window; the agent never acts on it";
const UNKNOWN_OWNER: &str =
    "refused: Forge cannot tell which program owns that window, so it could be Forge's";

/// The most `desktop_type` types in one call.
const MAX_TYPED_CHARS: usize = 10_000;

fn button_name(button: Button) -> &'static str {
    match button {
        Button::Left => "left",
        Button::Middle => "middle",
        Button::Right => "right",
    }
}

/// Perform `action` (blocking).
fn act(
    backend: &dyn ScreenBackend,
    scaling: &Scaling,
    own_pid: u32,
    action: &Action,
) -> Result<Acted, BackendError> {
    let point_of = |x: i32, y: i32| -> Result<Point, Acted> {
        scaling.to_screen(Point::new(x, y)).ok_or_else(|| {
            Acted::Failed(format!(
                "({x}, {y}) is outside the screenshot ({} × {})",
                scaling.shot_width, scaling.shot_height
            ))
        })
    };
    // Input never goes to Forge's window, nor to a window whose owner
    // cannot be told (it could be Forge's).
    let off_limits = |window: &Option<WindowInfo>| -> Option<String> {
        let window = window.as_ref()?;
        match window.pid {
            Some(pid) if pid == own_pid => Some(FORGE_WINDOW.into()),
            Some(_) => None,
            None => Some(UNKNOWN_OWNER.into()),
        }
    };
    // Keys go to the focused window, or with none to the one under the
    // pointer.
    let key_target = || -> Result<Option<WindowInfo>, BackendError> {
        match backend.focused_window()? {
            Some(window) => Ok(Some(window)),
            None => backend.window_at(backend.pointer()?),
        }
    };
    Ok(match action {
        Action::Screenshot { .. } => Acted::Done {
            summary: None,
            pointer: None,
        },
        Action::Click {
            x,
            y,
            button,
            double,
            ..
        } => {
            let point = match point_of(*x, *y) {
                Ok(point) => point,
                Err(acted) => return Ok(acted),
            };
            let window = backend.window_at(point)?;
            if let Some(refusal) = off_limits(&window) {
                return Ok(Acted::Refused(refusal));
            }
            backend.move_to(point)?;
            let clicks = if *double { 2 } else { 1 };
            for _ in 0..clicks {
                backend.button(*button, true)?;
                backend.button(*button, false)?;
            }
            let title = window.map(|window| window.title).unwrap_or_default();
            let mut summary = format!(
                "({x}, {y}) {}{}",
                button_name(*button),
                if *double { " double-click" } else { "" }
            );
            if !title.is_empty() {
                summary.push_str(&format!(" · {title}"));
            }
            Acted::Done {
                summary: Some(summary),
                pointer: Some(point),
            }
        }
        Action::Move { x, y, .. } => {
            let point = match point_of(*x, *y) {
                Ok(point) => point,
                Err(acted) => return Ok(acted),
            };
            backend.move_to(point)?;
            Acted::Done {
                summary: Some(format!("pointer at ({x}, {y})")),
                pointer: Some(point),
            }
        }
        Action::Scroll { x, y, dx, dy, .. } => {
            let point = match point_of(*x, *y) {
                Ok(point) => point,
                Err(acted) => return Ok(acted),
            };
            if let Some(refusal) = off_limits(&backend.window_at(point)?) {
                return Ok(Acted::Refused(refusal));
            }
            backend.move_to(point)?;
            backend.scroll(*dx, *dy)?;
            let mut parts = Vec::new();
            if *dy != 0 {
                parts.push(format!(
                    "{} {}",
                    if *dy > 0 { "down" } else { "up" },
                    dy.abs()
                ));
            }
            if *dx != 0 {
                parts.push(format!(
                    "{} {}",
                    if *dx > 0 { "right" } else { "left" },
                    dx.abs()
                ));
            }
            Acted::Done {
                summary: Some(format!("({x}, {y}) {}", parts.join(", "))),
                pointer: Some(point),
            }
        }
        Action::Type { text, .. } => {
            // Checked before every character: focus can move while typing.
            let total = text.chars().count();
            for (typed, c) in text.chars().enumerate() {
                if let Some(refusal) = off_limits(&key_target()?) {
                    return Ok(Acted::Refused(if typed == 0 {
                        refusal
                    } else {
                        format!("stopped after {typed} of {total} characters: {refusal}")
                    }));
                }
                let mut buf = [0u8; 4];
                backend.type_text(c.encode_utf8(&mut buf))?;
            }
            let shown: String = text.chars().take(60).collect();
            Acted::Done {
                summary: Some(format!(
                    "\u{201c}{shown}\u{201d} ({} characters)",
                    text.chars().count()
                )),
                pointer: None,
            }
        }
        Action::Key { keys, .. } => {
            let chord = match KeyChord::parse(keys) {
                Ok(chord) => chord,
                Err(error) => return Ok(Acted::Failed(error)),
            };
            if let Some(refusal) = off_limits(&key_target()?) {
                return Ok(Acted::Refused(refusal));
            }
            backend.key(&chord)?;
            Acted::Done {
                summary: Some(keys.clone()),
                pointer: None,
            }
        }
    })
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ScreenshotArgs {
    #[doc = "Why you need the screen (shown to the user)."]
    #[serde(default)]
    purpose: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ClickArgs {
    x: i32,
    y: i32,
    #[serde(default)]
    button: Option<Button>,
    #[serde(default)]
    double: Option<bool>,
    #[doc = "Why you need the screen (shown to the user)."]
    #[serde(default)]
    purpose: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct MoveArgs {
    x: i32,
    y: i32,
    #[doc = "Why you need the screen (shown to the user)."]
    #[serde(default)]
    purpose: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ScrollArgs {
    x: i32,
    y: i32,
    #[serde(default)]
    dx: Option<i32>,
    #[serde(default)]
    dy: Option<i32>,
    #[doc = "Why you need the screen (shown to the user)."]
    #[serde(default)]
    purpose: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct TypeArgs {
    text: String,
    #[doc = "Why you need the screen (shown to the user)."]
    #[serde(default)]
    purpose: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct KeyArgs {
    keys: String,
    #[doc = "Why you need the screen (shown to the user)."]
    #[serde(default)]
    purpose: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Screenshot,
    Click,
    Move,
    Scroll,
    Type,
    Key,
}

struct DesktopTool {
    kind: Kind,
    desktop: Arc<Desktop>,
}

fn schema<T: JsonSchema>() -> Value {
    forge_tools::compact_schema(
        serde_json::to_value(schemars::schema_for!(T))
            .unwrap_or_else(|_| json!({"type": "object"})),
    )
}

fn parse<T: for<'de> Deserialize<'de>>(tool: &str, args: Value) -> Result<T, ToolError> {
    serde_json::from_value(args)
        .map_err(|error| ToolError::Execution(format!("{tool}: invalid arguments: {error}")))
}

impl Kind {
    fn name(self) -> &'static str {
        DESKTOP_TOOL_NAMES[self as usize]
    }

    fn description(self) -> &'static str {
        match self {
            Kind::Screenshot => "See the user's screen (primary display, fit to 1280×800). This acts outside the sandbox as the user: the first desktop_* call in a turn asks the user, so give a `purpose`; control ends with the turn, or when the user moves the mouse (ask again). Every desktop_* tool answers with a new screenshot; x, y are its pixels. Forge's own window is off limits.",
            Kind::Click => "Click at x, y; double: true double-clicks.",
            Kind::Move => "Move the pointer to x, y (hover).",
            Kind::Scroll => "Turn the mouse wheel at x, y: dy notches down (negative: up), dx right.",
            Kind::Type => "Type text into the focused window (click it first).",
            Kind::Key => "Press a key or chord in the focused window: Return, Tab, Escape, ctrl+s, alt+Tab.",
        }
    }

    fn schema(self) -> Value {
        match self {
            Kind::Screenshot => schema::<ScreenshotArgs>(),
            Kind::Click => schema::<ClickArgs>(),
            Kind::Move => schema::<MoveArgs>(),
            Kind::Scroll => schema::<ScrollArgs>(),
            Kind::Type => schema::<TypeArgs>(),
            Kind::Key => schema::<KeyArgs>(),
        }
    }

    fn action(self, args: Value) -> Result<Action, ToolError> {
        let name = self.name();
        Ok(match self {
            Kind::Screenshot => {
                let args: ScreenshotArgs = parse(name, args)?;
                Action::Screenshot {
                    purpose: args.purpose,
                }
            }
            Kind::Click => {
                let args: ClickArgs = parse(name, args)?;
                Action::Click {
                    x: args.x,
                    y: args.y,
                    button: args.button.unwrap_or(Button::Left),
                    double: args.double.unwrap_or(false),
                    purpose: args.purpose,
                }
            }
            Kind::Move => {
                let args: MoveArgs = parse(name, args)?;
                Action::Move {
                    x: args.x,
                    y: args.y,
                    purpose: args.purpose,
                }
            }
            Kind::Scroll => {
                let args: ScrollArgs = parse(name, args)?;
                let (dx, dy) = (args.dx.unwrap_or(0), args.dy.unwrap_or(0));
                if dx == 0 && dy == 0 {
                    return Err(ToolError::Execution(
                        "desktop_scroll needs `dx` or `dy` (wheel notches)".into(),
                    ));
                }
                Action::Scroll {
                    x: args.x,
                    y: args.y,
                    dx: dx.clamp(-50, 50),
                    dy: dy.clamp(-50, 50),
                    purpose: args.purpose,
                }
            }
            Kind::Type => {
                let args: TypeArgs = parse(name, args)?;
                if args.text.chars().count() > MAX_TYPED_CHARS {
                    return Err(ToolError::Execution(format!(
                        "desktop_type types at most {MAX_TYPED_CHARS} characters at a time"
                    )));
                }
                Action::Type {
                    text: args.text,
                    purpose: args.purpose,
                }
            }
            Kind::Key => {
                let args: KeyArgs = parse(name, args)?;
                Action::Key {
                    keys: args.keys,
                    purpose: args.purpose,
                }
            }
        })
    }
}

#[async_trait]
impl Tool for DesktopTool {
    fn name(&self) -> &str {
        self.kind.name()
    }

    fn description(&self) -> &str {
        self.kind.description()
    }

    fn input_schema(&self) -> Value {
        self.kind.schema()
    }

    fn side_effect_class(&self) -> SideEffectClass {
        match self.kind {
            Kind::Screenshot => SideEffectClass::Read,
            _ => SideEffectClass::Exec,
        }
    }

    fn is_available(&self, _ctx: &ToolContext) -> bool {
        self.desktop.is_available()
    }

    fn requires_image_input(&self) -> bool {
        true
    }

    async fn call(&self, ctx: &ToolContext, args: Value) -> Result<ToolOutput, ToolError> {
        let action = self.kind.action(args)?;
        self.desktop.run(ctx, action).await
    }

    /// A cancelled turn ends screen control.
    async fn cancel_session(&self, session_id: SessionId) {
        self.desktop.stop(session_id);
    }

    /// So does any other end of the turn: the next turn asks again.
    async fn turn_ended(&self, session_id: SessionId) {
        self.desktop.stop(session_id);
    }

    /// The session is retired: give the screen back.
    async fn shutdown(&self) {
        let served: Vec<SessionId> = lock(&self.desktop.served).drain().collect();
        for session in served {
            self.desktop.stop(session);
        }
    }
}

/// The desktop tools, sharing `desktop`.
pub fn desktop_tools(desktop: Arc<Desktop>) -> Vec<Arc<dyn Tool>> {
    [
        Kind::Screenshot,
        Kind::Click,
        Kind::Move,
        Kind::Scroll,
        Kind::Type,
        Kind::Key,
    ]
    .into_iter()
    .map(|kind| {
        Arc::new(DesktopTool {
            kind,
            desktop: desktop.clone(),
        }) as Arc<dyn Tool>
    })
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::{BackendError, Button, Frame, Point, Rect, WindowInfo};
    use crate::keys::KeyChord;
    use forge_tools::{ConsentKind, ToolContext, ToolError};
    use serde_json::json;
    use std::sync::Mutex;

    /// A 2560×1440 display that records what was done to it.
    #[derive(Default)]
    struct FakeDisplay {
        actions: Mutex<Vec<String>>,
        pointer: Mutex<Point>,
        /// Owner pid of any window, and of the focused window.
        window_pid: Mutex<Option<u32>>,
        focused_pid: Mutex<Option<u32>>,
        /// No window has focus (keys go to the window under the pointer).
        no_focus: Mutex<bool>,
        /// After this many typed characters Forge takes the focus.
        forge_focuses_after: Mutex<Option<usize>>,
        typed: Mutex<usize>,
    }

    impl FakeDisplay {
        fn actions(&self) -> Vec<String> {
            self.actions.lock().unwrap().clone()
        }
        fn user_moves_mouse_to(&self, point: Point) {
            *self.pointer.lock().unwrap() = point;
        }
    }

    impl ScreenBackend for FakeDisplay {
        fn description(&self) -> String {
            "fake".into()
        }
        fn primary_area(&self) -> Result<Rect, BackendError> {
            Ok(Rect {
                x: 0,
                y: 0,
                width: 2560,
                height: 1440,
            })
        }
        fn capture(&self, area: Rect) -> Result<Frame, BackendError> {
            Ok(Frame {
                width: area.width,
                height: area.height,
                rgba: vec![128; (area.width * area.height * 4) as usize],
            })
        }
        fn pointer(&self) -> Result<Point, BackendError> {
            Ok(*self.pointer.lock().unwrap())
        }
        fn move_to(&self, point: Point) -> Result<(), BackendError> {
            *self.pointer.lock().unwrap() = point;
            self.actions
                .lock()
                .unwrap()
                .push(format!("move {} {}", point.x, point.y));
            Ok(())
        }
        fn button(&self, button: Button, pressed: bool) -> Result<(), BackendError> {
            self.actions.lock().unwrap().push(format!(
                "{button:?} {}",
                if pressed { "down" } else { "up" }
            ));
            Ok(())
        }
        fn scroll(&self, dx: i32, dy: i32) -> Result<(), BackendError> {
            self.actions
                .lock()
                .unwrap()
                .push(format!("scroll {dx} {dy}"));
            Ok(())
        }
        fn key(&self, chord: &KeyChord) -> Result<(), BackendError> {
            self.actions
                .lock()
                .unwrap()
                .push(format!("key {:?} {:#x}", chord.modifiers, chord.key));
            Ok(())
        }
        fn type_text(&self, text: &str) -> Result<(), BackendError> {
            self.actions.lock().unwrap().push(format!("type {text}"));
            let mut typed = self.typed.lock().unwrap();
            *typed += text.chars().count();
            if self
                .forge_focuses_after
                .lock()
                .unwrap()
                .is_some_and(|after| *typed >= after)
            {
                *self.focused_pid.lock().unwrap() = Some(FORGE_PID);
            }
            Ok(())
        }
        fn window_at(&self, _point: Point) -> Result<Option<WindowInfo>, BackendError> {
            Ok(Some(WindowInfo {
                pid: *self.window_pid.lock().unwrap(),
                title: "Document Viewer — report.pdf".into(),
            }))
        }
        fn focused_window(&self) -> Result<Option<WindowInfo>, BackendError> {
            if *self.no_focus.lock().unwrap() {
                return Ok(None);
            }
            Ok(Some(WindowInfo {
                pid: *self.focused_pid.lock().unwrap(),
                title: "Editor".into(),
            }))
        }
    }

    const FORGE_PID: u32 = 4242;

    struct Rig {
        display: Arc<FakeDisplay>,
        desktop: Arc<Desktop>,
        tools: Vec<Arc<dyn Tool>>,
        _lock: tempfile::TempDir,
        controller: Arc<ScreenController>,
    }

    fn rig(enabled: bool) -> Rig {
        let display = Arc::new(FakeDisplay::default());
        // Windows belong to some other program unless a test says otherwise.
        *display.window_pid.lock().unwrap() = Some(7);
        *display.focused_pid.lock().unwrap() = Some(7);
        let lock = tempfile::tempdir().unwrap();
        let controller = ScreenController::new(lock.path().join("screen.lock"));
        let backend: Arc<dyn ScreenBackend> = display.clone();
        let desktop = Desktop::new(
            DesktopConfig { enabled },
            controller.clone(),
            Box::new(move || Ok(backend.clone())),
            FORGE_PID,
        )
        .with_settle(Duration::ZERO);
        let tools = desktop_tools(desktop.clone());
        Rig {
            display,
            desktop,
            tools,
            _lock: lock,
            controller,
        }
    }

    fn session(n: u128) -> SessionId {
        SessionId::from_u128(n)
    }

    fn ctx(tmp: &tempfile::TempDir, session_id: SessionId, call: &str) -> ToolContext {
        let mut ctx = ToolContext::new(tmp.path().to_path_buf())
            .with_session_id(session_id)
            .with_tool_call_id(call);
        ctx.image_input = true;
        ctx.active_model = "anthropic/claude-sonnet-4.5".into();
        ctx
    }

    fn consented(tmp: &tempfile::TempDir, session_id: SessionId, call: &str) -> ToolContext {
        let ctx = ctx(tmp, session_id, call);
        ctx.consents.grant(ConsentKind::ScreenControl, SCREEN_SCOPE);
        ctx
    }

    impl Rig {
        fn tool(&self, name: &str) -> Arc<dyn Tool> {
            self.tools
                .iter()
                .find(|tool| tool.name() == name)
                .unwrap()
                .clone()
        }
    }

    #[test]
    fn every_tool_is_present_needs_images_and_runs_serially() {
        let rig = rig(true);
        let names: Vec<&str> = rig.tools.iter().map(|tool| tool.name()).collect();
        assert_eq!(names, DESKTOP_TOOL_NAMES);
        for tool in &rig.tools {
            assert!(tool.requires_image_input(), "{}", tool.name());
            assert!(!tool.parallel_safe(), "{}", tool.name());
            assert!(
                tool.input_schema()["properties"].get("purpose").is_some(),
                "{}",
                tool.name()
            );
        }
    }

    /// What every request carries when the feature is on: no generator
    /// boilerplate, and a size budget (about 30% under the first version).
    #[test]
    fn the_tool_definitions_are_compact() {
        let tools = rig(true).tools;
        let mut total = 0;
        for tool in &tools {
            let schema = serde_json::to_string(&tool.input_schema()).unwrap();
            for noise in ["$schema", "\"title\"", "$defs", "\"format\"", "\"default\""] {
                assert!(
                    !schema.contains(noise),
                    "{}: {noise} in {schema}",
                    tool.name()
                );
            }
            total += tool.name().len() + tool.description().len() + schema.len();
        }
        let budget = 2_900;
        assert!(total <= budget, "{total} characters (budget {budget})");
    }

    /// Each tool's compacted schema accepts and refuses exactly what the
    /// schema generated from its argument type does.
    #[test]
    fn compact_schemas_validate_exactly_as_generated() {
        fn generated<T: JsonSchema>() -> Value {
            serde_json::to_value(schemars::schema_for!(T)).unwrap()
        }
        fn errors(schema: &Value, arguments: &Value) -> Vec<String> {
            forge_tools::compile_validator(schema)
                .unwrap()
                .iter_errors(arguments)
                .map(|error| error.instance_path().to_string())
                .collect()
        }
        let table = [
            ("desktop_screenshot", generated::<ScreenshotArgs>()),
            ("desktop_click", generated::<ClickArgs>()),
            ("desktop_move", generated::<MoveArgs>()),
            ("desktop_scroll", generated::<ScrollArgs>()),
            ("desktop_type", generated::<TypeArgs>()),
            ("desktop_key", generated::<KeyArgs>()),
        ];
        let rig = rig(true);
        assert_eq!(table.len(), rig.tools.len(), "every tool is in the table");
        for (name, generated) in &table {
            forge_test_support::schema_check::assert_same_validation(
                name,
                generated,
                &rig.tool(name).input_schema(),
                &errors,
            );
        }
    }

    #[test]
    fn the_descriptions_still_state_the_rules() {
        let rig = rig(true);
        let first = rig.tool("desktop_screenshot").description().to_string();
        for rule in ["purpose", "Forge's own window", "move", "turn"] {
            assert!(first.contains(rule), "{rule}: {first}");
        }
        let mut registry = forge_tools::ToolRegistry::new();
        for tool in rig.tools.clone() {
            registry.register(tool);
        }
        assert!(registry
            .validate_call("desktop_click", &json!({"x": 1, "y": 2, "button": null}))
            .is_ok());
        assert!(registry
            .validate_call(
                "desktop_click",
                &json!({"x": 1, "y": 2, "button": "sideways"})
            )
            .is_err());
    }

    #[test]
    fn availability_follows_the_setting_and_the_display() {
        let rig = rig(false);
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp, session(1), "c");
        assert!(!rig.tools[0].is_available(&ctx));
        rig.desktop.set_config(DesktopConfig { enabled: true });
        assert!(rig.tools[0].is_available(&ctx));

        let lock = tempfile::tempdir().unwrap();
        let wayland = Desktop::new(
            DesktopConfig { enabled: true },
            ScreenController::new(lock.path().join("l")),
            Box::new(|| Err(Unsupported::Wayland)),
            FORGE_PID,
        );
        assert!(!wayland.is_available());
        assert_eq!(wayland.support(), Err(Unsupported::Wayland));
    }

    #[tokio::test]
    async fn a_text_only_model_is_refused() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        let mut ctx = consented(&tmp, session(1), "c");
        ctx.image_input = false;
        let output = rig
            .tool("desktop_screenshot")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("image"), "{}", output.content);
        assert!(rig.display.actions().is_empty());
    }

    /// The first use asks, with what the grant means, and touches nothing.
    #[tokio::test]
    async fn the_first_use_asks_for_consent_with_the_facts() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ctx(&tmp, session(1), "c1");
        let error = rig
            .tool("desktop_screenshot")
            .call(&ctx, json!({"purpose": "check the exported PDF"}))
            .await
            .unwrap_err();
        let ToolError::ConsentRequired(request) = error else {
            panic!("expected a consent request, got {error}");
        };
        assert_eq!(request.kind, ConsentKind::ScreenControl);
        assert_eq!(request.scope, SCREEN_SCOPE);
        assert_eq!(request.reason, "check the exported PDF");
        let facts: Vec<(&str, &str)> = request
            .facts
            .iter()
            .map(|fact| (fact.label.as_str(), fact.value.as_str()))
            .collect();
        assert_eq!(facts[0].0, "Sees");
        assert!(facts[0].1.contains("2560 × 1440"), "{facts:?}");
        assert!(facts.iter().any(
            |(label, value)| *label == "Sends" && value.contains("anthropic/claude-sonnet-4.5")
        ));
        assert!(facts
            .iter()
            .any(|(label, value)| *label == "Never" && value.contains("Forge")));
        assert!(
            facts
                .iter()
                .any(|(label, value)| *label == "Ends" && value.contains("this turn ends")),
            "{facts:?}"
        );
        assert!(rig.display.actions().is_empty());
        assert!(rig.controller.holder().is_none(), "asking holds nothing");
    }

    #[tokio::test]
    async fn a_screenshot_is_scaled_and_attached() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        let ctx = consented(&tmp, session(1), "c1");
        let output = rig
            .tool("desktop_screenshot")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert!(
            output.content.contains("2560 × 1440 → 1280 × 720"),
            "{}",
            output.content
        );
        let image = &output.attachments[0];
        assert_eq!((image.width, image.height), (Some(1280), Some(720)));
        let bytes = std::fs::read(&image.path).unwrap();
        assert_eq!(&bytes[..4], b"\x89PNG");
        assert_eq!(rig.controller.holder().unwrap().session, session(1));
    }

    /// Screenshots are not written where sandboxed commands can write: a
    /// command could swap the folder for a link.
    #[tokio::test]
    async fn a_screenshot_stays_out_of_the_session_temp_dir() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        let session_tmp = forge_tools::SessionTempDir::create(session(1)).unwrap();
        let ctx = consented(&tmp, session(1), "c1").with_session_tmp(session_tmp.clone());
        let output = rig
            .tool("desktop_screenshot")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        let image = &output.attachments[0];
        assert!(
            !std::path::Path::new(&image.path).starts_with(session_tmp.path()),
            "{}",
            image.path
        );
        assert!(!session_tmp.path().join("screenshots").exists());
    }

    /// Screenshot coordinates are scaled back to the screen.
    #[tokio::test]
    async fn a_click_lands_where_the_screenshot_showed() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        let ctx = consented(&tmp, session(1), "c1");
        let output = rig
            .tool("desktop_click")
            .call(&ctx, json!({"x": 412, "y": 288}))
            .await
            .unwrap();
        assert!(!output.is_error, "{}", output.content);
        assert_eq!(
            rig.display.actions(),
            ["move 824 576", "Left down", "Left up"]
        );
        assert!(
            output.content.starts_with("(412, 288) left"),
            "{}",
            output.content
        );
        assert!(
            output.content.contains("Document Viewer"),
            "{}",
            output.content
        );
        assert_eq!(output.attachments.len(), 1, "a fresh screenshot follows");

        let ctx = consented(&tmp, session(1), "c2");
        rig.tool("desktop_click")
            .call(
                &ctx,
                json!({"x": 10, "y": 10, "button": "right", "double": true}),
            )
            .await
            .unwrap();
        assert_eq!(
            &rig.display.actions()[3..],
            [
                "move 20 20",
                "Right down",
                "Right up",
                "Right down",
                "Right up"
            ]
        );
    }

    #[tokio::test]
    async fn a_point_off_the_screenshot_is_refused() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        let ctx = consented(&tmp, session(1), "c1");
        let output = rig
            .tool("desktop_click")
            .call(&ctx, json!({"x": 5000, "y": 10}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("outside"), "{}", output.content);
        assert!(rig.display.actions().is_empty());
    }

    #[tokio::test]
    async fn move_scroll_type_and_keys_drive_the_display() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        let run = |name: &'static str, args: serde_json::Value, call: &'static str| {
            let tool = rig.tool(name);
            let ctx = consented(&tmp, session(1), call);
            async move { tool.call(&ctx, args).await.unwrap() }
        };
        run("desktop_move", json!({"x": 100, "y": 50}), "a").await;
        run("desktop_scroll", json!({"x": 640, "y": 400, "dy": 5}), "b").await;
        run("desktop_type", json!({"text": "Q3 revenue"}), "c").await;
        let key = run("desktop_key", json!({"keys": "ctrl+f"}), "d").await;
        assert!(!key.is_error, "{}", key.content);
        let actions = rig.display.actions();
        assert_eq!(
            &actions[..3],
            ["move 200 100", "move 1280 800", "scroll 0 5"]
        );
        let typed: String = actions
            .iter()
            .filter_map(|action| action.strip_prefix("type "))
            .collect();
        assert_eq!(typed, "Q3 revenue");
        assert_eq!(actions.last().unwrap(), "key [65507] 0x66");
        let bad = run("desktop_key", json!({"keys": "hyper+q"}), "e").await;
        assert!(bad.is_error);
    }

    /// The agent never acts on Forge's own window: it could approve its own
    /// requests there.
    #[tokio::test]
    async fn forges_own_window_is_off_limits() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        *rig.display.window_pid.lock().unwrap() = Some(FORGE_PID);
        let output = rig
            .tool("desktop_click")
            .call(
                &consented(&tmp, session(1), "c1"),
                json!({"x": 30, "y": 12}),
            )
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(
            output.content.contains("Forge's window"),
            "{}",
            output.content
        );
        assert!(
            !rig.display.actions().iter().any(|a| a.contains("down")),
            "no click reached Forge"
        );

        *rig.display.window_pid.lock().unwrap() = Some(7);
        *rig.display.focused_pid.lock().unwrap() = Some(FORGE_PID);
        for (name, args) in [
            ("desktop_type", json!({"text": "yes"})),
            ("desktop_key", json!({"keys": "Return"})),
        ] {
            let output = rig
                .tool(name)
                .call(&consented(&tmp, session(1), "c2"), args)
                .await
                .unwrap();
            assert!(output.is_error, "{name}");
            assert!(
                output.content.contains("Forge's window"),
                "{}",
                output.content
            );
        }
        assert!(!rig
            .display
            .actions()
            .iter()
            .any(|a| a.starts_with("type") || a.starts_with("key")));
    }

    /// Moving the mouse takes control back: the next call stops, the
    /// consent is gone and the screen is free.
    #[tokio::test]
    async fn moving_the_mouse_takes_control_back() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        let ctx = consented(&tmp, session(1), "c1");
        rig.tool("desktop_click")
            .call(&ctx, json!({"x": 100, "y": 100}))
            .await
            .unwrap();
        rig.display.user_moves_mouse_to(Point::new(1500, 900));

        let ctx2 = ctx.clone().with_tool_call_id("c2");
        let output = rig
            .tool("desktop_click")
            .call(&ctx2, json!({"x": 620, "y": 130}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(
            output.content.contains("you moved the mouse"),
            "{}",
            output.content
        );
        assert!(!ctx
            .consents
            .is_granted(ConsentKind::ScreenControl, SCREEN_SCOPE));
        assert!(rig.controller.holder().is_none());
        assert_eq!(rig.display.actions().len(), 3, "nothing after the takeover");

        // Asking again starts fresh.
        let error = rig
            .tool("desktop_screenshot")
            .call(&ctx.clone().with_tool_call_id("c3"), json!({}))
            .await
            .unwrap_err();
        assert!(matches!(error, ToolError::ConsentRequired(_)));
    }

    /// A window whose owner cannot be told might be Forge's: not acted on.
    #[tokio::test]
    async fn a_window_of_unknown_owner_is_off_limits() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        *rig.display.window_pid.lock().unwrap() = None;
        let output = rig
            .tool("desktop_click")
            .call(
                &consented(&tmp, session(1), "c1"),
                json!({"x": 30, "y": 12}),
            )
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("cannot tell"), "{}", output.content);
        assert!(!rig.display.actions().iter().any(|a| a.contains("down")));
    }

    /// With no focused window, keys go to the window under the pointer: that
    /// is the one checked.
    #[tokio::test]
    async fn keys_without_focus_are_checked_against_the_window_under_the_pointer() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        *rig.display.no_focus.lock().unwrap() = true;
        *rig.display.window_pid.lock().unwrap() = Some(FORGE_PID);
        let output = rig
            .tool("desktop_key")
            .call(
                &consented(&tmp, session(1), "c1"),
                json!({"keys": "Return"}),
            )
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(
            output.content.contains("Forge's window"),
            "{}",
            output.content
        );
    }

    /// Focus is checked before every character: if Forge takes it mid-way,
    /// typing stops there.
    #[tokio::test]
    async fn typing_stops_when_forge_takes_the_focus() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        *rig.display.forge_focuses_after.lock().unwrap() = Some(3);
        let output = rig
            .tool("desktop_type")
            .call(
                &consented(&tmp, session(1), "c1"),
                json!({"text": "abcdef"}),
            )
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(
            output.content.contains("Forge's window"),
            "{}",
            output.content
        );
        let typed: String = rig
            .display
            .actions()
            .iter()
            .filter_map(|action| action.strip_prefix("type "))
            .collect();
        assert_eq!(typed, "abc");
    }

    #[tokio::test]
    async fn typing_is_capped() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        let error = rig
            .tool("desktop_type")
            .call(
                &consented(&tmp, session(1), "c1"),
                json!({"text": "x".repeat(10_001)}),
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("10000"), "{error}");
        assert!(rig.display.actions().is_empty());
    }

    /// Control ends with the turn: the next turn asks again.
    #[tokio::test]
    async fn control_ends_when_the_turn_ends() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        let ctx = consented(&tmp, session(1), "a");
        rig.tool("desktop_screenshot")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        assert!(rig.desktop.is_controlling(session(1)));
        rig.tool("desktop_click").turn_ended(session(1)).await;
        assert!(!rig.desktop.is_controlling(session(1)));
        assert!(!ctx
            .consents
            .is_granted(ConsentKind::ScreenControl, SCREEN_SCOPE));
    }

    #[tokio::test]
    async fn another_session_holding_the_screen_is_refused() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        rig.tool("desktop_screenshot")
            .call(&consented(&tmp, session(1), "a"), json!({}))
            .await
            .unwrap();
        let output = rig
            .tool("desktop_screenshot")
            .call(&consented(&tmp, session(2), "b"), json!({}))
            .await
            .unwrap();
        assert!(output.is_error);
        assert!(
            output.content.contains("another session"),
            "{}",
            output.content
        );
    }

    #[tokio::test]
    async fn stop_and_a_cancelled_turn_end_control() {
        let rig = rig(true);
        let tmp = tempfile::tempdir().unwrap();
        let ctx = consented(&tmp, session(1), "a");
        rig.tool("desktop_screenshot")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        assert!(rig.desktop.is_controlling(session(1)));
        assert!(rig.desktop.stop(session(1)));
        assert!(!rig.desktop.is_controlling(session(1)));
        assert!(!ctx
            .consents
            .is_granted(ConsentKind::ScreenControl, SCREEN_SCOPE));
        assert!(!rig.desktop.stop(session(1)));

        let ctx = consented(&tmp, session(1), "b");
        rig.tool("desktop_screenshot")
            .call(&ctx, json!({}))
            .await
            .unwrap();
        rig.tool("desktop_click").cancel_session(session(1)).await;
        assert!(!rig.desktop.is_controlling(session(1)));
        assert!(!ctx
            .consents
            .is_granted(ConsentKind::ScreenControl, SCREEN_SCOPE));
    }
}
