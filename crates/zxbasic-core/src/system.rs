//! Single-owner state machine for the whole runtime.
//!
//! MVP-3b additions on top of MVP-3a: string values and string-typed
//! identifiers (`A$`, `MSG$`), polymorphic `+` for concatenation, and the
//! string built-ins (`LEN`, `CODE`, `CHR$`, `STR$`, `VAL`).

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
                self.dispatch_input(&line);
            }
        }
        self.redraw_input();
    }

    fn redraw_input(&mut self) {
        let cursor_col = self.input_line.chars().count().min(CHAR_W - 1);
        self.display.print_input(&self.input_line, cursor_col);
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
                StepResult::Goto(_) => {
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
                StepResult::Ok
            }
            "CLS" => {
                self.display.clear();
                StepResult::Ok
            }
            "LIST" => self.cmd_list(),
            "RUN" => self.cmd_run(rest),
            "IF" => self.cmd_if(rest),
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
        let start = if args.trim().is_empty() {
            0u16
        } else {
            let env = SysEnv { vars: &self.vars, prng: &self.prng };
            match expression::evaluate_with(args, &env).and_then(|v| v.as_num()) {
                Ok(v) if (0.0..=65535.0).contains(&v) => v as u16,
                _ => return StepResult::Error("Nonsense in BASIC".to_string()),
            }
        };
        let mut pc = self.program.next_at_or_after(start);
        let mut steps = 0usize;
        while let Some(line_no) = pc {
            if steps >= RUN_STEP_LIMIT {
                self.println_error("D BREAK - CONT repeats");
                return StepResult::Ok;
            }
            steps += 1;
            let stmt = self
                .program
                .get(line_no)
                .map(str::to_string)
                .unwrap_or_default();
            match self.execute_statement(&stmt) {
                StepResult::Ok => {
                    pc = self.program.next_at_or_after(line_no.saturating_add(1));
                }
                StepResult::Stop => return StepResult::Ok,
                StepResult::Goto(n) => {
                    pc = self.program.next_at_or_after(n);
                }
                StepResult::Error(msg) => {
                    self.println_error(&format!("{}, {}:1", msg, line_no));
                    return StepResult::Ok;
                }
            }
        }
        StepResult::Ok
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
