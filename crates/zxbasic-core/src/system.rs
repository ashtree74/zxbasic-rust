//! Single-owner state machine for the whole runtime.
//!
//! MVP-3c additions on top of MVP-3b: `FOR / NEXT / STEP` (with a loop
//! stack), and `INPUT` (with a state machine that suspends RUN until the
//! next Enter).

use std::cell::Cell;
use std::collections::HashMap;

use crate::display::{
    make_attr, Display, CHAR_W, FRAME_RGBA_LEN,
};
use crate::expression::{self, Env};
use crate::fp_format;
use crate::program::Program;
use crate::value::{is_string_name, Value};

/// Logical key fed into [`System::feed_key`].
#[derive(Debug, Clone, Copy)]
pub enum Key {
    Char(u8),
    Enter,
    Backspace,
    /// Spectrum BREAK (Caps Shift + Space). We accept it from the host as
    /// a dedicated key — JS maps Esc to this. Interrupts a running program
    /// the same way the ROM does (`08_command.asm:378` calls break_key
    /// after every statement).
    Break,
}

/// How many BASIC statements to execute per host frame. Picked so a tight
/// loop on a desktop processes ~300k stmts/sec while still yielding to the
/// browser event loop every 16 ms — Esc can reach us, the canvas keeps
/// redrawing, and audio scheduled by BEEP fires on time.
const STATEMENTS_PER_FRAME: usize = 5_000;

pub struct System {
    display: Display,
    input_line: String,
    vars: HashMap<String, Value>,
    program: Program,
    prng: Cell<u64>,
    for_stack: Vec<ForFrame>,
    /// GOSUB return stack: line number of the caller's GOSUB statement.
    /// RETURN pops and resumes at the next line after that.
    gosub_stack: Vec<u16>,
    /// User-defined functions (DEF FN). Single-parameter; body stored as raw
    /// source so RUN can re-parse it under each call's local scope.
    user_fns: HashMap<String, UserFn>,
    /// Numeric arrays declared by `DIM` (1D for MVP-5b; multi-dim later).
    /// Strings stored as a separate map in MVP-5c.
    arrays: HashMap<String, Vec<f64>>,
    /// Set while RUN is iterating a line; used by `FOR` to capture the
    /// matching return point for `NEXT`.
    current_line: Option<u16>,
    /// When `Some`, the next Enter binds the input line to `var` and resumes
    /// RUN at `after_line`. While set, the editor renders a `?` prompt.
    pending_input: Option<PendingInput>,
    /// Drawing/print attribute state ("permanent" colours in Spectrum
    /// parlance). Updated by INK/PAPER/BRIGHT/FLASH.
    current_ink: u8,
    current_paper: u8,
    current_bright: bool,
    current_flash: bool,
    /// Plot pen position in Spectrum coords (origin at the bottom-left of
    /// the screen). Updated by PLOT and DRAW.
    pen_x: i32,
    pen_y: i32,
    /// Colour of the screen border (BORDER 0..7). Default 7 (white) to
    /// match Spectrum boot.
    current_border: u8,
    /// Lower-screen status line. On boot this holds the Spectrum copyright;
    /// after every immediate command or RUN it becomes the standard
    /// `<code> <message>, <line>:<stmt>` report. Cleared while the user is
    /// actively typing on the input row.
    status_line: String,
    /// Last line / statement RUN finished on, for the report.
    last_run_line: u16,
    /// Spectrum's lower screen is one line tall on boot (copyright only,
    /// no cursor) and grows to two lines as soon as the user starts
    /// typing. This flag flips on the first keystroke.
    started_typing: bool,
    /// Queue of `BEEP duration, pitch` requests, drained by the host's
    /// audio code. Each entry is `(duration_seconds, frequency_hz)`.
    pending_beeps: Vec<(f32, f32)>,
    /// Where the next RUN statement should execute. `Some` while a program
    /// is mid-flight, `None` when execution is idle (immediate mode).
    pc: Option<u16>,
    /// Set by the host when the user presses BREAK. Polled each statement
    /// by the RUN loop; clearing happens once the interrupt is reported.
    break_requested: bool,
    /// Number of host frames the RUN loop must wait before executing its
    /// next statement, because a `BEEP` is currently playing. Decremented
    /// by `frame()`. Matches Spectrum semantics where `BEEP d, p` blocks
    /// program execution for `d` seconds.
    beep_frames_remaining: u32,
    /// One-shot flag: when set, the host should cancel any sounds that are
    /// currently playing. Drained by `take_audio_cancel`.
    audio_cancel_requested: bool,
}

#[derive(Clone)]
struct UserFn {
    param: String,
    body: String,
}

struct ForFrame {
    var: String,
    end: f64,
    step: f64,
    /// Line number of the `FOR` statement. `NEXT` resumes execution at the
    /// first line strictly greater than this.
    return_line: u16,
}

struct PendingInput {
    var: String,
    after_line: u16,
}

impl System {
    pub fn new() -> Self {
        let mut display = Display::new();
        paint_boot_screen(&mut display);
        let mut sys = Self {
            display,
            input_line: String::new(),
            vars: HashMap::new(),
            program: Program::new(),
            prng: Cell::new(0x9E3779B97F4A7C15),
            for_stack: Vec::new(),
            gosub_stack: Vec::new(),
            user_fns: HashMap::new(),
            arrays: HashMap::new(),
            current_line: None,
            pending_input: None,
            current_ink: 0,
            current_paper: 7,
            current_bright: false,
            current_flash: false,
            pen_x: 0,
            pen_y: 0,
            current_border: 7,
            status_line: "\u{00A9} 1982 Sinclair Research Ltd".to_string(),
            last_run_line: 0,
            started_typing: false,
            pending_beeps: Vec::new(),
            pc: None,
            break_requested: false,
            beep_frames_remaining: 0,
            audio_cancel_requested: false,
        };
        sys.redraw_input();
        sys
    }

    fn current_attr(&self) -> u8 {
        make_attr(
            self.current_ink,
            self.current_paper,
            self.current_bright,
            self.current_flash,
        )
    }

    pub const FRAME_RGBA_LEN: usize = FRAME_RGBA_LEN;

    pub fn render_into(&self, out: &mut [u8]) {
        self.display.render_into(out);
    }

    pub fn frame(&mut self) {
        self.display.frame_advance();
        if self.beep_frames_remaining > 0 {
            self.beep_frames_remaining -= 1;
        }
        self.tick_run();
        // Keep the lower screen in sync with the runtime state: hide the
        // cursor while a program is still going, surface the final report
        // (or boot copyright) once it parks back at idle.
        self.redraw_input();
    }

    /// Whether a program is currently executing (between `RUN` and the
    /// completion / error / BREAK report). Used by hosts that drive
    /// execution explicitly (e.g. unit tests).
    pub fn is_running(&self) -> bool {
        self.pc.is_some()
    }

    pub fn feed_key(&mut self, key: Key) {
        match key {
            Key::Char(b) if (32..=126).contains(&b) => {
                self.started_typing = true;
                self.status_line.clear();
                if self.input_line.len() < 255 {
                    self.input_line.push(b as char);
                }
            }
            Key::Char(_) => {}
            Key::Backspace => {
                self.started_typing = true;
                self.status_line.clear();
                self.input_line.pop();
            }
            Key::Enter => {
                // Pressing Enter — even on an empty line — acknowledges any
                // standing status report and returns to a clean K-cursor
                // edit prompt. If the line had content, dispatch_input /
                // resolve_pending_input below will set a fresh status.
                self.started_typing = true;
                self.status_line.clear();
                let line = std::mem::take(&mut self.input_line);
                if let Some(pending) = self.pending_input.take() {
                    self.resolve_pending_input(pending, &line);
                } else {
                    self.dispatch_input(&line);
                }
            }
            Key::Break => {
                // Latched flag — RUN's per-statement loop notices it on
                // the next tick. Has no immediate effect outside RUN.
                self.break_requested = true;
            }
        }
        self.redraw_input();
    }

    fn redraw_input(&mut self) {
        // A running program (or a BEEP still playing out) owns the lower
        // screen — no cursor, no edit prompt. The status line stays blank
        // until tick_run publishes the final report.
        if self.pc.is_some() || self.beep_frames_remaining > 0 {
            self.display.print_input("", None);
            return;
        }
        // While there's a status message pending (boot copyright or last
        // report) the lower screen shows *only* that — no cursor, no input
        // — matching the Spectrum's one-line-or-the-other behaviour.
        if !self.status_line.is_empty() {
            self.display.print_input(&self.status_line, None);
            return;
        }
        let prompt = if self.pending_input.is_some() { "?" } else { "" };
        let combined = format!("{}{}", prompt, self.input_line);
        let chars: Vec<char> = combined.chars().collect();
        let visible: String = if chars.len() >= CHAR_W {
            chars[chars.len() - (CHAR_W - 1)..].iter().collect()
        } else {
            chars.iter().collect()
        };
        let cursor_col = visible.chars().count().min(CHAR_W - 1);
        self.display.print_input(&visible, Some(cursor_col));
    }

    fn set_report(&mut self, code: i32, message: &str, line: u16, stmt: u16) {
        // Spectrum's report format: a single-character code (0-9 → '0'-'9',
        // 10-21 → 'A'-'L'), then space, message, comma+space, line:stmt.
        let code_ch = if (0..10).contains(&code) {
            char::from(b'0' + code as u8)
        } else if (10..=21).contains(&code) {
            char::from(b'A' + (code as u8 - 10))
        } else {
            '?'
        };
        self.status_line = format!("{} {}, {}:{}", code_ch, message, line, stmt);
    }

    fn resolve_pending_input(&mut self, pending: PendingInput, raw: &str) {
        let parsed: Result<Value, ()> = if is_string_name(&pending.var) {
            Ok(Value::Str(raw.as_bytes().to_vec()))
        } else {
            // Spectrum: numeric INPUT evaluates the typed string as an
            // expression. Re-use evaluate_with against current vars (so
            // INPUT can reference other variables, just like real Spectrum).
            let env = self.env_view();
            expression::evaluate_with(raw, &env)
                .and_then(|v| v.as_num().map(Value::Num))
                .map_err(|_| ())
        };
        match parsed {
            Ok(v) => {
                self.vars.insert(pending.var, v);
                self.resume_run(pending.after_line);
            }
            Err(_) => {
                self.println_error("Nonsense in BASIC");
                // Drop back to Idle.
            }
        }
    }

    fn dispatch_input(&mut self, line: &str) {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return;
        }
        if trimmed.chars().next().map_or(false, |c| c.is_ascii_digit()) {
            self.store_program_line(trimmed);
            // Entering a program line leaves the status blank, like Spectrum.
            self.status_line.clear();
        } else {
            match self.execute_statement(trimmed) {
                StepResult::Ok => {
                    if self.pc.is_some() {
                        // RUN (or any command that armed the program
                        // counter) has only just kicked the program off.
                        // tick_run will publish the final "0 OK, …" status
                        // once the program actually finishes; until then
                        // keep the status line blank.
                        self.status_line.clear();
                    } else {
                        // Immediate command completed → "0 OK, 0:1" (the
                        // Spectrum's PPC is 0 in command mode, and SUBPPC
                        // ends at the statement *after* the one we just
                        // ran, so it's 1).
                        self.set_report(0, "OK", 0, 1);
                    }
                }
                StepResult::Stop => {
                    self.set_report(9, "STOP statement", 0, 1);
                }
                StepResult::Goto(_) | StepResult::AwaitInput => {
                    self.pending_input = None;
                    self.set_report(1, "Nonsense in BASIC", 0, 1);
                }
                StepResult::Error(msg) => {
                    self.set_report(1, &msg, 0, 1);
                }
            }
        }
    }

    fn store_program_line(&mut self, line: &str) {
        let (num_str, rest) = split_leading_number(line);
        let Ok(n) = num_str.parse::<u16>() else {
            self.println_error("Nonsense in BASIC");
            return;
        };
        let body = rest.trim_start().to_string();
        if !body.is_empty() {
            self.display.println(&format!("{} {}", n, body));
        }
        self.program.upsert(n, body);
    }

    /// Execute zero or more `:`-separated statements on a single source
    /// line. Stops early on Stop / Goto / Error / AwaitInput so those
    /// outcomes propagate to RUN.
    fn execute_statement(&mut self, line: &str) -> StepResult {
        let mut rest = line.trim();
        loop {
            if rest.is_empty() {
                return StepResult::Ok;
            }
            // IF and DEF FN both consume everything up to end-of-line,
            // including any `:` that would otherwise look like a separator
            // (the colons belong to the THEN body / the function body).
            let head_upper = first_word_uppercase(rest);
            let (this, next) = if head_upper == "IF" || head_upper == "DEF" {
                (rest, "")
            } else {
                split_top_level_colon(rest)
            };
            match self.execute_one(this) {
                StepResult::Ok => {}
                other => return other,
            }
            rest = next.trim_start();
        }
    }

    /// Dispatch a single statement (no further `:` splitting).
    fn execute_one(&mut self, stmt: &str) -> StepResult {
        let stmt = stmt.trim();
        if stmt.is_empty() {
            return StepResult::Ok;
        }
        let (head, rest) = split_first_word(stmt);
        let head_upper = head.to_ascii_uppercase();
        match head_upper.as_str() {
            "PRINT" => self.cmd_print(rest),
            "LET" => self.cmd_let(rest),
            "GOTO" => self.cmd_goto(rest),
            "STOP" => StepResult::Stop,
            "NEW" => {
                self.program.clear();
                self.vars.clear();
                self.for_stack.clear();
                self.gosub_stack.clear();
                self.user_fns.clear();
                self.arrays.clear();
                self.pending_input = None;
                StepResult::Ok
            }
            "GOSUB" => self.cmd_gosub(rest),
            "RETURN" => self.cmd_return(),
            "DEF" => self.cmd_def(rest),
            "DIM" => self.cmd_dim(rest),
            "CLS" => {
                let attr = self.current_attr();
                self.display.cls(attr);
                StepResult::Ok
            }
            "LIST" => self.cmd_list(),
            "RUN" => self.cmd_run(rest),
            "IF" => self.cmd_if(rest),
            "FOR" => self.cmd_for(rest),
            "NEXT" => self.cmd_next(rest),
            "INPUT" => self.cmd_input(rest),
            "INK" => self.cmd_set_colour(rest, ColourKind::Ink),
            "PAPER" => self.cmd_set_colour(rest, ColourKind::Paper),
            "BRIGHT" => self.cmd_set_colour(rest, ColourKind::Bright),
            "FLASH" => self.cmd_set_colour(rest, ColourKind::Flash),
            "PLOT" => self.cmd_plot(rest),
            "DRAW" => self.cmd_draw(rest),
            "CIRCLE" => self.cmd_circle(rest),
            "BORDER" => self.cmd_border(rest),
            "BEEP" => self.cmd_beep(rest),
            _ => StepResult::Error("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_print(&mut self, args: &str) -> StepResult {
        // Items separated by `;` (no separator), `,` (TAB to half-screen),
        // or `'` (newline). A `AT row, col;` or `TAB n;` item moves the
        // print cursor before the next item. Trailing separator means no
        // newline; otherwise PRINT ends with a newline.
        let attr = self.current_attr();
        if args.trim().is_empty() {
            self.display.print_with_attr("\n", attr);
            return StepResult::Ok;
        }
        let items = split_print_items(args);
        let mut produced_value = false;
        for item in &items {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }
            let upper = item.to_ascii_uppercase();
            if let Some(rest) = upper.strip_prefix("AT") {
                // AT row, col
                if !rest.starts_with(|c: char| c.is_ascii_whitespace() || c == '(') {
                    return self.print_value_item(item, attr).map(|_| {
                        produced_value = true;
                    }).err().map(StepResult::Error)
                        .unwrap_or(StepResult::Ok);
                }
                let coord_src = &item[2..];
                let Some((row_src, col_src)) = coord_src.split_once(',') else {
                    return StepResult::Error("Nonsense in BASIC".to_string());
                };
                let env = SysEnv { vars: &self.vars, prng: &self.prng, user_fns: &self.user_fns, arrays: &self.arrays };
                let row = expression::evaluate_with(row_src, &env).and_then(|v| v.as_num());
                let col = expression::evaluate_with(col_src, &env).and_then(|v| v.as_num());
                match (row, col) {
                    (Ok(r), Ok(c)) => {
                        self.display.set_print_cursor(c as usize, r as usize);
                    }
                    _ => return StepResult::Error("Nonsense in BASIC".to_string()),
                }
                continue;
            }
            if let Some(rest) = upper.strip_prefix("TAB") {
                if !rest.starts_with(|c: char| c.is_ascii_whitespace() || c == '(') {
                    if let Err(e) = self.print_value_item(item, attr) {
                        return StepResult::Error(e);
                    }
                    produced_value = true;
                    continue;
                }
                let n_src = &item[3..];
                let env = SysEnv { vars: &self.vars, prng: &self.prng, user_fns: &self.user_fns, arrays: &self.arrays };
                let Ok(n) = expression::evaluate_with(n_src, &env).and_then(|v| v.as_num()) else {
                    return StepResult::Error("Nonsense in BASIC".to_string());
                };
                let target_col = (n as usize) % CHAR_W;
                let (cur_col, cur_row) = self.display.print_cursor();
                if target_col >= cur_col {
                    let pad: String = " ".repeat(target_col - cur_col);
                    self.display.print_with_attr(&pad, attr);
                } else {
                    // TAB to a column we've passed → newline and move to it.
                    self.display.print_with_attr("\n", attr);
                    let _ = cur_row;
                    let pad: String = " ".repeat(target_col);
                    self.display.print_with_attr(&pad, attr);
                }
                continue;
            }
            // Otherwise evaluate as an expression and print its value.
            if let Err(e) = self.print_value_item(item, attr) {
                return StepResult::Error(e);
            }
            produced_value = true;
        }
        // Decide whether to append a newline. If the user ended with a
        // separator (last char of args trimmed-right is `;` or `,`), no
        // newline; otherwise newline.
        let trimmed_end = args.trim_end();
        let ends_in_sep = trimmed_end.ends_with(';') || trimmed_end.ends_with(',');
        if !ends_in_sep && produced_value {
            self.display.print_with_attr("\n", attr);
        }
        StepResult::Ok
    }

    fn print_value_item(&mut self, src: &str, attr: u8) -> Result<(), String> {
        let env = SysEnv { vars: &self.vars, prng: &self.prng, user_fns: &self.user_fns, arrays: &self.arrays };
        match expression::evaluate_with(src, &env) {
            Ok(v) => {
                self.display.print_with_attr(&format_value(&v), attr);
                Ok(())
            }
            Err(_) => Err("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_set_colour(&mut self, args: &str, kind: ColourKind) -> StepResult {
        let env = SysEnv { vars: &self.vars, prng: &self.prng, user_fns: &self.user_fns, arrays: &self.arrays };
        let Ok(n) = expression::evaluate_with(args, &env).and_then(|v| v.as_num()) else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let n = n as i32;
        match kind {
            ColourKind::Ink => {
                if !(0..=7).contains(&n) {
                    return StepResult::Error("Integer out of range".to_string());
                }
                self.current_ink = n as u8;
            }
            ColourKind::Paper => {
                if !(0..=7).contains(&n) {
                    return StepResult::Error("Integer out of range".to_string());
                }
                self.current_paper = n as u8;
            }
            ColourKind::Bright => {
                if !(0..=1).contains(&n) {
                    return StepResult::Error("Integer out of range".to_string());
                }
                self.current_bright = n == 1;
            }
            ColourKind::Flash => {
                if !(0..=1).contains(&n) {
                    return StepResult::Error("Integer out of range".to_string());
                }
                self.current_flash = n == 1;
            }
        }
        StepResult::Ok
    }

    /// Drain the audio-cancel flag. `true` means the host should stop any
    /// currently playing sounds (called by JS after BREAK).
    pub fn take_audio_cancel(&mut self) -> bool {
        std::mem::replace(&mut self.audio_cancel_requested, false)
    }

    fn cmd_beep(&mut self, args: &str) -> StepResult {
        // BEEP <duration>, <pitch>
        // pitch is semitones from middle C (C4 = 0). Frequency:
        //   f = 440 * 2 ^ ((pitch - 9) / 12)
        // because A4 = 440 Hz sits 9 semitones above C4.
        let parts: Vec<&str> = args.splitn(2, ',').collect();
        if parts.len() != 2 {
            return StepResult::Error("Nonsense in BASIC".to_string());
        }
        let env = self.env_view();
        let dur = expression::evaluate_with(parts[0], &env).and_then(|v| v.as_num());
        let pitch = expression::evaluate_with(parts[1], &env).and_then(|v| v.as_num());
        let (dur, pitch) = match (dur, pitch) {
            (Ok(d), Ok(p)) => (d, p),
            _ => return StepResult::Error("Nonsense in BASIC".to_string()),
        };
        if !(0.0..=120.0).contains(&dur) {
            return StepResult::Error("Integer out of range".to_string());
        }
        let freq = 440.0 * 2f64.powf((pitch - 9.0) / 12.0);
        self.pending_beeps.push((dur as f32, freq as f32));
        // BEEP is blocking on the Spectrum: the next statement only runs
        // after the tone has finished. Approximate that with a frame-count
        // gate (rAF ≈ 60 Hz, close enough to Spectrum's 50 Hz interrupt).
        let frames = (dur * 60.0).ceil() as u32 + 1;
        self.beep_frames_remaining = self.beep_frames_remaining.saturating_add(frames);
        StepResult::Ok
    }

    /// Drain queued BEEP requests for the host to play. Each pair is
    /// `(duration_seconds, frequency_hz)`.
    pub fn drain_beeps(&mut self) -> Vec<(f32, f32)> {
        std::mem::take(&mut self.pending_beeps)
    }

    fn cmd_border(&mut self, args: &str) -> StepResult {
        let env = self.env_view();
        let Ok(n) = expression::evaluate_with(args, &env).and_then(|v| v.as_num()) else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let n = n as i32;
        if !(0..=7).contains(&n) {
            return StepResult::Error("Integer out of range".to_string());
        }
        let n = n as u8;
        self.current_border = n;
        // Build bordcr the same way 08_command.asm:1833 does: paper = N,
        // ink = 7 (white) for dark borders 0..3, ink = 0 (black) for light
        // borders 4..7. The lower screen and the post-RUN report repaint
        // in that colour.
        let paper_bits = n << 3;
        let bordcr = if n >= 4 { paper_bits } else { paper_bits | 0x07 };
        self.display.set_lower_attr(bordcr);
        StepResult::Ok
    }

    /// RGB triple of the current screen border, for the host UI to render
    /// around the canvas.
    pub fn border_rgb(&self) -> [u8; 3] {
        crate::display::spectrum_palette(self.current_border, false)
    }

    fn cmd_plot(&mut self, args: &str) -> StepResult {
        let Some((x_src, y_src)) = args.split_once(',') else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let env = SysEnv { vars: &self.vars, prng: &self.prng, user_fns: &self.user_fns, arrays: &self.arrays };
        let x = expression::evaluate_with(x_src, &env).and_then(|v| v.as_num());
        let y = expression::evaluate_with(y_src, &env).and_then(|v| v.as_num());
        match (x, y) {
            (Ok(x), Ok(y)) => {
                let x = x as i32;
                let y = y as i32;
                let attr = self.current_attr();
                self.display.plot(x, y, true, attr);
                self.pen_x = x;
                self.pen_y = y;
                StepResult::Ok
            }
            _ => StepResult::Error("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_draw(&mut self, args: &str) -> StepResult {
        // DRAW dx, dy  — Spectrum's "arc" third arg is not yet supported.
        let Some((dx_src, dy_src)) = args.split_once(',') else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let env = SysEnv { vars: &self.vars, prng: &self.prng, user_fns: &self.user_fns, arrays: &self.arrays };
        let dx = expression::evaluate_with(dx_src, &env).and_then(|v| v.as_num());
        let dy = expression::evaluate_with(dy_src, &env).and_then(|v| v.as_num());
        match (dx, dy) {
            (Ok(dx), Ok(dy)) => {
                let x1 = self.pen_x + dx as i32;
                let y1 = self.pen_y + dy as i32;
                let attr = self.current_attr();
                self.display.draw_line(self.pen_x, self.pen_y, x1, y1, attr);
                self.pen_x = x1;
                self.pen_y = y1;
                StepResult::Ok
            }
            _ => StepResult::Error("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_circle(&mut self, args: &str) -> StepResult {
        // CIRCLE x, y, r
        let parts: Vec<&str> = args.splitn(3, ',').collect();
        if parts.len() != 3 {
            return StepResult::Error("Nonsense in BASIC".to_string());
        }
        let env = SysEnv { vars: &self.vars, prng: &self.prng, user_fns: &self.user_fns, arrays: &self.arrays };
        let x = expression::evaluate_with(parts[0], &env).and_then(|v| v.as_num());
        let y = expression::evaluate_with(parts[1], &env).and_then(|v| v.as_num());
        let r = expression::evaluate_with(parts[2], &env).and_then(|v| v.as_num());
        match (x, y, r) {
            (Ok(x), Ok(y), Ok(r)) => {
                let attr = self.current_attr();
                self.display.draw_circle(x as i32, y as i32, r as i32, attr);
                self.pen_x = x as i32;
                self.pen_y = y as i32;
                StepResult::Ok
            }
            _ => StepResult::Error("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_let(&mut self, args: &str) -> StepResult {
        let Some(eq_pos) = find_top_level_assignment_eq(args) else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let lhs_raw = args[..eq_pos].trim();
        let rhs = &args[eq_pos + 1..];

        // Array-element assignment: LET A(i) = expr
        if let Some(open_idx) = lhs_raw.find('(') {
            let name = normalise_var_name(lhs_raw[..open_idx].trim());
            if !is_valid_var_name(&name) {
                return StepResult::Error("Nonsense in BASIC".to_string());
            }
            let inside = &lhs_raw[open_idx + 1..];
            let Some(close_idx) = inside.rfind(')') else {
                return StepResult::Error("Nonsense in BASIC".to_string());
            };
            let idx_src = &inside[..close_idx];
            let (idx, value) = {
                let env = self.env_view();
                let idx = expression::evaluate_with(idx_src, &env).and_then(|v| v.as_num());
                let value = expression::evaluate_with(rhs, &env).and_then(|v| v.as_num());
                (idx, value)
            };
            let (idx, value) = match (idx, value) {
                (Ok(i), Ok(v)) => (i, v),
                _ => return StepResult::Error("Nonsense in BASIC".to_string()),
            };
            let Some(arr) = self.arrays.get_mut(&name) else {
                return StepResult::Error("Subscript wrong".to_string());
            };
            let i = idx as i64;
            if i < 1 || (i as usize) > arr.len() {
                return StepResult::Error("Subscript wrong".to_string());
            }
            arr[i as usize - 1] = value;
            return StepResult::Ok;
        }

        // Scalar assignment.
        let name = normalise_var_name(lhs_raw);
        if name.is_empty() || !is_valid_var_name(&name) {
            return StepResult::Error("Nonsense in BASIC".to_string());
        }
        let env = self.env_view();
        match expression::evaluate_with(rhs, &env) {
            Ok(v) => {
                let typed_ok = matches!(
                    (&v, is_string_name(&name)),
                    (Value::Str(_), true) | (Value::Num(_), false)
                );
                if !typed_ok {
                    return StepResult::Error("Nonsense in BASIC".to_string());
                }
                self.vars.insert(name, v);
                StepResult::Ok
            }
            Err(_) => StepResult::Error("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_dim(&mut self, args: &str) -> StepResult {
        let args = args.trim();
        let Some(open_idx) = args.find('(') else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let name = normalise_var_name(args[..open_idx].trim());
        if !is_valid_var_name(&name) || is_string_name(&name) {
            return StepResult::Error("Nonsense in BASIC".to_string());
        }
        let inside = &args[open_idx + 1..];
        let Some(close_idx) = inside.rfind(')') else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let size_src = &inside[..close_idx];
        let n = {
            let env = self.env_view();
            expression::evaluate_with(size_src, &env).and_then(|v| v.as_num())
        };
        let Ok(n) = n else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let n = n as i64;
        if n < 1 || n > 10_000 {
            return StepResult::Error("Subscript wrong".to_string());
        }
        self.arrays.insert(name, vec![0.0; n as usize]);
        StepResult::Ok
    }

    fn cmd_goto(&mut self, args: &str) -> StepResult {
        let env = SysEnv { vars: &self.vars, prng: &self.prng, user_fns: &self.user_fns, arrays: &self.arrays };
        let v = match expression::evaluate_with(args, &env) {
            Ok(v) => match v.as_num() {
                Ok(n) => n,
                Err(_) => return StepResult::Error("Nonsense in BASIC".to_string()),
            },
            Err(_) => return StepResult::Error("Nonsense in BASIC".to_string()),
        };
        if !(0.0..=65535.0).contains(&v) {
            return StepResult::Error("Integer out of range".to_string());
        }
        StepResult::Goto(v as u16)
    }

    fn cmd_list(&mut self) -> StepResult {
        let snapshot: Vec<(u16, String)> = self
            .program
            .iter()
            .map(|(n, s)| (n, s.to_string()))
            .collect();
        for (n, text) in snapshot {
            let line = format!("{} {}", n, text);
            self.display.println(&line);
        }
        StepResult::Ok
    }

    fn cmd_run(&mut self, args: &str) -> StepResult {
        self.vars.clear();
        self.for_stack.clear();
        self.gosub_stack.clear();
        self.arrays.clear();
        self.pending_input = None;
        let start = if args.trim().is_empty() {
            0u16
        } else {
            let env = SysEnv { vars: &self.vars, prng: &self.prng, user_fns: &self.user_fns, arrays: &self.arrays };
            match expression::evaluate_with(args, &env).and_then(|v| v.as_num()) {
                Ok(v) if (0.0..=65535.0).contains(&v) => v as u16,
                _ => return StepResult::Error("Nonsense in BASIC".to_string()),
            }
        };
        // Don't execute synchronously — just arm the program counter. The
        // host's frame() drives execution in chunks so the browser event
        // loop can interleave BREAK keypresses, audio, and rendering.
        self.pc = self.program.next_at_or_after(start);
        self.break_requested = false;
        StepResult::Ok
    }

    /// Resume a suspended RUN at `from_line` (smallest existing line ≥ this).
    /// Called by `feed_key` after `INPUT` has been satisfied.
    fn resume_run(&mut self, from_line: u16) {
        self.pc = self.program.next_at_or_after(from_line);
    }

    /// Execute up to [`STATEMENTS_PER_FRAME`] BASIC statements. Called once
    /// per host frame. Honours BREAK after every statement (matching
    /// `08_command.asm:378` which calls `break_key` per `stmt_ret`).
    fn tick_run(&mut self) {
        if self.pc.is_none() {
            // Clear any stray BREAK request that arrived while idle.
            self.break_requested = false;
            return;
        }
        // While a BEEP is in flight we still listen for BREAK — it must
        // interrupt the tone immediately, like Caps+Space on real hardware.
        if self.beep_frames_remaining > 0 {
            if self.break_requested {
                let line_no = self.pc.unwrap_or(0);
                self.set_report(13, "BREAK into program", line_no, 1);
                self.pc = None;
                self.current_line = None;
                self.break_requested = false;
                self.pending_beeps.clear();
                self.beep_frames_remaining = 0;
                self.audio_cancel_requested = true;
            }
            return;
        }
        for _ in 0..STATEMENTS_PER_FRAME {
            let Some(line_no) = self.pc else { return };
            if self.break_requested {
                self.set_report(13, "BREAK into program", line_no, 1);
                self.pc = None;
                self.current_line = None;
                self.break_requested = false;
                self.pending_beeps.clear();
                self.beep_frames_remaining = 0;
                self.audio_cancel_requested = true;
                return;
            }
            // If the statement we just executed enqueued a BEEP, stop
            // pulling more statements until the tone has played out.
            if self.beep_frames_remaining > 0 {
                return;
            }
            self.last_run_line = line_no;
            self.current_line = Some(line_no);
            let stmt = self
                .program
                .get(line_no)
                .map(str::to_string)
                .unwrap_or_default();
            match self.execute_statement(&stmt) {
                StepResult::Ok => {
                    self.pc = self.program.next_at_or_after(line_no.saturating_add(1));
                    if self.pc.is_none() {
                        self.set_report(0, "OK", line_no, 1);
                        self.current_line = None;
                        return;
                    }
                }
                StepResult::Stop => {
                    self.set_report(9, "STOP statement", line_no, 1);
                    self.pc = None;
                    self.current_line = None;
                    return;
                }
                StepResult::Goto(n) => {
                    self.pc = self.program.next_at_or_after(n);
                    if self.pc.is_none() {
                        self.set_report(0, "OK", line_no, 1);
                        self.current_line = None;
                        return;
                    }
                }
                StepResult::AwaitInput => {
                    // pending_input was set; suspend until the user presses
                    // Enter to satisfy it. resume_run reopens self.pc.
                    self.pc = None;
                    self.current_line = None;
                    return;
                }
                StepResult::Error(msg) => {
                    self.set_report(1, &msg, line_no, 1);
                    self.pc = None;
                    self.current_line = None;
                    return;
                }
            }
        }
        // Budget for this frame exhausted; program continues next tick.
    }

    fn cmd_if(&mut self, args: &str) -> StepResult {
        let parsed = {
            let env = self.env_view();
            expression::evaluate_until_keyword(args, "THEN", &env)
        };
        match parsed {
            Ok((cond, rest)) => {
                let truthy = match cond.as_num() {
                    Ok(n) => n != 0.0,
                    Err(_) => return StepResult::Error("Nonsense in BASIC".to_string()),
                };
                if truthy {
                    let rest_owned = rest.trim().to_string();
                    if rest_owned.is_empty() {
                        return StepResult::Error("Nonsense in BASIC".to_string());
                    }
                    self.execute_statement(&rest_owned)
                } else {
                    StepResult::Ok
                }
            }
            Err(_) => StepResult::Error("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_for(&mut self, args: &str) -> StepResult {
        // FOR I = start TO end [STEP step]
        let Some((lhs, after_eq)) = args.split_once('=') else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let var = normalise_var_name(lhs.trim());
        if !is_valid_var_name(&var) || is_string_name(&var) {
            return StepResult::Error("Nonsense in BASIC".to_string());
        }
        // Split on TO (whole word, case-insensitive).
        let Some((start_src, after_to)) = split_whole_word_ci(after_eq, "TO") else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let (end_src, step_src) = match split_whole_word_ci(after_to, "STEP") {
            Some((e, s)) => (e, Some(s)),
            None => (after_to, None),
        };
        let triple = {
            let env = SysEnv { vars: &self.vars, prng: &self.prng, user_fns: &self.user_fns, arrays: &self.arrays };
            let s = expression::evaluate_with(start_src, &env).and_then(|v| v.as_num());
            let e = expression::evaluate_with(end_src, &env).and_then(|v| v.as_num());
            let st = match step_src {
                Some(src) => expression::evaluate_with(src, &env).and_then(|v| v.as_num()),
                None => Ok(1.0),
            };
            match (s, e, st) {
                (Ok(s), Ok(e), Ok(st)) => Some((s, e, st)),
                _ => None,
            }
        };
        let Some((start, end, step)) = triple else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        if step == 0.0 {
            return StepResult::Error("Nonsense in BASIC".to_string());
        }
        self.vars.insert(var.clone(), Value::Num(start));
        let return_line = self.current_line.unwrap_or(0);
        self.for_stack.push(ForFrame {
            var,
            end,
            step,
            return_line,
        });
        StepResult::Ok
    }

    fn cmd_next(&mut self, args: &str) -> StepResult {
        let want = normalise_var_name(args.trim());
        let idx = if want.is_empty() {
            self.for_stack.len().checked_sub(1)
        } else {
            self.for_stack.iter().rposition(|f| f.var == want)
        };
        let Some(idx) = idx else {
            return StepResult::Error("NEXT without FOR".to_string());
        };
        let (var_name, end, step, return_line) = {
            let f = &self.for_stack[idx];
            (f.var.clone(), f.end, f.step, f.return_line)
        };
        let current = match self.vars.get(&var_name) {
            Some(Value::Num(n)) => *n,
            _ => return StepResult::Error("Nonsense in BASIC".to_string()),
        };
        let new_val = current + step;
        self.vars.insert(var_name, Value::Num(new_val));
        let done = (step > 0.0 && new_val > end) || (step < 0.0 && new_val < end);
        if done {
            // Drop this frame and any nested frames above it.
            self.for_stack.truncate(idx);
            StepResult::Ok
        } else {
            // Resume on the first line strictly after the FOR.
            StepResult::Goto(return_line.saturating_add(1))
        }
    }

    fn cmd_gosub(&mut self, args: &str) -> StepResult {
        let env = self.env_view();
        let target = match expression::evaluate_with(args, &env).and_then(|v| v.as_num()) {
            Ok(v) if (0.0..=65535.0).contains(&v) => v as u16,
            _ => return StepResult::Error("Nonsense in BASIC".to_string()),
        };
        // Push the line we're calling FROM (so RETURN can resume at line+1).
        let caller = self.current_line.unwrap_or(0);
        self.gosub_stack.push(caller);
        StepResult::Goto(target)
    }

    fn cmd_return(&mut self) -> StepResult {
        let Some(caller) = self.gosub_stack.pop() else {
            return StepResult::Error("RETURN without GOSUB".to_string());
        };
        StepResult::Goto(caller.saturating_add(1))
    }

    fn cmd_def(&mut self, args: &str) -> StepResult {
        // DEF FN <name>(<param>) = <expr>
        let upper = args.trim_start().to_ascii_uppercase();
        let rest = match upper.strip_prefix("FN") {
            Some(r) if r.starts_with(|c: char| c.is_ascii_whitespace()) => {
                &args.trim_start()[2..]
            }
            _ => return StepResult::Error("Nonsense in BASIC".to_string()),
        };
        let rest = rest.trim_start();
        let (name_part, after_name) = split_identifier(rest);
        let name = name_part.to_ascii_uppercase();
        if name.is_empty() {
            return StepResult::Error("Nonsense in BASIC".to_string());
        }
        let after = after_name.trim_start();
        // `(param)`
        let Some(after_open) = after.strip_prefix('(') else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let Some(close_idx) = after_open.find(')') else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let param = after_open[..close_idx].trim().to_ascii_uppercase();
        if !is_valid_var_name(&param) {
            return StepResult::Error("Nonsense in BASIC".to_string());
        }
        let after_paren = after_open[close_idx + 1..].trim_start();
        let Some(body) = after_paren.strip_prefix('=') else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let body = body.trim().to_string();
        if body.is_empty() {
            return StepResult::Error("Nonsense in BASIC".to_string());
        }
        self.user_fns.insert(name, UserFn { param, body });
        StepResult::Ok
    }

    fn env_view(&self) -> SysEnv<'_> {
        SysEnv {
            vars: &self.vars,
            prng: &self.prng,
            user_fns: &self.user_fns,
            arrays: &self.arrays,
        }
    }

    fn cmd_input(&mut self, args: &str) -> StepResult {
        // INPUT <var>
        let var = normalise_var_name(args.trim());
        if !is_valid_var_name(&var) {
            return StepResult::Error("Nonsense in BASIC".to_string());
        }
        // If we're running, suspend at the *next* line; if we're immediate,
        // there is no resume target.
        let after_line = match self.current_line {
            Some(n) => n.saturating_add(1),
            None => return StepResult::Error("Nonsense in BASIC".to_string()),
        };
        self.pending_input = Some(PendingInput { var, after_line });
        StepResult::AwaitInput
    }

    fn println_error(&mut self, msg: &str) {
        self.display.println(msg);
    }
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}

enum StepResult {
    Ok,
    Stop,
    Goto(u16),
    Error(String),
    /// The statement (only `INPUT` today) parked the RUN loop in
    /// `pending_input`. The loop must return without printing an error.
    AwaitInput,
}

enum ColourKind {
    Ink,
    Paper,
    Bright,
    Flash,
}

/// Split a PRINT argument list on top-level `;`. Comma is intentionally NOT
/// a separator here: in Spectrum BASIC `,` *is* the half-screen tab in PRINT,
/// but it also belongs to the inner arguments of `AT row,col` / `TAB n`.
/// Treating it as part of items lets `PRINT AT 5,10;"HI"` parse cleanly; the
/// half-screen tab semantics arrive in MVP-5 alongside a proper PRINT-item
/// parser. String literals are still respected.
fn split_print_items(src: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_str = false;
    for c in src.chars() {
        if c == '"' {
            in_str = !in_str;
            cur.push(c);
            continue;
        }
        if !in_str && c == ';' {
            out.push(std::mem::take(&mut cur));
            continue;
        }
        cur.push(c);
    }
    out.push(cur);
    out
}

/// Split `src` on the first occurrence of `kw` matched as a whole word
/// (case-insensitive), respecting double-quoted string literals. Returns
/// `Some((before, after))`.
fn split_whole_word_ci<'a>(src: &'a str, kw: &str) -> Option<(&'a str, &'a str)> {
    let upper = src.to_ascii_uppercase();
    let kw_up = kw.to_ascii_uppercase();
    let mut search_from = 0;
    loop {
        let rel = upper[search_from..].find(&kw_up)?;
        let abs = search_from + rel;
        let before_ok = abs == 0 || !src.as_bytes()[abs - 1].is_ascii_alphanumeric();
        let after_idx = abs + kw_up.len();
        let after_ok =
            after_idx >= src.len() || !src.as_bytes()[after_idx].is_ascii_alphanumeric();
        if before_ok && after_ok && !inside_string_literal(src, abs) {
            return Some((&src[..abs], &src[abs + kw_up.len()..]));
        }
        search_from = abs + 1;
    }
}

fn inside_string_literal(src: &str, pos: usize) -> bool {
    let mut in_str = false;
    for (i, &b) in src.as_bytes().iter().enumerate() {
        if i >= pos {
            return in_str;
        }
        if b == b'"' {
            in_str = !in_str;
        }
    }
    in_str
}

/// Read-only view of System's variables and PRNG, exposing them to the
/// expression evaluator as an [`Env`].
struct SysEnv<'a> {
    vars: &'a HashMap<String, Value>,
    prng: &'a Cell<u64>,
    user_fns: &'a HashMap<String, UserFn>,
    arrays: &'a HashMap<String, Vec<f64>>,
}
impl<'a> Env for SysEnv<'a> {
    fn get_var(&self, name: &str) -> Option<Value> {
        self.vars.get(name).cloned()
    }
    fn get_array(&self, name: &str, indices: &[f64]) -> Option<Value> {
        let arr = self.arrays.get(name)?;
        if indices.len() != 1 {
            return None;
        }
        let i = indices[0] as i64;
        if i < 1 || (i as usize) > arr.len() {
            return None;
        }
        Some(Value::Num(arr[i as usize - 1]))
    }
    fn call_user_fn(&self, name: &str, arg: Value) -> Option<Value> {
        let def = self.user_fns.get(name)?;
        let local = UserFnEnv {
            parent: self,
            param: &def.param,
            value: arg,
        };
        expression::evaluate_with(&def.body, &local).ok()
    }
    fn call_fn(&self, name: &str, args: &[Value]) -> Option<Value> {
        Some(match (name, args) {
            ("PI", []) => Value::Num(core::f64::consts::PI),
            ("RND", []) => Value::Num(rnd_next(self.prng)),
            ("SIN", [Value::Num(x)]) => Value::Num(x.sin()),
            ("COS", [Value::Num(x)]) => Value::Num(x.cos()),
            ("TAN", [Value::Num(x)]) => Value::Num(x.tan()),
            ("ASN", [Value::Num(x)]) => Value::Num(x.asin()),
            ("ACS", [Value::Num(x)]) => Value::Num(x.acos()),
            ("ATN", [Value::Num(x)]) => Value::Num(x.atan()),
            ("LN", [Value::Num(x)]) => Value::Num(x.ln()),
            ("EXP", [Value::Num(x)]) => Value::Num(x.exp()),
            ("INT", [Value::Num(x)]) => Value::Num(x.floor()),
            ("ABS", [Value::Num(x)]) => Value::Num(x.abs()),
            ("SQR", [Value::Num(x)]) => Value::Num(x.sqrt()),
            ("SGN", [Value::Num(x)]) => Value::Num(if *x > 0.0 {
                1.0
            } else if *x < 0.0 {
                -1.0
            } else {
                0.0
            }),
            ("LEN", [Value::Str(s)]) => Value::Num(s.len() as f64),
            ("CODE", [Value::Str(s)]) => Value::Num(s.first().copied().unwrap_or(0) as f64),
            ("CHR$", [Value::Num(n)]) => {
                let b = (*n as i64).rem_euclid(256) as u8;
                Value::Str(vec![b])
            }
            ("STR$", [Value::Num(n)]) => Value::Str(fp_format::format(*n).into_bytes()),
            ("VAL", [Value::Str(s)]) => {
                let src = core::str::from_utf8(s).ok()?;
                // VAL on a user-provided string runs with the same env (so
                // VAL "A+1" can reference user variables).
                return expression::evaluate_with(src, self).ok();
            }
            _ => return None,
        })
    }
}

/// Local scope wrapper for a single DEF FN call: the named parameter
/// shadows the outer environment for the duration of the body evaluation.
struct UserFnEnv<'a> {
    parent: &'a dyn Env,
    param: &'a str,
    value: Value,
}
impl<'a> Env for UserFnEnv<'a> {
    fn get_var(&self, name: &str) -> Option<Value> {
        if name == self.param {
            Some(self.value.clone())
        } else {
            self.parent.get_var(name)
        }
    }
    fn call_fn(&self, name: &str, args: &[Value]) -> Option<Value> {
        self.parent.call_fn(name, args)
    }
    fn call_user_fn(&self, name: &str, arg: Value) -> Option<Value> {
        self.parent.call_user_fn(name, arg)
    }
}

/// Find the byte index of the `=` that separates an assignment's LHS from
/// its RHS — at top level, i.e. not inside `(...)` or `"..."`.
fn find_top_level_assignment_eq(src: &str) -> Option<usize> {
    let bytes = src.as_bytes();
    let mut depth = 0i32;
    let mut in_str = false;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'"' {
            in_str = !in_str;
        } else if !in_str {
            if b == b'(' {
                depth += 1;
            } else if b == b')' {
                depth -= 1;
            } else if b == b'=' && depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

fn split_identifier(s: &str) -> (&str, &str) {
    let end = s
        .char_indices()
        .find(|(_, c)| !(c.is_ascii_alphanumeric() || *c == '$'))
        .map_or(s.len(), |(i, _)| i);
    (&s[..end], &s[end..])
}

fn rnd_next(state: &Cell<u64>) -> f64 {
    let mut s = state.get();
    s = s.wrapping_add(0x9E3779B97F4A7C15);
    state.set(s);
    let mut z = s;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
    z ^= z >> 31;
    ((z >> 11) as f64) / ((1u64 << 53) as f64)
}

/// Format a `Value` the way `PRINT` would: numbers via `fp_format`, strings
/// rendered as their bytes (with non-ASCII bytes shown as their UTF-8 source
/// — string identity round-trips for ASCII-only programs).
fn format_value(v: &Value) -> String {
    match v {
        Value::Num(n) => fp_format::format(*n),
        Value::Str(bytes) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn split_leading_number(s: &str) -> (&str, &str) {
    let end = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map_or(s.len(), |(i, _)| i);
    (&s[..end], &s[end..])
}

/// Return `(prefix, rest)` split on the first top-level `:` in `s`. Top
/// level means: outside `"..."` and outside `(...)`. If there's no such
/// colon, returns `(s, "")`.
fn split_top_level_colon(s: &str) -> (&str, &str) {
    let bytes = s.as_bytes();
    let mut depth: i32 = 0;
    let mut in_str = false;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'"' {
            in_str = !in_str;
        } else if !in_str {
            if b == b'(' {
                depth += 1;
            } else if b == b')' {
                depth -= 1;
            } else if b == b':' && depth == 0 {
                return (&s[..i], &s[i + 1..]);
            }
        }
    }
    (s, "")
}

/// First whitespace-delimited word of `s`, uppercased. Used for keyword
/// detection without slicing arguments.
fn first_word_uppercase(s: &str) -> String {
    let s = s.trim_start();
    let end = s
        .char_indices()
        .find(|(_, c)| c.is_ascii_whitespace())
        .map_or(s.len(), |(i, _)| i);
    s[..end].to_ascii_uppercase()
}

fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    let end = s
        .char_indices()
        .find(|(_, c)| c.is_ascii_whitespace())
        .map_or(s.len(), |(i, _)| i);
    (&s[..end], s[end..].trim_start())
}

fn normalise_var_name(raw: &str) -> String {
    let mut out = String::new();
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_uppercase());
        } else if c == '$' {
            out.push('$');
        } else {
            return String::new(); // invalid char
        }
    }
    out
}

fn is_valid_var_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    if !bytes[0].is_ascii_alphabetic() {
        return false;
    }
    // All but the last char must be alphanumeric. The last may also be `$`.
    let body_end = if name.ends_with('$') {
        bytes.len() - 1
    } else {
        bytes.len()
    };
    bytes[..body_end].iter().all(|b| b.is_ascii_alphanumeric())
}

fn paint_boot_screen(_d: &mut Display) {
    // The Spectrum boot copyright lives in the *lower screen*, not the
    // upper one. We paint it from `redraw_input` so it disappears
    // automatically the moment the user presses a key (the lower screen
    // gets repurposed as the edit area). Nothing to paint here.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed_str(sys: &mut System, s: &str) {
        for b in s.bytes() {
            sys.feed_key(Key::Char(b));
        }
    }
    fn enter(sys: &mut System) {
        sys.feed_key(Key::Enter);
    }
    /// Drive the frame-based RUN loop to completion (or until `max_frames`
    /// elapses, simulating BREAK after a long wait). Used by every test
    /// that issues RUN.
    fn drive(sys: &mut System) {
        for _ in 0..10_000 {
            if !sys.is_running() {
                return;
            }
            sys.frame();
        }
        panic!("program didn't finish in 10_000 frames");
    }

    fn num(sys: &System, name: &str) -> Option<f64> {
        sys.vars.get(name).and_then(|v| match v {
            Value::Num(n) => Some(*n),
            _ => None,
        })
    }

    fn s(sys: &System, name: &str) -> Option<String> {
        sys.vars.get(name).and_then(|v| match v {
            Value::Str(b) => Some(String::from_utf8_lossy(b).into_owned()),
            _ => None,
        })
    }

    #[test]
    fn let_then_print() {
        let mut sys = System::new();
        feed_str(&mut sys, "LET A=5");
        enter(&mut sys);
        feed_str(&mut sys, "PRINT A*2+1");
        enter(&mut sys);
        assert_eq!(num(&sys, "A"), Some(5.0));
    }

    #[test]
    fn store_and_list() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET A=1");
        enter(&mut sys);
        feed_str(&mut sys, "20 PRINT A");
        enter(&mut sys);
        let lines: Vec<_> = sys.program.iter().collect();
        assert_eq!(lines, vec![(10, "LET A=1"), (20, "PRINT A")]);
    }

    #[test]
    fn run_simple_program() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET A=2");
        enter(&mut sys);
        feed_str(&mut sys, "20 LET B=3");
        enter(&mut sys);
        feed_str(&mut sys, "30 PRINT A*B");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        drive(&mut sys);
        assert_eq!(num(&sys, "A"), Some(2.0));
        assert_eq!(num(&sys, "B"), Some(3.0));
    }

    #[test]
    fn goto_loops_terminate_via_stop() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET I=0");
        enter(&mut sys);
        feed_str(&mut sys, "20 LET I=I+1");
        enter(&mut sys);
        feed_str(&mut sys, "30 STOP");
        enter(&mut sys);
        feed_str(&mut sys, "40 GOTO 20");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        drive(&mut sys);
        assert_eq!(num(&sys, "I"), Some(1.0));
    }

    #[test]
    fn break_interrupts_infinite_loop() {
        // The Spectrum has no step limit — BREAK is the only way out of a
        // tight loop. We exercise that: tick a few frames, raise BREAK,
        // tick one more, expect the loop to stop with a fat counter and
        // a "BREAK into program" report.
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET I=0");
        enter(&mut sys);
        feed_str(&mut sys, "20 LET I=I+1");
        enter(&mut sys);
        feed_str(&mut sys, "30 GOTO 20");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        // Let the loop spin for a couple of frame budgets, then break.
        for _ in 0..3 { sys.frame(); }
        sys.feed_key(Key::Break);
        sys.frame();
        assert!(!sys.is_running(), "BREAK should have stopped the loop");
        let i = num(&sys, "I").unwrap();
        assert!(i > 1_000.0, "expected many iterations before BREAK, got {}", i);
    }

    #[test]
    fn new_clears_program_and_vars() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET A=1");
        enter(&mut sys);
        feed_str(&mut sys, "LET B=2");
        enter(&mut sys);
        feed_str(&mut sys, "NEW");
        enter(&mut sys);
        assert!(sys.program.is_empty());
        assert!(sys.vars.is_empty());
    }

    #[test]
    fn if_then_branches() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET A=5");
        enter(&mut sys);
        feed_str(&mut sys, "20 IF A>3 THEN LET B=99");
        enter(&mut sys);
        feed_str(&mut sys, "30 IF A<3 THEN LET B=42");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        drive(&mut sys);
        assert_eq!(num(&sys, "B"), Some(99.0));
    }

    #[test]
    fn builtins_resolve_in_print() {
        let mut sys = System::new();
        feed_str(&mut sys, "LET A=INT 3.7");
        enter(&mut sys);
        assert_eq!(num(&sys, "A"), Some(3.0));
        feed_str(&mut sys, "LET B=ABS -5");
        enter(&mut sys);
        assert_eq!(num(&sys, "B"), Some(5.0));
        feed_str(&mut sys, "LET C=SQR 16");
        enter(&mut sys);
        assert_eq!(num(&sys, "C"), Some(4.0));
    }

    #[test]
    fn rnd_produces_unit_interval() {
        let mut sys = System::new();
        for _ in 0..50 {
            feed_str(&mut sys, "LET R=RND");
            enter(&mut sys);
            let r = num(&sys, "R").unwrap();
            assert!((0.0..1.0).contains(&r), "RND out of range: {}", r);
        }
    }

    #[test]
    fn empty_text_deletes_program_line() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET A=1");
        enter(&mut sys);
        feed_str(&mut sys, "10");
        enter(&mut sys);
        assert!(sys.program.is_empty());
    }

    #[test]
    fn string_variable_and_concat() {
        let mut sys = System::new();
        feed_str(&mut sys, "LET A$=\"hello\"");
        enter(&mut sys);
        feed_str(&mut sys, "LET B$=A$+\" world\"");
        enter(&mut sys);
        assert_eq!(s(&sys, "A$").as_deref(), Some("hello"));
        assert_eq!(s(&sys, "B$").as_deref(), Some("hello world"));
    }

    #[test]
    fn type_mismatched_let_rejected() {
        let mut sys = System::new();
        feed_str(&mut sys, "LET A$=5");
        enter(&mut sys);
        assert!(sys.vars.get("A$").is_none());
        feed_str(&mut sys, "LET A=\"hi\"");
        enter(&mut sys);
        assert!(sys.vars.get("A").is_none());
    }

    #[test]
    fn multi_statement_colon() {
        let mut sys = System::new();
        // Three colon-separated statements on one immediate line.
        feed_str(&mut sys, "LET A=10: LET B=20: LET C=A+B");
        enter(&mut sys);
        assert_eq!(num(&sys, "A"), Some(10.0));
        assert_eq!(num(&sys, "B"), Some(20.0));
        assert_eq!(num(&sys, "C"), Some(30.0));
    }

    #[test]
    fn multi_statement_inside_program_line() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET A=1: LET B=2: LET C=A+B");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        drive(&mut sys);
        assert_eq!(num(&sys, "C"), Some(3.0));
    }

    #[test]
    fn dim_then_index_assign() {
        let mut sys = System::new();
        feed_str(&mut sys, "DIM A(5)");
        enter(&mut sys);
        feed_str(&mut sys, "LET A(1)=10");
        enter(&mut sys);
        feed_str(&mut sys, "LET A(5)=50");
        enter(&mut sys);
        feed_str(&mut sys, "LET B=A(1)+A(5)");
        enter(&mut sys);
        assert_eq!(num(&sys, "B"), Some(60.0));
    }

    #[test]
    fn array_out_of_range_errors() {
        let mut sys = System::new();
        feed_str(&mut sys, "DIM A(3)");
        enter(&mut sys);
        feed_str(&mut sys, "LET A(0)=1");
        enter(&mut sys);
        // A unchanged because 0 is out of range (Spectrum is 1-indexed).
        assert_eq!(sys.arrays.get("A").map(|a| a[0]), Some(0.0));
    }

    #[test]
    fn dim_in_program_with_loop() {
        // Two-loop program: fill A(1..5)=I*I, then sum into S.
        let mut sys = System::new();
        feed_str(&mut sys, "10 DIM A(5)");
        enter(&mut sys);
        feed_str(&mut sys, "20 LET S=0");
        enter(&mut sys);
        feed_str(&mut sys, "30 FOR I=1 TO 5");
        enter(&mut sys);
        feed_str(&mut sys, "40 LET A(I)=I*I");
        enter(&mut sys);
        feed_str(&mut sys, "50 LET S=S+A(I)");
        enter(&mut sys);
        feed_str(&mut sys, "60 NEXT I");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        drive(&mut sys);
        // 1+4+9+16+25 = 55
        assert_eq!(num(&sys, "S"), Some(55.0));
    }

    #[test]
    fn gosub_return_visits_subroutine() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET A=0");
        enter(&mut sys);
        feed_str(&mut sys, "20 GOSUB 100");
        enter(&mut sys);
        feed_str(&mut sys, "30 STOP");
        enter(&mut sys);
        feed_str(&mut sys, "100 LET A=99");
        enter(&mut sys);
        feed_str(&mut sys, "110 RETURN");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        drive(&mut sys);
        assert_eq!(num(&sys, "A"), Some(99.0));
    }

    #[test]
    fn return_without_gosub_errors() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 RETURN");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        drive(&mut sys);
        // No assertion on display, just check we didn't panic.
        // RETURN without GOSUB → error printed, no infinite loop.
        assert!(sys.gosub_stack.is_empty());
    }

    #[test]
    fn def_fn_callable() {
        let mut sys = System::new();
        feed_str(&mut sys, "DEF FN F(X)=X*X+1");
        enter(&mut sys);
        feed_str(&mut sys, "LET A=FN F(5)");
        enter(&mut sys);
        assert_eq!(num(&sys, "A"), Some(26.0));
    }

    #[test]
    fn def_fn_local_scope() {
        // The parameter shadows any outer variable of the same name.
        let mut sys = System::new();
        feed_str(&mut sys, "LET X=100");
        enter(&mut sys);
        feed_str(&mut sys, "DEF FN G(X)=X+1");
        enter(&mut sys);
        feed_str(&mut sys, "LET A=FN G(7)");
        enter(&mut sys);
        assert_eq!(num(&sys, "A"), Some(8.0)); // not 101
        assert_eq!(num(&sys, "X"), Some(100.0)); // outer X intact
    }

    #[test]
    fn for_next_basic_loop() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET S=0");
        enter(&mut sys);
        feed_str(&mut sys, "20 FOR I=1 TO 5");
        enter(&mut sys);
        feed_str(&mut sys, "30 LET S=S+I");
        enter(&mut sys);
        feed_str(&mut sys, "40 NEXT I");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        drive(&mut sys);
        // 1+2+3+4+5 = 15
        assert_eq!(num(&sys, "S"), Some(15.0));
        // I is left at 6 after the loop terminates.
        assert_eq!(num(&sys, "I"), Some(6.0));
    }

    #[test]
    fn for_next_step() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET S=0");
        enter(&mut sys);
        feed_str(&mut sys, "20 FOR I=10 TO 2 STEP -2");
        enter(&mut sys);
        feed_str(&mut sys, "30 LET S=S+I");
        enter(&mut sys);
        feed_str(&mut sys, "40 NEXT I");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        drive(&mut sys);
        // 10+8+6+4+2 = 30
        assert_eq!(num(&sys, "S"), Some(30.0));
    }

    #[test]
    fn input_resumes_after_enter() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 INPUT N");
        enter(&mut sys);
        feed_str(&mut sys, "20 LET R=N*N");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        drive(&mut sys);
        // System should now be parked awaiting input.
        assert!(sys.pending_input.is_some(), "expected pending input");
        // User types 7.
        feed_str(&mut sys, "7");
        enter(&mut sys);
        // Resume execution after INPUT and finish line 20.
        drive(&mut sys);
        assert_eq!(num(&sys, "N"), Some(7.0));
        assert_eq!(num(&sys, "R"), Some(49.0));
        assert!(sys.pending_input.is_none());
    }

    #[test]
    fn input_string_variable() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 INPUT N$");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        drive(&mut sys);
        feed_str(&mut sys, "hello world");
        enter(&mut sys);
        drive(&mut sys);
        assert_eq!(s(&sys, "N$").as_deref(), Some("hello world"));
    }

    #[test]
    fn string_builtins() {
        let mut sys = System::new();
        feed_str(&mut sys, "LET A=LEN \"hello\"");
        enter(&mut sys);
        assert_eq!(num(&sys, "A"), Some(5.0));
        feed_str(&mut sys, "LET A=CODE \"A\"");
        enter(&mut sys);
        assert_eq!(num(&sys, "A"), Some(65.0));
        feed_str(&mut sys, "LET S$=CHR$ 65");
        enter(&mut sys);
        assert_eq!(s(&sys, "S$").as_deref(), Some("A"));
        feed_str(&mut sys, "LET S$=STR$ 42");
        enter(&mut sys);
        assert_eq!(s(&sys, "S$").as_deref(), Some("42"));
        feed_str(&mut sys, "LET A=VAL \"1+2*3\"");
        enter(&mut sys);
        assert_eq!(num(&sys, "A"), Some(7.0));
    }
}
