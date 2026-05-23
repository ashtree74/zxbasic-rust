//! Expression parser + evaluator producing [`Value`] (numeric or string).
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
//! primary  = number | string_literal | identifier | '(' expr ')'
//! number   = digit+ ('.' digit+)? ([eE] [+-]? digit+)?
//! string_literal = '"' (any-byte-except-")* '"'
//! identifier = [A-Za-z] [A-Za-z0-9]* '$'?
//! ```
//!
//! Type rules:
//!   * `+` works on `num + num` (add) or `str + str` (concat). Mixed → error.
//!   * `-`, `*`, `/`, `^` are numeric only.
//!   * Comparisons require the same type on both sides; strings compare
//!     lexicographically.
//!   * Identifiers ending in `$` are string-typed; others are numeric.

use core::iter::Peekable;
use core::str::Chars;

use crate::value::Value;

/// Variable lookup and function dispatch.
pub trait Env {
    fn get_var(&self, name: &str) -> Option<Value>;

    /// Call a built-in function. Default: no functions known.
    fn call_fn(&self, _name: &str, _args: &[Value]) -> Option<Value> {
        None
    }
}

/// An [`Env`] with no variables and no functions.
pub struct EmptyEnv;
impl Env for EmptyEnv {
    fn get_var(&self, _: &str) -> Option<Value> {
        None
    }
}

/// Built-in numeric functions recognised at parse time (single argument).
pub const FUNCS_1ARG_NUM: &[&str] = &[
    "SIN", "COS", "TAN", "ASN", "ACS", "ATN", "LN", "EXP", "INT", "ABS", "SQR", "SGN",
    "LEN", "CODE", "VAL",
];
/// Built-in string-returning functions (single argument). Their names carry
/// the `$` suffix.
pub const FUNCS_1ARG_STR: &[&str] = &["CHR$", "STR$"];
/// Built-in 0-arg functions.
pub const FUNCS_0ARG: &[&str] = &["RND", "PI"];

fn is_func_0arg(name: &str) -> bool {
    FUNCS_0ARG.contains(&name)
}
fn is_func_1arg(name: &str) -> bool {
    FUNCS_1ARG_NUM.contains(&name) || FUNCS_1ARG_STR.contains(&name)
}

/// Parse and evaluate `src` as an expression with no variables.
pub fn evaluate(src: &str) -> Result<Value, EvalError> {
    evaluate_with(src, &EmptyEnv)
}

/// Parse and evaluate `src` with variable lookups and function dispatch
/// against `env`.
pub fn evaluate_with(src: &str, env: &dyn Env) -> Result<Value, EvalError> {
    let mut p = Parser::new(src, env);
    let v = p.expr()?;
    p.skip_ws();
    if p.peek().is_some() {
        return Err(EvalError::TrailingInput);
    }
    Ok(v)
}

/// Convenience: parse `src` and require it to be numeric.
pub fn evaluate_num_with(src: &str, env: &dyn Env) -> Result<f64, EvalError> {
    evaluate_with(src, env)?.as_num()
}

/// Parse `src` up to the first occurrence of `stop_at` (case-insensitive
/// whole word). Returns the evaluated value and the byte offset of `stop_at`.
/// Used by `IF expr THEN stmt` and `FOR I = a TO b STEP s`.
pub fn evaluate_until_keyword<'a>(
    src: &'a str,
    stop_at: &str,
    env: &dyn Env,
) -> Result<(Value, &'a str), EvalError> {
    let upper = src.to_ascii_uppercase();
    let kw = stop_at.to_ascii_uppercase();
    let mut search_from = 0;
    let pos = loop {
        let Some(rel) = upper[search_from..].find(&kw) else {
            return Err(EvalError::Nonsense);
        };
        let abs = search_from + rel;
        let before_ok = abs == 0 || !src.as_bytes()[abs - 1].is_ascii_alphanumeric();
        let after_idx = abs + kw.len();
        let after_ok =
            after_idx >= src.len() || !src.as_bytes()[after_idx].is_ascii_alphanumeric();
        if before_ok && after_ok {
            // Don't match if this position is inside a string literal.
            if inside_string_literal(src, abs) {
                search_from = abs + 1;
                continue;
            }
            break abs;
        }
        search_from = abs + 1;
    };
    let cond_src = &src[..pos];
    let rest = &src[pos + kw.len()..];
    let v = evaluate_with(cond_src, env)?;
    Ok((v, rest))
}

/// Best-effort check: is `pos` inside a `"..."` literal in `src`? Scans
/// forward counting unescaped quotes.
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

#[derive(Debug, PartialEq, Eq)]
pub enum EvalError {
    Nonsense,
    TrailingInput,
    MissingCloseParen,
    BadNumber,
    UnknownVariable(String),
    UnknownFunction(String),
    TypeMismatch,
    UnterminatedString,
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

    fn expr(&mut self) -> Result<Value, EvalError> {
        self.relation()
    }

    fn relation(&mut self) -> Result<Value, EvalError> {
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
        compare(&lhs, &rhs, op)
    }

    fn peek_after(&mut self, skip: usize) -> Option<char> {
        self.chars.clone().nth(skip)
    }

    fn additive(&mut self) -> Result<Value, EvalError> {
        let mut lhs = self.term()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('+') => {
                    self.bump();
                    let rhs = self.term()?;
                    lhs = add(lhs, rhs)?;
                }
                Some('-') => {
                    self.bump();
                    let rhs = self.term()?.as_num()?;
                    lhs = Value::Num(lhs.as_num()? - rhs);
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn term(&mut self) -> Result<Value, EvalError> {
        let mut lhs = self.power()?;
        loop {
            self.skip_ws();
            match self.peek() {
                Some('*') => {
                    self.bump();
                    let rhs = self.power()?.as_num()?;
                    lhs = Value::Num(lhs.as_num()? * rhs);
                }
                Some('/') => {
                    self.bump();
                    let rhs = self.power()?.as_num()?;
                    lhs = Value::Num(lhs.as_num()? / rhs);
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn power(&mut self) -> Result<Value, EvalError> {
        let base = self.unary()?;
        self.skip_ws();
        if self.peek() == Some('^') {
            self.bump();
            let exp = self.power()?.as_num()?;
            Ok(Value::Num(base.as_num()?.powf(exp)))
        } else {
            Ok(base)
        }
    }

    fn unary(&mut self) -> Result<Value, EvalError> {
        self.skip_ws();
        if self.peek() == Some('-') {
            self.bump();
            Ok(Value::Num(-self.unary()?.as_num()?))
        } else if self.peek() == Some('+') {
            self.bump();
            self.unary()
        } else {
            self.call_or_primary()
        }
    }

    fn call_or_primary(&mut self) -> Result<Value, EvalError> {
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

    fn primary(&mut self) -> Result<Value, EvalError> {
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
            Some('"') => self.string_literal(),
            Some(c) if c.is_ascii_digit() || c == '.' => self.number().map(Value::Num),
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
        // Optional `$` suffix marks a string-typed name.
        if self.peek() == Some('$') {
            self.bump();
            name.push('$');
        }
        name
    }

    fn string_literal(&mut self) -> Result<Value, EvalError> {
        self.bump(); // opening "
        let mut bytes = Vec::new();
        loop {
            match self.peek() {
                None => return Err(EvalError::UnterminatedString),
                Some('"') => {
                    self.bump();
                    // Spectrum convention: "" inside a string is a literal ".
                    if self.peek() == Some('"') {
                        self.bump();
                        bytes.push(b'"');
                        continue;
                    }
                    return Ok(Value::Str(bytes));
                }
                Some(c) => {
                    self.bump();
                    // Push as-is; we treat strings as byte strings, so non-ASCII
                    // is encoded as its UTF-8 bytes for now (real Spectrum char
                    // set arrives with full keyboard support in MVP-6).
                    let mut buf = [0u8; 4];
                    let s = c.encode_utf8(&mut buf);
                    bytes.extend_from_slice(s.as_bytes());
                }
            }
        }
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

fn add(lhs: Value, rhs: Value) -> Result<Value, EvalError> {
    match (lhs, rhs) {
        (Value::Num(a), Value::Num(b)) => Ok(Value::Num(a + b)),
        (Value::Str(mut a), Value::Str(b)) => {
            a.extend_from_slice(&b);
            Ok(Value::Str(a))
        }
        _ => Err(EvalError::TypeMismatch),
    }
}

fn compare(lhs: &Value, rhs: &Value, op: &str) -> Result<Value, EvalError> {
    let ord = match (lhs, rhs) {
        (Value::Num(a), Value::Num(b)) => a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal),
        (Value::Str(a), Value::Str(b)) => a.cmp(b),
        _ => return Err(EvalError::TypeMismatch),
    };
    use core::cmp::Ordering::*;
    let r = match op {
        "=" => ord == Equal,
        "<>" => ord != Equal,
        "<" => ord == Less,
        ">" => ord == Greater,
        "<=" => ord != Greater,
        ">=" => ord != Less,
        _ => unreachable!(),
    };
    Ok(Value::Num(if r { 1.0 } else { 0.0 }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct TestEnv {
        vars: HashMap<&'static str, Value>,
    }
    impl Env for TestEnv {
        fn get_var(&self, name: &str) -> Option<Value> {
            self.vars.get(name).cloned()
        }
        fn call_fn(&self, name: &str, args: &[Value]) -> Option<Value> {
            Some(match (name, args) {
                ("PI", []) => Value::Num(core::f64::consts::PI),
                ("RND", []) => Value::Num(0.5),
                ("SIN", [Value::Num(x)]) => Value::Num(x.sin()),
                ("COS", [Value::Num(x)]) => Value::Num(x.cos()),
                ("ABS", [Value::Num(x)]) => Value::Num(x.abs()),
                ("INT", [Value::Num(x)]) => Value::Num(x.floor()),
                ("SQR", [Value::Num(x)]) => Value::Num(x.sqrt()),
                ("LN", [Value::Num(x)]) => Value::Num(x.ln()),
                ("EXP", [Value::Num(x)]) => Value::Num(x.exp()),
                ("SGN", [Value::Num(x)]) => Value::Num(x.signum()),
                ("TAN", [Value::Num(x)]) => Value::Num(x.tan()),
                ("ASN", [Value::Num(x)]) => Value::Num(x.asin()),
                ("ACS", [Value::Num(x)]) => Value::Num(x.acos()),
                ("ATN", [Value::Num(x)]) => Value::Num(x.atan()),
                ("LEN", [Value::Str(s)]) => Value::Num(s.len() as f64),
                ("CODE", [Value::Str(s)]) => Value::Num(s.first().copied().unwrap_or(0) as f64),
                ("CHR$", [Value::Num(n)]) => Value::Str(vec![*n as u8]),
                ("STR$", [Value::Num(n)]) => Value::Str(crate::fp_format::format(*n).into_bytes()),
                ("VAL", [Value::Str(s)]) => {
                    let src = std::str::from_utf8(s).ok()?;
                    return evaluate(src).ok();
                }
                _ => return None,
            })
        }
    }

    fn empty_env() -> TestEnv {
        TestEnv { vars: HashMap::new() }
    }

    #[track_caller]
    fn ok_num(src: &str, want: f64) {
        let got = evaluate_with(src, &empty_env()).expect(src).as_num().unwrap();
        let close = if want == 0.0 {
            got.abs() < 1e-12
        } else {
            ((got - want) / want).abs() < 1e-9
        };
        assert!(close, "evaluate({:?}) = {}, want {}", src, got, want);
    }

    #[track_caller]
    fn ok_str(src: &str, want: &str) {
        let got = evaluate_with(src, &empty_env()).expect(src);
        let bytes = got.as_str().unwrap();
        assert_eq!(bytes, want.as_bytes(), "evaluate({:?})", src);
    }

    #[test]
    fn numeric_basics() {
        ok_num("1+2*3", 7.0);
        ok_num("2^3^2", 512.0);
        ok_num("-3", -3.0);
        ok_num("1=1", 1.0);
        ok_num("3<>3", 0.0);
    }

    #[test]
    fn string_literal_and_concat() {
        ok_str("\"hello\"", "hello");
        ok_str("\"foo\"+\"bar\"", "foobar");
    }

    #[test]
    fn string_compare() {
        ok_num("\"abc\"<\"abd\"", 1.0);
        ok_num("\"abc\"=\"abc\"", 1.0);
        ok_num("\"abc\">\"abb\"", 1.0);
    }

    #[test]
    fn type_mismatch_on_minus() {
        let env = empty_env();
        assert!(matches!(
            evaluate_with("\"a\"-\"b\"", &env),
            Err(EvalError::TypeMismatch)
        ));
    }

    #[test]
    fn type_mismatch_on_mixed_add() {
        let env = empty_env();
        assert!(matches!(
            evaluate_with("\"a\"+1", &env),
            Err(EvalError::TypeMismatch)
        ));
    }

    #[test]
    fn string_funcs() {
        ok_num("LEN \"hello\"", 5.0);
        ok_num("CODE \"A\"", 65.0);
        ok_str("CHR$ 65", "A");
        ok_str("STR$ 42", "42");
        ok_num("VAL \"1+2*3\"", 7.0);
    }

    #[test]
    fn dollar_identifier() {
        let env = TestEnv {
            vars: HashMap::from([("A$", Value::Str(b"hi".to_vec())), ("A", Value::Num(7.0))]),
        };
        match evaluate_with("A$", &env).unwrap() {
            Value::Str(s) => assert_eq!(s, b"hi"),
            _ => panic!("expected string"),
        }
        match evaluate_with("A", &env).unwrap() {
            Value::Num(n) => assert_eq!(n, 7.0),
            _ => panic!("expected num"),
        }
    }

    #[test]
    fn func_calls_legacy() {
        ok_num("INT 3.7", 3.0);
        ok_num("SQR 9", 3.0);
        ok_num("SIN(0)+COS(0)", 1.0);
        ok_num("PI", core::f64::consts::PI);
        ok_num("RND", 0.5);
    }

    #[test]
    fn unterminated_string_errors() {
        let env = empty_env();
        assert_eq!(
            evaluate_with("\"oops", &env),
            Err(EvalError::UnterminatedString)
        );
    }

    #[test]
    fn evaluate_until_then_skips_quoted_then() {
        // The word THEN inside a string literal must not be treated as the
        // separator.
        let env = empty_env();
        let (v, rest) = evaluate_until_keyword(
            "\"if THEN go\" = \"if THEN go\" THEN PRINT 1",
            "THEN",
            &env,
        )
        .unwrap();
        assert_eq!(v.as_num().unwrap(), 1.0);
        assert_eq!(rest.trim_start(), "PRINT 1");
    }
}
