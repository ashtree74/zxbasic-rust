//! Numeric expression parser + evaluator.
//!
//! Grammar (precedence low to high):
//!
//! ```text
//! expr     = relation
//! relation = additive (('=' | '<>' | '<' | '>' | '<=' | '>=') additive)?
//! additive = term     (('+' | '-') term)*
//! term     = power    (('*' | '/') power)*
//! power    = unary    ('^' power)?            // right-associative
//! unary    = '-' unary | '+' unary | call_or_primary
//! call_or_primary = func_call | primary
//! func_call = FUNC_NAME unary?                // 0- or 1-arg, Spectrum-style
//! primary  = number | identifier | '(' expr ')'
//! number   = digit+ ('.' digit+)? ([eE] [+-]? digit+)?
//! identifier = [A-Za-z] [A-Za-z0-9]*
//! ```
//!
//! Function calls follow Spectrum syntax: `SIN x` (no parens) or `SIN(x)`
//! (parens belong to the inner expression). Comparisons return 1.0 if true,
//! 0.0 if false — matching Spectrum's "false=0, true=1" numeric booleans.

use core::iter::Peekable;
use core::str::Chars;

/// Variable lookup and function dispatch. Identifiers and function names are
/// passed in Spectrum-style uppercase.
pub trait Env {
    fn get_var(&self, name: &str) -> Option<f64>;

    /// Call a built-in function. Default: no functions known.
    fn call_fn(&self, _name: &str, _args: &[f64]) -> Option<f64> {
        None
    }
}

/// An [`Env`] with no variables and no functions.
pub struct EmptyEnv;
impl Env for EmptyEnv {
    fn get_var(&self, _: &str) -> Option<f64> {
        None
    }
}

/// Built-in functions recognised at parse time. Listed centrally so both the
/// parser (to decide identifier-vs-call) and a host [`Env`] can stay in sync.
pub const FUNCS_0ARG: &[&str] = &["RND", "PI"];
pub const FUNCS_1ARG: &[&str] = &[
    "SIN", "COS", "TAN", "ASN", "ACS", "ATN", "LN", "EXP", "INT", "ABS", "SQR", "SGN",
];

fn is_func_0arg(name: &str) -> bool {
    FUNCS_0ARG.contains(&name)
}
fn is_func_1arg(name: &str) -> bool {
    FUNCS_1ARG.contains(&name)
}

/// Parse and evaluate `src` as a numeric expression with no variables.
pub fn evaluate(src: &str) -> Result<f64, EvalError> {
    evaluate_with(src, &EmptyEnv)
}

/// Parse and evaluate `src` with variable lookups and function dispatch
/// against `env`.
pub fn evaluate_with(src: &str, env: &dyn Env) -> Result<f64, EvalError> {
    let mut p = Parser::new(src, env);
    let v = p.expr()?;
    p.skip_ws();
    if p.peek().is_some() {
        return Err(EvalError::TrailingInput);
    }
    Ok(v)
}

/// Parse `src` up to the first occurrence of `stop_at` (case-insensitive
/// whole word). Returns the value and the byte offset of `stop_at`. Used by
/// `IF expr THEN stmt` so the caller can resume parsing `stmt`.
pub fn evaluate_until_keyword<'a>(
    src: &'a str,
    stop_at: &str,
    env: &dyn Env,
) -> Result<(f64, &'a str), EvalError> {
    let upper = src.to_ascii_uppercase();
    let kw = stop_at.to_ascii_uppercase();
    // Find " THEN" (with leading whitespace) as a whole word.
    let mut search_from = 0;
    let pos = loop {
        let Some(rel) = upper[search_from..].find(&kw) else {
            return Err(EvalError::Nonsense);
        };
        let abs = search_from + rel;
        let before_ok = abs == 0
            || !src.as_bytes()[abs - 1].is_ascii_alphanumeric();
        let after_idx = abs + kw.len();
        let after_ok = after_idx >= src.len()
            || !src.as_bytes()[after_idx].is_ascii_alphanumeric();
        if before_ok && after_ok {
            break abs;
        }
        search_from = abs + 1;
    };
    let cond_src = &src[..pos];
    let rest = &src[pos + kw.len()..];
    let v = evaluate_with(cond_src, env)?;
    Ok((v, rest))
}

#[derive(Debug, PartialEq, Eq)]
pub enum EvalError {
    /// Generic "didn't parse" — Spectrum reports this as "Nonsense in BASIC".
    Nonsense,
    /// Extra characters after the expression.
    TrailingInput,
    /// Unbalanced parens / missing closer.
    MissingCloseParen,
    /// Number literal we couldn't parse as f64.
    BadNumber,
    /// Reference to a name that the environment doesn't know.
    UnknownVariable(String),
    /// Call to a function the environment doesn't implement.
    UnknownFunction(String),
}

struct Parser<'a, 'e> {
    chars: Peekable<Chars<'a>>,
    env: &'e dyn Env,
}

impl<'a, 'e> Parser<'a, 'e> {
    fn new(src: &'a str, env: &'e dyn Env) -> Self {
        Self {
            chars: src.chars().peekable(),
            env,
        }
    }

    fn peek(&mut self) -> Option<char> {
        self.chars.peek().copied()
    }
    fn bump(&mut self) -> Option<char> {
        self.chars.next()
    }
    fn skip_ws(&mut self) {
        while matches!(self.peek(), Some(c) if c.is_ascii_whitespace()) {
            self.bump();
        }
    }
    fn eat(&mut self, want: char) -> bool {
        self.skip_ws();
        if self.peek() == Some(want) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn expr(&mut self) -> Result<f64, EvalError> {
        self.relation()
    }

    fn relation(&mut self) -> Result<f64, EvalError> {
        let lhs = self.additive()?;
        self.skip_ws();
        let op = match (self.peek(), self.peek_after(1)) {
            (Some('<'), Some('=')) => Some(("<=", 2)),
            (Some('>'), Some('=')) => Some((">=", 2)),
            (Some('<'), Some('>')) => Some(("<>", 2)),
            (Some('<'), _) => Some(("<", 1)),
            (Some('>'), _) => Some((">", 1)),
            (Some('='), _) => Some(("=", 1)),
            _ => None,
        };
        let Some((op, n)) = op else { return Ok(lhs) };
        for _ in 0..n {
            self.bump();
        }
        let rhs = self.additive()?;
        Ok(match op {
            "=" => bool_to_num(lhs == rhs),
            "<>" => bool_to_num(lhs != rhs),
            "<" => bool_to_num(lhs < rhs),
            ">" => bool_to_num(lhs > rhs),
            "<=" => bool_to_num(lhs <= rhs),
            ">=" => bool_to_num(lhs >= rhs),
            _ => unreachable!(),
        })
    }

    fn peek_after(&mut self, skip: usize) -> Option<char> {
        self.chars.clone().nth(skip)
    }

    fn additive(&mut self) -> Result<f64, EvalError> {
        let mut lhs = self.term()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('+') => {
                    self.bump();
                    lhs += self.term()?;
                }
                Some('-') => {
                    self.bump();
                    lhs -= self.term()?;
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn term(&mut self) -> Result<f64, EvalError> {
        let mut lhs = self.power()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('*') => {
                    self.bump();
                    lhs *= self.power()?;
                }
                Some('/') => {
                    self.bump();
                    lhs /= self.power()?;
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn power(&mut self) -> Result<f64, EvalError> {
        let base = self.unary()?;
        self.skip_ws();
        if self.peek() == Some('^') {
            self.bump();
            let exp = self.power()?; // right-associative
            Ok(base.powf(exp))
        } else {
            Ok(base)
        }
    }

    fn unary(&mut self) -> Result<f64, EvalError> {
        self.skip_ws();
        if self.peek() == Some('-') {
            self.bump();
            Ok(-self.unary()?)
        } else if self.peek() == Some('+') {
            self.bump();
            self.unary()
        } else {
            self.call_or_primary()
        }
    }

    fn call_or_primary(&mut self) -> Result<f64, EvalError> {
        self.skip_ws();
        match self.peek() {
            Some(c) if c.is_ascii_alphabetic() => {
                let name = self.read_identifier();
                if is_func_0arg(&name) {
                    self.env
                        .call_fn(&name, &[])
                        .ok_or(EvalError::UnknownFunction(name))
                } else if is_func_1arg(&name) {
                    let arg = self.unary()?;
                    self.env
                        .call_fn(&name, &[arg])
                        .ok_or(EvalError::UnknownFunction(name))
                } else {
                    self.env
                        .get_var(&name)
                        .ok_or(EvalError::UnknownVariable(name))
                }
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Result<f64, EvalError> {
        self.skip_ws();
        match self.peek() {
            Some('(') => {
                self.bump();
                let v = self.expr()?;
                if !self.eat(')') {
                    return Err(EvalError::MissingCloseParen);
                }
                Ok(v)
            }
            Some(c) if c.is_ascii_digit() || c == '.' => self.number(),
            _ => Err(EvalError::Nonsense),
        }
    }

    fn read_identifier(&mut self) -> String {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() {
                name.push(c.to_ascii_uppercase());
                self.bump();
            } else {
                break;
            }
        }
        name
    }

    fn number(&mut self) -> Result<f64, EvalError> {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                s.push(c);
                self.bump();
            } else {
                break;
            }
        }
        if self.peek() == Some('.') {
            s.push('.');
            self.bump();
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    s.push(c);
                    self.bump();
                } else {
                    break;
                }
            }
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            s.push('e');
            self.bump();
            if matches!(self.peek(), Some('+' | '-')) {
                s.push(self.bump().unwrap());
            }
            let start = s.len();
            while let Some(c) = self.peek() {
                if c.is_ascii_digit() {
                    s.push(c);
                    self.bump();
                } else {
                    break;
                }
            }
            if s.len() == start {
                return Err(EvalError::BadNumber);
            }
        }
        s.parse::<f64>().map_err(|_| EvalError::BadNumber)
    }
}

fn bool_to_num(b: bool) -> f64 {
    if b { 1.0 } else { 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct TestEnv {
        vars: HashMap<&'static str, f64>,
    }
    impl Env for TestEnv {
        fn get_var(&self, name: &str) -> Option<f64> {
            self.vars.get(name).copied()
        }
        fn call_fn(&self, name: &str, args: &[f64]) -> Option<f64> {
            match name {
                "PI" => Some(core::f64::consts::PI),
                "RND" => Some(0.5), // deterministic for tests
                "SIN" => Some(args[0].sin()),
                "COS" => Some(args[0].cos()),
                "ABS" => Some(args[0].abs()),
                "INT" => Some(args[0].floor()),
                "SQR" => Some(args[0].sqrt()),
                "LN" => Some(args[0].ln()),
                "EXP" => Some(args[0].exp()),
                "SGN" => Some(args[0].signum()),
                "TAN" => Some(args[0].tan()),
                "ASN" => Some(args[0].asin()),
                "ACS" => Some(args[0].acos()),
                "ATN" => Some(args[0].atan()),
                _ => None,
            }
        }
    }

    fn empty_env() -> TestEnv {
        TestEnv { vars: HashMap::new() }
    }

    #[track_caller]
    fn ok(src: &str, want: f64) {
        let got = evaluate_with(src, &empty_env()).expect(src);
        let close = if want == 0.0 {
            got.abs() < 1e-12
        } else {
            ((got - want) / want).abs() < 1e-9
        };
        assert!(close, "evaluate({:?}) = {}, want {}", src, got, want);
    }

    #[test]
    fn integers() {
        ok("0", 0.0);
        ok("7", 7.0);
        ok("42", 42.0);
    }

    #[test]
    fn arithmetic_precedence() {
        ok("1+2*3", 7.0);
        ok("(1+2)*3", 9.0);
        ok("10-2-3", 5.0);
        ok("16/4/2", 2.0);
    }

    #[test]
    fn unary_minus() {
        ok("-3", -3.0);
        ok("-3+5", 2.0);
        ok("- -3", 3.0);
        ok("4*-3", -12.0);
    }

    #[test]
    fn power_right_assoc() {
        ok("2^3", 8.0);
        ok("2^3^2", 512.0);
        ok("(2^3)^2", 64.0);
    }

    #[test]
    fn comparisons() {
        ok("1=1", 1.0);
        ok("1=2", 0.0);
        ok("1<>2", 1.0);
        ok("3<5", 1.0);
        ok("5<5", 0.0);
        ok("5<=5", 1.0);
        ok("5>=5", 1.0);
        ok("6>=5", 1.0);
        ok("4>5", 0.0);
    }

    #[test]
    fn comparisons_have_lowest_precedence() {
        ok("1+2=3", 1.0);   // (1+2)=3 → 1
        ok("2*3>5", 1.0);   // (2*3)>5 → 1
    }

    #[test]
    fn func_calls_no_parens() {
        ok("INT 3.7", 3.0);
        ok("ABS -5", 5.0);
        ok("SQR 9", 3.0);
        ok("SIN 0", 0.0);
        ok("COS 0", 1.0);
    }

    #[test]
    fn func_calls_with_parens() {
        ok("INT(3.7)", 3.0);
        ok("SIN(0)+COS(0)", 1.0);
    }

    #[test]
    fn no_arg_funcs() {
        ok("PI", core::f64::consts::PI);
        ok("RND", 0.5);
    }

    #[test]
    fn func_eats_unary_argument() {
        // SIN 3^2 = SIN(3)^2 on Spectrum, since function precedence is
        // higher than ^. We model that via unary-arg consumption.
        let want = (3.0f64.sin()).powi(2);
        ok("SIN 3^2", want);
        // But SIN(3^2) reads the whole power inside parens.
        ok("SIN(3^2)", 9.0f64.sin());
    }

    #[test]
    fn variables_resolve() {
        let env = TestEnv {
            vars: HashMap::from([("A", 5.0), ("B", 10.0)]),
        };
        assert_eq!(evaluate_with("A", &env), Ok(5.0));
        assert_eq!(evaluate_with("A*B+1", &env), Ok(51.0));
        assert_eq!(evaluate_with("a + b", &env), Ok(15.0));
    }

    #[test]
    fn unknown_variable_errors() {
        let env = empty_env();
        match evaluate_with("X", &env) {
            Err(EvalError::UnknownVariable(s)) => assert_eq!(s, "X"),
            other => panic!("want UnknownVariable, got {:?}", other),
        }
    }

    #[test]
    fn evaluate_until_then() {
        let env = empty_env();
        let (v, rest) = evaluate_until_keyword("3 > 1 THEN PRINT 5", "THEN", &env).unwrap();
        assert_eq!(v, 1.0);
        assert_eq!(rest.trim_start(), "PRINT 5");
    }

    #[test]
    fn evaluate_until_then_skips_inside_identifier() {
        // The literal word "WEATHER" contains "THE" — must not match as a
        // keyword.
        let env = TestEnv {
            vars: HashMap::from([("WEATHEN", 1.0)]),
        };
        let (v, rest) = evaluate_until_keyword("WEATHEN THEN PRINT 1", "THEN", &env).unwrap();
        assert_eq!(v, 1.0);
        assert_eq!(rest.trim_start(), "PRINT 1");
    }
}
