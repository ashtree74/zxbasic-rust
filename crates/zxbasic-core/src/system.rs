//! Single-owner state machine for the whole runtime.
//!
//! MVP-3c additions on top of MVP-3b: `FOR / NEXT / STEP` (with a loop
//! stack), and `INPUT` (with a state machine that suspends RUN until the
//! next Enter).

use std::cell::Cell;
use std::collections::HashMap;

use crate::display::{Display, CHAR_H, CHAR_W, FRAME_RGBA_LEN};
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
}

const RUN_STEP_LIMIT: usize = 100_000;

pub struct System {
    display: Display,
    input_line: String,
    vars: HashMap<String, Value>,
    program: Program,
    prng: Cell<u64>,
    for_stack: Vec<ForFrame>,
    /// Set while RUN is iterating a line; used by `FOR` to capture the
    /// matching return point for `NEXT`.
    current_line: Option<u16>,
    /// When `Some`, the next Enter binds the input line to `var` and resumes
    /// RUN at `after_line`. While set, the editor renders a `?` prompt.
    pending_input: Option<PendingInput>,
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
            current_line: None,
            pending_input: None,
        };
        sys.redraw_input();
        sys
    }

    pub const FRAME_RGBA_LEN: usize = FRAME_RGBA_LEN;

    pub fn render_into(&self, out: &mut [u8]) {
        self.display.render_into(out);
    }

    pub fn frame(&mut self) {}

    pub fn feed_key(&mut self, key: Key) {
        match key {
            Key::Char(b) if (32..=126).contains(&b) => {
                if self.input_line.len() < CHAR_W - 1 {
                    self.input_line.push(b as char);
                }
            }
            Key::Char(_) => {}
            Key::Backspace => {
                self.input_line.pop();
            }
            Key::Enter => {
                let line = std::mem::take(&mut self.input_line);
                if let Some(pending) = self.pending_input.take() {
                    self.resolve_pending_input(pending, &line);
                } else {
                    self.dispatch_input(&line);
                }
            }
        }
        self.redraw_input();
    }

    fn redraw_input(&mut self) {
        let prompt = if self.pending_input.is_some() { "?" } else { "" };
        let combined = format!("{}{}", prompt, self.input_line);
        let cursor_col = combined.chars().count().min(CHAR_W - 1);
        self.display.print_input(&combined, cursor_col);
    }

    fn resolve_pending_input(&mut self, pending: PendingInput, raw: &str) {
        let parsed: Result<Value, ()> = if is_string_name(&pending.var) {
            Ok(Value::Str(raw.as_bytes().to_vec()))
        } else {
            // Spectrum: numeric INPUT evaluates the typed string as an
            // expression. Re-use evaluate_with against current vars (so
            // INPUT can reference other variables, just like real Spectrum).
            let env = SysEnv {
                vars: &self.vars,
                prng: &self.prng,
            };
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
        } else {
            match self.execute_statement(trimmed) {
                StepResult::Ok | StepResult::Stop => {}
                StepResult::Goto(_) | StepResult::AwaitInput => {
                    // GOTO and INPUT outside of RUN are meaningless in our
                    // model. (Real Spectrum permits immediate INPUT in K
                    // mode; we'll add that with the full editor in MVP-6.)
                    self.pending_input = None;
                    self.println_error("Nonsense in BASIC");
                }
                StepResult::Error(msg) => self.println_error(&msg),
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

    fn execute_statement(&mut self, stmt: &str) -> StepResult {
        let stmt = stmt.trim();
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
                self.pending_input = None;
                StepResult::Ok
            }
            "CLS" => {
                self.display.clear();
                StepResult::Ok
            }
            "LIST" => self.cmd_list(),
            "RUN" => self.cmd_run(rest),
            "IF" => self.cmd_if(rest),
            "FOR" => self.cmd_for(rest),
            "NEXT" => self.cmd_next(rest),
            "INPUT" => self.cmd_input(rest),
            _ => StepResult::Error("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_print(&mut self, args: &str) -> StepResult {
        if args.trim().is_empty() {
            self.display.println("");
            return StepResult::Ok;
        }
        let env = SysEnv { vars: &self.vars, prng: &self.prng };
        match expression::evaluate_with(args, &env) {
            Ok(v) => {
                self.display.println(&format_value(&v));
                StepResult::Ok
            }
            Err(_) => StepResult::Error("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_let(&mut self, args: &str) -> StepResult {
        let Some((lhs, rhs)) = args.split_once('=') else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let name = normalise_var_name(lhs.trim());
        if name.is_empty() || !is_valid_var_name(&name) {
            return StepResult::Error("Nonsense in BASIC".to_string());
        }
        let env = SysEnv { vars: &self.vars, prng: &self.prng };
        match expression::evaluate_with(rhs, &env) {
            Ok(v) => {
                // Spectrum type rule: a string variable (name ends with `$`)
                // accepts only strings, and a numeric variable only numbers.
                let typed_ok = match (&v, is_string_name(&name)) {
                    (Value::Str(_), true) | (Value::Num(_), false) => true,
                    _ => false,
                };
                if !typed_ok {
                    return StepResult::Error("Nonsense in BASIC".to_string());
                }
                self.vars.insert(name, v);
                StepResult::Ok
            }
            Err(_) => StepResult::Error("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_goto(&mut self, args: &str) -> StepResult {
        let env = SysEnv { vars: &self.vars, prng: &self.prng };
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
        self.pending_input = None;
        let start = if args.trim().is_empty() {
            0u16
        } else {
            let env = SysEnv { vars: &self.vars, prng: &self.prng };
            match expression::evaluate_with(args, &env).and_then(|v| v.as_num()) {
                Ok(v) if (0.0..=65535.0).contains(&v) => v as u16,
                _ => return StepResult::Error("Nonsense in BASIC".to_string()),
            }
        };
        let pc = self.program.next_at_or_after(start);
        self.run_loop(pc);
        StepResult::Ok
    }

    /// Resume a suspended RUN at `from_line` (smallest existing line ≥ this).
    /// Called by `feed_key` after `INPUT` has been satisfied.
    fn resume_run(&mut self, from_line: u16) {
        let pc = self.program.next_at_or_after(from_line);
        self.run_loop(pc);
    }

    /// The actual statement-by-statement RUN loop. Reports its own errors
    /// directly into the display, and yields cleanly when an `INPUT`
    /// statement parks the system in `pending_input`.
    fn run_loop(&mut self, mut pc: Option<u16>) {
        let mut steps = 0usize;
        while let Some(line_no) = pc {
            if steps >= RUN_STEP_LIMIT {
                self.println_error("D BREAK - CONT repeats");
                self.current_line = None;
                return;
            }
            steps += 1;
            self.current_line = Some(line_no);
            let stmt = self
                .program
                .get(line_no)
                .map(str::to_string)
                .unwrap_or_default();
            match self.execute_statement(&stmt) {
                StepResult::Ok => {
                    pc = self.program.next_at_or_after(line_no.saturating_add(1));
                }
                StepResult::Stop => {
                    self.current_line = None;
                    return;
                }
                StepResult::Goto(n) => {
                    pc = self.program.next_at_or_after(n);
                }
                StepResult::AwaitInput => {
                    // cmd_input set self.pending_input already.
                    self.current_line = None;
                    return;
                }
                StepResult::Error(msg) => {
                    self.println_error(&format!("{}, {}:1", msg, line_no));
                    self.current_line = None;
                    return;
                }
            }
        }
        self.current_line = None;
    }

    fn cmd_if(&mut self, args: &str) -> StepResult {
        let parsed = {
            let env = SysEnv {
                vars: &self.vars,
                prng: &self.prng,
            };
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
            let env = SysEnv { vars: &self.vars, prng: &self.prng };
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
}
impl<'a> Env for SysEnv<'a> {
    fn get_var(&self, name: &str) -> Option<Value> {
        self.vars.get(name).cloned()
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

fn paint_boot_screen(d: &mut Display) {
    let line_a = "(c) 2026 zxbasic-rust";
    let line_b = "based on Sinclair 1982 ROM";
    d.print_str(0, CHAR_H - 4, line_a);
    d.print_str(0, CHAR_H - 3, line_b);
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
        assert_eq!(num(&sys, "I"), Some(1.0));
    }

    #[test]
    fn step_limit_breaks_infinite_loop() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET I=0");
        enter(&mut sys);
        feed_str(&mut sys, "20 LET I=I+1");
        enter(&mut sys);
        feed_str(&mut sys, "30 GOTO 20");
        enter(&mut sys);
        feed_str(&mut sys, "RUN");
        enter(&mut sys);
        let i = num(&sys, "I").unwrap();
        assert!(i > 10_000.0, "expected many iterations, got {}", i);
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
        // System should now be parked awaiting input.
        assert!(sys.pending_input.is_some(), "expected pending input");
        // User types 7.
        feed_str(&mut sys, "7");
        enter(&mut sys);
        // RUN resumes; N=7, R=49.
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
        feed_str(&mut sys, "hello world");
        enter(&mut sys);
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
