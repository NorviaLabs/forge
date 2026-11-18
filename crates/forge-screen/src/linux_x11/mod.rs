//! Linux X11: capture with the core protocol, input with XTEST, the primary
//! output from RandR, and a window's owner from X-Resource.
//!
//! Pure Rust (`x11rb`): no libX11, libxdo or other C libraries are loaded.

use std::sync::Mutex;
use std::time::Duration;

use x11rb::connection::{Connection, RequestConnection};
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::res::{self, ConnectionExt as _};
use x11rb::protocol::xproto::{
    self, Atom, AtomEnum, ConnectionExt as _, ImageFormat, ImageOrder, Keycode, Keysym, Window,
};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use crate::backend::{
    BackendError, Button, Frame, Point, Rect, ScreenBackend, Unsupported, WindowInfo,
};
use crate::keys::{char_keysym, keysym, KeyChord};
use crate::scale::{downscale, downscale_raw, RawLayout, Scaling};

/// Let clients pick up a keymap change before a key that relies on it.
const REMAP_SETTLE: Duration = Duration::from_millis(20);

/// A connection to one X display.
pub struct X11Backend {
    conn: RustConnection,
    root: Window,
    display: String,
    atoms: Atoms,
    /// Multi-request sequences (a chord, a remapped key) run one at a time.
    input: Mutex<()>,
}

struct Atoms {
    wm_state: Atom,
    net_wm_name: Atom,
    utf8_string: Atom,
}

/// The keyboard mapping: `per` keysyms for each keycode from `min`.
struct Keymap {
    min: Keycode,
    per: usize,
    syms: Vec<Keysym>,
}

impl Keymap {
    fn count(&self) -> usize {
        self.syms.len() / self.per.max(1)
    }

    fn at(&self, index: usize, level: usize) -> Keysym {
        if level < self.per {
            self.syms[index * self.per + level]
        } else {
            0
        }
    }

    /// The keycode producing `sym`, and whether Shift is needed for it.
    fn find(&self, sym: Keysym) -> Option<(Keycode, bool)> {
        for level in 0..2 {
            for index in 0..self.count() {
                if self.at(index, level) == sym {
                    return Some((self.min + index as u8, level == 1));
                }
            }
        }
        None
    }

    /// A keycode with no symbols at all, free to borrow.
    fn spare(&self) -> Option<Keycode> {
        (0..self.count())
            .rev()
            .find(|index| (0..self.per).all(|level| self.at(*index, level) == 0))
            .map(|index| self.min + index as u8)
    }
}

fn display_error(error: impl std::fmt::Display) -> BackendError {
    BackendError::Display(error.to_string())
}

impl X11Backend {
    /// Connect to `display` (`":0"`), or to `$DISPLAY` when `None`.
    pub fn connect(display: Option<&str>) -> Result<Self, Unsupported> {
        let (conn, screen) =
            RustConnection::connect(display).map_err(|e| Unsupported::Connect(e.to_string()))?;
        let root = conn.setup().roots[screen].root;
        let has = |name: &'static str| conn.extension_information(name).ok().flatten().is_some();
        if !has(x11rb::protocol::xtest::X11_EXTENSION_NAME) {
            return Err(Unsupported::Connect(
                "the X server has no XTEST extension, so input cannot be sent".into(),
            ));
        }
        // Without X-Resource the owner of a window cannot be told, and the
        // agent must never act on Forge's own: refuse rather than guess.
        if !has(res::X11_EXTENSION_NAME) {
            return Err(Unsupported::Connect(
                "the X server has no X-Resource extension, which desktop control needs to keep \
                 the agent off Forge's own window"
                    .into(),
            ));
        }
        let intern = |name: &[u8]| -> Result<Atom, Unsupported> {
            conn.intern_atom(false, name)
                .map_err(|e| Unsupported::Connect(e.to_string()))?
                .reply()
                .map(|reply| reply.atom)
                .map_err(|e| Unsupported::Connect(e.to_string()))
        };
        let atoms = Atoms {
            wm_state: intern(b"WM_STATE")?,
            net_wm_name: intern(b"_NET_WM_NAME")?,
            utf8_string: intern(b"UTF8_STRING")?,
        };
        let display = display
            .map(str::to_string)
            .or_else(|| std::env::var("DISPLAY").ok())
            .unwrap_or_default();
        Ok(Self {
            conn,
            root,
            display,
            atoms,
            input: Mutex::new(()),
        })
    }

    fn sync(&self) -> Result<(), BackendError> {
        // A round trip: everything sent before it has been processed.
        self.conn
            .get_input_focus()
            .map_err(display_error)?
            .reply()
            .map_err(display_error)?;
        Ok(())
    }

    fn fake(&self, kind: u8, detail: u8, x: i16, y: i16) -> Result<(), BackendError> {
        self.conn
            .xtest_fake_input(kind, detail, x11rb::CURRENT_TIME, self.root, x, y, 0)
            .map_err(display_error)?;
        Ok(())
    }

    fn keymap(&self) -> Result<Keymap, BackendError> {
        let setup = self.conn.setup();
        let (min, max) = (setup.min_keycode, setup.max_keycode);
        let reply = self
            .conn
            .get_keyboard_mapping(min, max - min + 1)
            .map_err(display_error)?
            .reply()
            .map_err(display_error)?;
        Ok(Keymap {
            min,
            per: reply.keysyms_per_keycode as usize,
            syms: reply.keysyms,
        })
    }

    fn key_event(&self, keycode: Keycode, pressed: bool) -> Result<(), BackendError> {
        let kind = if pressed {
            xproto::KEY_PRESS_EVENT
        } else {
            xproto::KEY_RELEASE_EVENT
        };
        self.fake(kind, keycode, 0, 0)
    }

    /// Press and release `sym` (with Shift when its level needs it),
    /// borrowing a spare keycode for a symbol the keymap lacks.
    fn tap(&self, map: &Keymap, sym: Keysym, shift_held: bool) -> Result<(), BackendError> {
        if let Some((keycode, needs_shift)) = map.find(sym) {
            let shift = if needs_shift && !shift_held {
                map.find(keysym::SHIFT_L).map(|(code, _)| code)
            } else {
                None
            };
            if let Some(shift) = shift {
                self.key_event(shift, true)?;
            }
            self.key_event(keycode, true)?;
            self.key_event(keycode, false)?;
            if let Some(shift) = shift {
                self.key_event(shift, false)?;
            }
            return self.sync();
        }
        let spare = map
            .spare()
            .ok_or_else(|| BackendError::Untypable(char::from_u32(sym).unwrap_or('?')))?;
        let per = map.per.max(1) as u8;
        let mut syms = vec![0 as Keysym; per as usize];
        syms[0] = sym;
        if per > 1 {
            syms[1] = sym;
        }
        self.conn
            .change_keyboard_mapping(1, spare, per, &syms)
            .map_err(display_error)?;
        self.sync()?;
        std::thread::sleep(REMAP_SETTLE);
        let pressed = self
            .key_event(spare, true)
            .and_then(|()| self.key_event(spare, false))
            .and_then(|()| self.sync());
        std::thread::sleep(REMAP_SETTLE);
        // Give the keycode back whatever happened.
        let blank = vec![0 as Keysym; per as usize];
        let restored = self
            .conn
            .change_keyboard_mapping(1, spare, per, &blank)
            .map_err(display_error)
            .and_then(|_| self.sync());
        pressed.and(restored)
    }

    fn property(&self, window: Window, property: Atom, kind: Atom) -> Option<Vec<u8>> {
        let reply = self
            .conn
            .get_property(false, window, property, kind, 0, 1024)
            .ok()?
            .reply()
            .ok()?;
        (reply.type_ != 0).then_some(reply.value)
    }

    fn title(&self, window: Window) -> String {
        let bytes = self
            .property(window, self.atoms.net_wm_name, self.atoms.utf8_string)
            .or_else(|| self.property(window, AtomEnum::WM_NAME.into(), AtomEnum::STRING.into()))
            .unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }

    /// The application window inside `top` (a window manager frame holds the
    /// client, which carries `WM_STATE`); `top` itself without a manager.
    fn client_of(&self, top: Window) -> Window {
        let mut queue = vec![(top, 0)];
        while let Some((window, depth)) = queue.pop() {
            if self
                .property(window, self.atoms.wm_state, AtomEnum::ANY.into())
                .is_some()
            {
                return window;
            }
            if depth < 3 {
                if let Ok(Ok(tree)) = self.conn.query_tree(window).map(|cookie| cookie.reply()) {
                    queue.extend(tree.children.into_iter().map(|child| (child, depth + 1)));
                }
            }
        }
        top
    }

    /// The process owning `window`, from the X server's own record of the
    /// client connection (not a property the application could forge).
    fn owner_pid(&self, window: Window) -> Option<u32> {
        let spec = res::ClientIdSpec {
            client: window,
            mask: res::ClientIdMask::LOCAL_CLIENT_PID,
        };
        let reply = self.conn.res_query_client_ids(&[spec]).ok()?.reply().ok()?;
        reply
            .ids
            .iter()
            .find(|id| {
                u32::from(id.spec.mask) & u32::from(res::ClientIdMask::LOCAL_CLIENT_PID) != 0
            })
            .and_then(|id| id.value.first().copied())
    }

    fn info(&self, window: Window) -> WindowInfo {
        let client = self.client_of(window);
        WindowInfo {
            pid: self.owner_pid(client).or_else(|| self.owner_pid(window)),
            title: self.title(client),
        }
    }

    fn get_image(&self, area: Rect) -> Result<xproto::GetImageReply, BackendError> {
        self.conn
            .get_image(
                ImageFormat::Z_PIXMAP,
                self.root,
                area.x as i16,
                area.y as i16,
                area.width as u16,
                area.height as u16,
                !0,
            )
            .map_err(display_error)?
            .reply()
            .map_err(display_error)
    }

    /// Where red, green and blue sit in each pixel, when every channel is a
    /// whole byte (24-bit colour in 24 or 32 bits per pixel); `None` for any
    /// other format, which takes the general conversion.
    fn raw_layout(&self, depth: u8, visual: u32, width: u32) -> Option<RawLayout> {
        let setup = self.conn.setup();
        let format = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == depth)?;
        let bytes_per_pixel = match format.bits_per_pixel {
            32 => 4,
            24 => 3,
            _ => return None,
        };
        let visual = setup
            .roots
            .iter()
            .flat_map(|screen| screen.allowed_depths.iter())
            .flat_map(|depth| depth.visuals.iter())
            .find(|candidate| candidate.visual_id == visual)?;
        let lsb = setup.image_byte_order == ImageOrder::LSB_FIRST;
        let offset = |mask: u32| -> Option<usize> {
            let shift = mask.trailing_zeros();
            if mask.count_ones() != 8 || !shift.is_multiple_of(8) || (mask >> shift) != 0xff {
                return None;
            }
            let byte = (shift / 8) as usize;
            (byte < bytes_per_pixel).then(|| {
                if lsb {
                    byte
                } else {
                    bytes_per_pixel - 1 - byte
                }
            })
        };
        let pad = usize::from(format.scanline_pad.max(8));
        Some(RawLayout {
            bytes_per_pixel,
            stride: (width as usize * usize::from(format.bits_per_pixel)).div_ceil(pad) * pad / 8,
            red: offset(visual.red_mask)?,
            green: offset(visual.green_mask)?,
            blue: offset(visual.blue_mask)?,
        })
    }

    /// Convert a Z-pixmap image to RGBA using the visual's masks.
    fn to_rgba(
        &self,
        depth: u8,
        visual: u32,
        data: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Frame, BackendError> {
        let setup = self.conn.setup();
        let bpp = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == depth)
            .map(|format| format.bits_per_pixel)
            .ok_or_else(|| BackendError::Display(format!("no pixmap format for depth {depth}")))?;
        let pad = setup
            .pixmap_formats
            .iter()
            .find(|format| format.depth == depth)
            .map(|format| format.scanline_pad)
            .unwrap_or(32);
        let visual = setup
            .roots
            .iter()
            .flat_map(|screen| screen.allowed_depths.iter())
            .flat_map(|depth| depth.visuals.iter())
            .find(|candidate| candidate.visual_id == visual)
            .ok_or_else(|| BackendError::Display("unknown visual".into()))?;
        let bytes_per_pixel = match bpp {
            32 => 4,
            24 => 3,
            16 => 2,
            other => {
                return Err(BackendError::Display(format!(
                    "{other} bits per pixel is not supported"
                )))
            }
        };
        let row_bits = width as usize * bpp as usize;
        let pad = pad.max(8) as usize;
        let stride = row_bits.div_ceil(pad) * pad / 8;
        if data.len() < stride * height as usize {
            return Err(BackendError::Display(
                "short image from the X server".into(),
            ));
        }
        let lsb = setup.image_byte_order == ImageOrder::LSB_FIRST;
        let channel = |pixel: u32, mask: u32| -> u8 {
            if mask == 0 {
                return 0;
            }
            let shift = mask.trailing_zeros();
            let bits = (mask >> shift).count_ones();
            let value = (pixel & mask) >> shift;
            ((value * 255) / ((1u32 << bits) - 1)) as u8
        };
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for row in 0..height as usize {
            let line = &data[row * stride..];
            for column in 0..width as usize {
                let at = column * bytes_per_pixel;
                let raw = &line[at..at + bytes_per_pixel];
                let mut pixel = 0u32;
                if lsb {
                    for (index, byte) in raw.iter().enumerate() {
                        pixel |= u32::from(*byte) << (8 * index);
                    }
                } else {
                    for byte in raw {
                        pixel = (pixel << 8) | u32::from(*byte);
                    }
                }
                rgba.push(channel(pixel, visual.red_mask));
                rgba.push(channel(pixel, visual.green_mask));
                rgba.push(channel(pixel, visual.blue_mask));
                rgba.push(255);
            }
        }
        Ok(Frame {
            width,
            height,
            rgba,
        })
    }
}

impl ScreenBackend for X11Backend {
    fn description(&self) -> String {
        format!("X11 {}", self.display)
    }

    fn primary_area(&self) -> Result<Rect, BackendError> {
        let primary = || -> Option<Rect> {
            let output = self
                .conn
                .randr_get_output_primary(self.root)
                .ok()?
                .reply()
                .ok()?
                .output;
            if output == 0 {
                return None;
            }
            let info = self
                .conn
                .randr_get_output_info(output, x11rb::CURRENT_TIME)
                .ok()?
                .reply()
                .ok()?;
            if info.crtc == 0 {
                return None;
            }
            let crtc = self
                .conn
                .randr_get_crtc_info(info.crtc, x11rb::CURRENT_TIME)
                .ok()?
                .reply()
                .ok()?;
            (crtc.width > 0 && crtc.height > 0).then(|| Rect {
                x: i32::from(crtc.x),
                y: i32::from(crtc.y),
                width: u32::from(crtc.width),
                height: u32::from(crtc.height),
            })
        };
        if let Some(area) = primary() {
            return Ok(area);
        }
        let screen = self
            .conn
            .setup()
            .roots
            .iter()
            .find(|screen| screen.root == self.root)
            .ok_or_else(|| BackendError::Display("no screen".into()))?;
        Ok(Rect {
            x: 0,
            y: 0,
            width: u32::from(screen.width_in_pixels),
            height: u32::from(screen.height_in_pixels),
        })
    }

    fn capture(&self, area: Rect) -> Result<Frame, BackendError> {
        let reply = self.get_image(area)?;
        self.to_rgba(
            reply.depth,
            reply.visual,
            &reply.data,
            area.width,
            area.height,
        )
    }

    fn capture_scaled(&self, area: Rect, scaling: &Scaling) -> Result<Frame, BackendError> {
        let reply = self.get_image(area)?;
        match self.raw_layout(reply.depth, reply.visual, area.width) {
            // The usual formats: scale straight from the server's bytes.
            Some(layout) => {
                if reply.data.len() < layout.stride * area.height as usize {
                    return Err(BackendError::Display(
                        "short image from the X server".into(),
                    ));
                }
                Ok(downscale_raw(
                    &reply.data,
                    &layout,
                    area.width,
                    area.height,
                    scaling,
                ))
            }
            None => {
                let frame = self.to_rgba(
                    reply.depth,
                    reply.visual,
                    &reply.data,
                    area.width,
                    area.height,
                )?;
                Ok(downscale(&frame, scaling))
            }
        }
    }

    fn pointer(&self) -> Result<Point, BackendError> {
        let reply = self
            .conn
            .query_pointer(self.root)
            .map_err(display_error)?
            .reply()
            .map_err(display_error)?;
        Ok(Point::new(i32::from(reply.root_x), i32::from(reply.root_y)))
    }

    fn move_to(&self, point: Point) -> Result<(), BackendError> {
        let _input = self.input.lock().unwrap_or_else(|p| p.into_inner());
        self.fake(
            xproto::MOTION_NOTIFY_EVENT,
            0,
            point.x as i16,
            point.y as i16,
        )?;
        self.sync()
    }

    fn button(&self, button: Button, pressed: bool) -> Result<(), BackendError> {
        let _input = self.input.lock().unwrap_or_else(|p| p.into_inner());
        let detail = match button {
            Button::Left => 1,
            Button::Middle => 2,
            Button::Right => 3,
        };
        let kind = if pressed {
            xproto::BUTTON_PRESS_EVENT
        } else {
            xproto::BUTTON_RELEASE_EVENT
        };
        self.fake(kind, detail, 0, 0)?;
        self.sync()
    }

    fn scroll(&self, dx: i32, dy: i32) -> Result<(), BackendError> {
        let _input = self.input.lock().unwrap_or_else(|p| p.into_inner());
        // Wheel notches are buttons: 4 up, 5 down, 6 left, 7 right.
        let notch = |button: u8, times: i32| -> Result<(), BackendError> {
            for _ in 0..times.unsigned_abs() {
                self.fake(xproto::BUTTON_PRESS_EVENT, button, 0, 0)?;
                self.fake(xproto::BUTTON_RELEASE_EVENT, button, 0, 0)?;
            }
            Ok(())
        };
        notch(if dy > 0 { 5 } else { 4 }, dy)?;
        notch(if dx > 0 { 7 } else { 6 }, dx)?;
        self.sync()
    }

    fn key(&self, chord: &KeyChord) -> Result<(), BackendError> {
        let _input = self.input.lock().unwrap_or_else(|p| p.into_inner());
        let map = self.keymap()?;
        let mut held = Vec::new();
        for modifier in &chord.modifiers {
            let (keycode, _) = map.find(*modifier).ok_or_else(|| {
                BackendError::Display(format!("no key for modifier {modifier:#x}"))
            })?;
            self.key_event(keycode, true)?;
            held.push(keycode);
        }
        let shift_held = chord.modifiers.contains(&keysym::SHIFT_L);
        let result = self.tap(&map, chord.key, shift_held);
        for keycode in held.into_iter().rev() {
            self.key_event(keycode, false)?;
        }
        self.sync()?;
        result
    }

    fn type_text(&self, text: &str) -> Result<(), BackendError> {
        let _input = self.input.lock().unwrap_or_else(|p| p.into_inner());
        let map = self.keymap()?;
        for c in text.chars() {
            let sym = match c {
                '\n' => keysym::RETURN,
                '\t' => keysym::TAB,
                c => char_keysym(c),
            };
            self.tap(&map, sym, false)?;
        }
        Ok(())
    }

    fn window_at(&self, point: Point) -> Result<Option<WindowInfo>, BackendError> {
        let reply = self
            .conn
            .translate_coordinates(self.root, self.root, point.x as i16, point.y as i16)
            .map_err(display_error)?
            .reply()
            .map_err(display_error)?;
        if reply.child == 0 {
            return Ok(None);
        }
        Ok(Some(self.info(reply.child)))
    }

    fn focused_window(&self) -> Result<Option<WindowInfo>, BackendError> {
        let focus = self
            .conn
            .get_input_focus()
            .map_err(display_error)?
            .reply()
            .map_err(display_error)?
            .focus;
        // 0 is None, 1 is PointerRoot: no window has the keyboard.
        if focus <= 1 || focus == self.root {
            return Ok(None);
        }
        Ok(Some(self.info(focus)))
    }
}
