//! Single-owner state machine for the whole runtime.
//!
//! MVP-2 scope: variables, `LET`, numbered program lines, `LIST`, `RUN`,
//! `GOTO`, `STOP`, `NEW`, `CLS`. Statement parsing is still ad-hoc — a proper
//! tokenised dispatcher arrives when we extract the full token table from
//! `02_keyboard.asm` (planned for MVP-2's polish phase or MVP-3).

use std::collections::HashMap;

use crate::display::{Display, CHAR_H, CHAR_W, FRAME_RGBA_LEN};
use crate::expression::{self, Env};
use crate::fp_format;
use crate::program::Program;

/// Logical key fed into [`System::feed_key`].
#[derive(Debug, Clone, Copy)]
pub enum Key {
    /// Ordinary printable ASCII character (32..=126 typically).
    Char(u8),
    /// CR / Enter.
    Enter,
    /// Backspace / Delete.
    Backspace,
}

/// Hard limit on RUN steps. Prevents an unattended infinite loop from hanging
/// the browser. When hit, prints "B Integer out of range" — wrong text in
/// Spectrum terms but a fair stand-in until we have BREAK key handling.
const RUN_STEP_LIMIT: usize = 100_000;

/// Top-level runtime state.
pub struct System {
    display: Display,
    input_line: String,
    vars: HashMap<String, f64>,
    program: Program,
}

impl System {
    /// New system with the boot screen pre-painted and an empty input line.
    pub fn new() -> Self {
        let mut display = Display::new();
        paint_boot_screen(&mut display);
        let mut sys = Self {
            display,
            input_line: String::new(),
            vars: HashMap::new(),
            program: Program::new(),
        };
        sys.redraw_input();
        sys
    }

    /// Length of the RGBA buffer that [`Self::render_into`] expects.
    pub const FRAME_RGBA_LEN: usize = FRAME_RGBA_LEN;

    /// Render the current screen state into an RGBA byte buffer.
    pub fn render_into(&self, out: &mut [u8]) {
        self.display.render_into(out);
    }

    /// Advance one frame. MVP-2: no-op.
    pub fn frame(&mut self) {}

    /// Feed a single keystroke from the host.
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

    /// Top-level dispatch for an entered line:
    ///   * starts with a digit → store/delete a numbered program line;
    ///   * otherwise → execute as an immediate statement.
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
                    // GOTO outside of RUN is meaningless in our model.
                    self.println_error("Nonsense in BASIC");
                }
                StepResult::Error(msg) => self.println_error(&msg),
            }
        }
    }

    fn store_program_line(&mut self, line: &str) {
        // Pull the leading integer.
        let (num_str, rest) = split_leading_number(line);
        let Ok(n) = num_str.parse::<u16>() else {
            self.println_error("Nonsense in BASIC");
            return;
        };
        self.program.upsert(n, rest.trim_start().to_string());
    }

    /// Execute a single statement (e.g. one line of a program, or one
    /// immediate-mode entry). Statement chaining via `:` lands in MVP-3.
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
            _ => StepResult::Error("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_print(&mut self, args: &str) -> StepResult {
        // MVP-2: a single numeric expression. PRINT with no args prints a
        // blank line.
        if args.trim().is_empty() {
            self.display.println("");
            return StepResult::Ok;
        }
        let env = VarsRef(&self.vars);
        match expression::evaluate_with(args, &env) {
            Ok(v) => {
                let s = fp_format::format(v);
                self.display.println(&s);
                StepResult::Ok
            }
            Err(_) => StepResult::Error("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_let(&mut self, args: &str) -> StepResult {
        // <name> = <expr>
        let Some((lhs, rhs)) = args.split_once('=') else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        let name = lhs.trim().to_ascii_uppercase();
        if name.is_empty() || !is_valid_var_name(&name) {
            return StepResult::Error("Nonsense in BASIC".to_string());
        }
        let env = VarsRef(&self.vars);
        match expression::evaluate_with(rhs, &env) {
            Ok(v) => {
                self.vars.insert(name, v);
                StepResult::Ok
            }
            Err(_) => StepResult::Error("Nonsense in BASIC".to_string()),
        }
    }

    fn cmd_goto(&mut self, args: &str) -> StepResult {
        let env = VarsRef(&self.vars);
        let Ok(v) = expression::evaluate_with(args, &env) else {
            return StepResult::Error("Nonsense in BASIC".to_string());
        };
        if !(0.0..=65535.0).contains(&v) {
            return StepResult::Error("Integer out of range".to_string());
        }
        StepResult::Goto(v as u16)
    }

    fn cmd_list(&mut self) -> StepResult {
        // Collect first so we don't hold a borrow on self.program while
        // mutating self.display.
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
        // RUN clears variables and starts at the first line, or at the
        // argument if given.
        self.vars.clear();
        let start = if args.trim().is_empty() {
            0u16
        } else {
            let env = VarsRef(&self.vars);
            match expression::evaluate_with(args, &env) {
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

    fn println_error(&mut self, msg: &str) {
        self.display.println(msg);
    }
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of executing one statement.
enum StepResult {
    Ok,
    Stop,
    Goto(u16),
    Error(String),
}

/// Read-only view of System's variables, exposing them to the expression
/// evaluator as an [`Env`].
struct VarsRef<'a>(&'a HashMap<String, f64>);
impl<'a> Env for VarsRef<'a> {
    fn get_var(&self, name: &str) -> Option<f64> {
        self.0.get(name).copied()
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

fn is_valid_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric())
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

    #[test]
    fn let_then_print() {
        let mut sys = System::new();
        feed_str(&mut sys, "LET A=5");
        enter(&mut sys);
        feed_str(&mut sys, "PRINT A*2+1");
        enter(&mut sys);
        assert_eq!(sys.vars.get("A"), Some(&5.0));
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
        assert_eq!(sys.vars.get("A"), Some(&2.0));
        assert_eq!(sys.vars.get("B"), Some(&3.0));
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
        assert_eq!(sys.vars.get("I"), Some(&1.0));
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
        // I should have advanced approximately RUN_STEP_LIMIT / 2 times
        // (each iteration is 2 statements: LET, GOTO).
        let i = *sys.vars.get("I").unwrap();
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
    fn empty_text_deletes_program_line() {
        let mut sys = System::new();
        feed_str(&mut sys, "10 LET A=1");
        enter(&mut sys);
        feed_str(&mut sys, "10");
        enter(&mut sys);
        assert!(sys.program.is_empty());
    }
}
