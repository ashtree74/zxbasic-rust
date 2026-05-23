//! ZX Spectrum-style formatting of `f64` for `PRINT`.
//!
//! Rules approximated from the Spectrum ROM (see `vendor/zxrom/11_calculator.asm`
//! and Vickers chapter 24):
//!
//! * Integers (|x| < 1e16 and x == trunc(x)) are printed with no decimal point.
//! * Otherwise, up to 8 significant digits, no trailing zeros.
//! * Outside the range 1e-4 .. 1e8 (exclusive on the upper side, inclusive on
//!   the lower) the number is shown in `E` notation, e.g. `1.5E12`.
//! * Negative numbers get a leading `-`. Positive numbers get no leading `+`
//!   (PRINT doesn't pad with a sign placeholder; that's only PRINT USING in
//!   later ROMs).
//! * `0` is printed as `0`, never `-0` or `0.0`.

/// Convert an `f64` to its Spectrum `PRINT` representation.
pub fn format(x: f64) -> String {
    if x == 0.0 || x == -0.0 {
        return "0".to_string();
    }
    if x.is_nan() {
        // Real Spectrum can't produce NaN, but we may. Print something sensible.
        return "NaN".to_string();
    }
    if x.is_infinite() {
        return if x.is_sign_negative() { "-Inf" } else { "Inf" }.to_string();
    }

    let abs = x.abs();
    let sign = if x.is_sign_negative() { "-" } else { "" };

    // Spectrum's fixed-notation window is 1e-4 ..< 1e8.
    let use_e = abs < 1e-4 || abs >= 1e8;

    // Integer path: exact integers below 1e8 print without a dot (1e8 itself
    // and above go to E-notation by the rule above).
    if !use_e && x == x.trunc() {
        return format!("{}{}", sign, abs as u64);
    }

    let body = if use_e {
        format_scientific(abs)
    } else {
        format_fixed(abs)
    };
    format!("{}{}", sign, body)
}

/// 8 significant digits, fixed notation, no trailing zeros, no trailing dot.
fn format_fixed(abs: f64) -> String {
    // Round to 8 significant digits.
    let digits = round_to_significant(abs, 8);
    // `{}` on an f64 uses Rust's shortest-round-trip representation, which is
    // not what we want (e.g. 0.1 → "0.1" but 1.0/3.0 → "0.3333333333333333").
    // Build the string ourselves using `{:.*}` and a decimal-place count.
    let log10 = abs.log10().floor() as i32; // position of the most-sig digit
    let decimals = (7 - log10).max(0) as usize;
    let s = format!("{:.*}", decimals, digits);
    trim_fp(&s)
}

fn format_scientific(abs: f64) -> String {
    let exp = abs.log10().floor() as i32;
    let mantissa = abs / 10f64.powi(exp);
    let m_rounded = round_to_significant(mantissa, 8);
    // Print mantissa with up to 7 decimals (8 sig figs total), trim zeros.
    let m_str = trim_fp(&format!("{:.7}", m_rounded));
    // Spectrum uses no `+` on positive exponents and no leading zero (`E12`,
    // `E-3`).
    format!("{}E{}", m_str, exp)
}

fn round_to_significant(x: f64, sig: i32) -> f64 {
    if x == 0.0 {
        return 0.0;
    }
    let d = x.abs().log10().ceil() as i32;
    let factor = 10f64.powi(sig - d);
    (x * factor).round() / factor
}

fn trim_fp(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    let trimmed = s.trim_end_matches('0').trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::format;

    #[track_caller]
    fn check(x: f64, want: &str) {
        let got = format(x);
        assert_eq!(got, want, "format({}) = {:?}, want {:?}", x, got, want);
    }

    #[test]
    fn integers() {
        check(0.0, "0");
        check(-0.0, "0");
        check(1.0, "1");
        check(-1.0, "-1");
        check(7.0, "7");
        check(42.0, "42");
        check(1000.0, "1000");
        check(-99999.0, "-99999");
    }

    #[test]
    fn simple_fractions() {
        check(0.5, "0.5");
        check(-0.5, "-0.5");
        check(1.5, "1.5");
        check(3.14, "3.14");
    }

    #[test]
    fn one_third() {
        // 8 significant digits.
        check(1.0 / 3.0, "0.33333333");
    }

    #[test]
    fn small_uses_e_notation() {
        // Below 1e-4 switches to E.
        check(1e-5, "1E-5");
        check(0.00001, "1E-5");
        check(1.5e-6, "1.5E-6");
    }

    #[test]
    fn large_uses_e_notation() {
        // 1e8 and above switches to E.
        check(1e8, "1E8");
        check(1.5e12, "1.5E12");
        check(-2e30, "-2E30");
    }

    #[test]
    fn boundary_just_below_e() {
        // 1e-4 stays fixed; 9.9999e7 stays fixed.
        check(0.0001, "0.0001");
        check(99_999_999.0, "99999999");
    }
}
