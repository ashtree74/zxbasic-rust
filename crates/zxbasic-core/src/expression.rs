//! Numeric expression parser + evaluator.
//!
//! Grammar (precedence low to high):
//!
//! ```text
//! expr   = term   (('+' | '-') term)*
//! term   = power  (('*' | '/') power)*
//! power  = unary  ('^' power)?            // right-associative
//! unary  = '-' unary | primary
//! primary = number | identifier | '(' expr ')'
//! number = digit+ ('.' digit+)? ([eE] [+-]? digit+)?
//! identifier = [A-Za-z] [A-Za-z0-9]*
//! ```
//!
//! Whitespace is skipped between tokens. String values, functions, and array
//! subscripts arrive in later MVPs.

use core::iter::Peekable;
use core::str::Chars;

/// Variable lookup. Names are passed in Spectrum-style uppercase.
pub trait Env {
    fn get_var(&self, name: &str) -> Option<f64>;
}

/// An [`Env`] with no variables defined.
pub struct EmptyEnv;
impl Env for EmptyEnv {
    fn get_var(&self, _: &str) -> Option<f64> {
        None
    }
}

/// Parse and evaluate `src` as a numeric expression with no variables.
pub fn evaluate(src: &str) -> Result<f64, EvalError> {
    evaluate_with(src, &EmptyEnv)
}

/// Parse and evaluate `src` with variable lookups against `env`.
pub fn evaluate_with(src: &str, env: &dyn Env) -> Result<f64, EvalError> {
    let mut p = Parser::new(src, env);
    let v = p.expr()?;
    p.skip_ws();
    if p.peek().is_some() {
        return Err(EvalError::TrailingInput);
    }
    Ok(v)
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
            self.primary()
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
            Some(c) if c.is_ascii_alphabetic() => self.identifier(),
            _ => Err(EvalError::Nonsense),
        }
    }

    fn identifier(&mut self) -> Result<f64, EvalError> {
        let mut name = String::new();
        while let Some(c) = self.peek() {
            if c.is_ascii_alphanumeric() {
                name.push(c.to_ascii_uppercase());
                self.bump();
            } else {
                break;
            }
        }
        match self.env.get_var(&name) {
            Some(v) => Ok(v),
            None => Err(EvalError::UnknownVariable(name)),
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

#[cfg(test)]
mod tests {
    use super::{evaluate, evaluate_with, Env, EvalError};
    use std::collections::HashMap;

    struct MapEnv(HashMap<&'static str, f64>);
    impl Env for MapEnv {
        fn get_var(&self, name: &str) -> Option<f64> {
            self.0.get(name).copied()
        }
    }

    #[track_caller]
    fn ok(src: &str, want: f64) {
        let got = evaluate(src).expect(src);
        let close = if want == 0.0 {
            got.abs() < 1e-12
        } else {
            ((got - want) / want).abs() < 1e-12
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
    fn decimals_and_exponents() {
        ok("3.14", 3.14);
        ok("0.5+0.5", 1.0);
        ok("1e3", 1000.0);
        ok("1.5E2", 150.0);
        ok("2e-3", 0.002);
    }

    #[test]
    fn whitespace_tolerated() {
        ok(" 1 + 2 ", 3.0);
        ok("\t1+  2*3 ", 7.0);
    }

    #[test]
    fn variables_resolve() {
        let env = MapEnv(HashMap::from([("A", 5.0), ("B", 10.0)]));
        assert_eq!(evaluate_with("A", &env), Ok(5.0));
        assert_eq!(evaluate_with("A*B+1", &env), Ok(51.0));
        assert_eq!(evaluate_with("a + b", &env), Ok(15.0)); // case-folded
    }

    #[test]
    fn unknown_variable_errors() {
        let env = MapEnv(HashMap::new());
        match evaluate_with("X", &env) {
            Err(EvalError::UnknownVariable(s)) => assert_eq!(s, "X"),
            other => panic!("want UnknownVariable, got {:?}", other),
        }
    }

    #[test]
    fn parse_errors() {
        assert_eq!(evaluate(""), Err(EvalError::Nonsense));
        assert_eq!(evaluate("1+"), Err(EvalError::Nonsense));
        assert_eq!(evaluate("(1+2"), Err(EvalError::MissingCloseParen));
        assert_eq!(evaluate("1 2"), Err(EvalError::TrailingInput));
        assert_eq!(evaluate("1e"), Err(EvalError::BadNumber));
    }
}
