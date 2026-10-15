//! A small, pure calculator behind the agent's `calculate` tool.
//!
//! Small models make arithmetic slips, so the agent can hand expressions to
//! this evaluator instead. It is a hand-written recursive-descent parser: it
//! never runs code, touches files, or allocates without bound, and every
//! input yields a result or an error message, never a panic.
//!
//! Numbers stay exact rationals through `+ - * / // %` and integer powers,
//! so `0.1 + 0.2` is `0.3` and `1474 / 1.1` is `1340`. Arithmetic that
//! leaves exact rationals (roots, logarithms, fractional powers, or values
//! too large for 128-bit integers) uses `f64` and shows 12 significant
//! digits.
//!
//! Conventions, chosen to match common calculators and Python:
//! - `^` and `**` are powers and associate to the right: `2^3^2` is 512.
//! - Unary minus binds looser than a power: `-2^2` is -4.
//! - `%` is the floored modulo, and `//` the floored division: `-7 % 3` is 2.
//! - Commas separate function arguments inside a call, such as `max(1, 2)`.
//!   Outside a call, a comma followed by exactly three digits groups
//!   thousands: `1,474` is 1474. Any other comma is an error.
//! - A trailing `=` is ignored, so `1474 / 1.1 =` works.
use std::fmt::Write as _;

/// Longest input accepted in one call, in characters.
pub const MAX_INPUT_CHARS: usize = 4_000;
/// Most expressions evaluated in one call.
pub const MAX_EXPRESSIONS: usize = 50;
/// Deepest nesting of parentheses, calls, and unary operators.
const MAX_DEPTH: usize = 64;
/// Largest integer exponent computed exactly.
const MAX_EXACT_EXPONENT: i128 = 4_096;
/// Significant digits shown for inexact results.
const SIGNIFICANT_DIGITS: usize = 12;

/// The outcome of one expression in a batch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    /// The expression as written, trimmed.
    pub input: String,
    /// `"<normalised expression> = <value>"`, or an actionable error.
    pub outcome: Result<String, String>,
}

/// Evaluate one or more expressions separated by newlines or `;`.
pub fn evaluate_batch(input: &str) -> Result<Vec<Line>, String> {
    if input.chars().count() > MAX_INPUT_CHARS {
        return Err(format!(
            "input is longer than {MAX_INPUT_CHARS} characters; split it across calls"
        ));
    }
    let expressions: Vec<&str> = input
        .split(['\n', ';'])
        .map(str::trim)
        .filter(|expression| !expression.is_empty())
        .collect();
    if expressions.is_empty() {
        return Err("no expression to calculate; pass one such as \"1474 / 1.1\"".into());
    }
    if expressions.len() > MAX_EXPRESSIONS {
        return Err(format!(
            "{} expressions in one call; the limit is {MAX_EXPRESSIONS}",
            expressions.len()
        ));
    }
    Ok(expressions
        .into_iter()
        .map(|expression| Line {
            input: expression.to_owned(),
            outcome: evaluate(expression)
                .map(|(normalised, value)| format!("{normalised} = {value}"))
                .map_err(|error| format!("error at column {}: {}", error.column, error.message)),
        })
        .collect())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalcError {
    /// 1-based character column in the trimmed expression.
    pub column: usize,
    pub message: String,
}

fn fail<T>(column: usize, message: impl Into<String>) -> Result<T, CalcError> {
    Err(CalcError {
        column,
        message: message.into(),
    })
}

/// Evaluate one expression to its normalised form and formatted value.
pub fn evaluate(expression: &str) -> Result<(String, String), CalcError> {
    let expression = expression.trim();
    let expression = expression
        .strip_suffix('=')
        .map(str::trim_end)
        .unwrap_or(expression);
    let tokens = lex(expression)?;
    if tokens.is_empty() {
        return fail(1, "no expression to calculate");
    }
    let mut parser = Parser {
        tokens: &tokens,
        position: 0,
        depth: 0,
        end: expression.chars().count() + 1,
    };
    let value = parser.expression()?;
    if let Some(token) = parser.peek() {
        return match token.kind {
            Kind::RParen => fail(token.column, "unexpected \")\"; check the parentheses"),
            Kind::Comma => fail(
                token.column,
                "unexpected \",\"; commas only separate function arguments or group thousands as 1,234",
            ),
            _ => fail(
                token.column,
                "missing operator between values; use * to multiply",
            ),
        };
    }
    Ok((normalise(&tokens), format_value(&value)?))
}

// ---------------------------------------------------------------- lexing

#[derive(Debug, Clone, PartialEq)]
enum Kind {
    Number(Value),
    Name(String),
    Plus,
    Minus,
    Times,
    Divide,
    FloorDivide,
    Modulo,
    Power,
    LParen,
    RParen,
    Comma,
}

#[derive(Debug, Clone, PartialEq)]
struct Token {
    kind: Kind,
    /// The text to echo: separators removed, Unicode operators made ASCII.
    text: String,
    column: usize,
}

fn lex(expression: &str) -> Result<Vec<Token>, CalcError> {
    let chars: Vec<char> = expression.chars().collect();
    let mut tokens = Vec::new();
    // For each open parenthesis: whether it opened a function call.
    let mut calls: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        let column = i + 1;
        let simple = |kind: Kind, text: &str| Token {
            kind,
            text: text.into(),
            column,
        };
        match ch {
            c if c.is_whitespace() => {
                i += 1;
                continue;
            }
            '0'..='9' | '.' => {
                let in_call = calls.last().copied().unwrap_or(false);
                let (token, next) = lex_number(&chars, i, in_call)?;
                tokens.push(token);
                i = next;
                continue;
            }
            'a'..='z' | 'A'..='Z' | 'π' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == 'π') {
                    i += 1;
                }
                let name: String = chars[start..i].iter().collect();
                tokens.push(Token {
                    kind: Kind::Name(name.to_ascii_lowercase()),
                    text: name,
                    column,
                });
                continue;
            }
            '+' => tokens.push(simple(Kind::Plus, "+")),
            '-' | '−' | '–' => tokens.push(simple(Kind::Minus, "-")),
            '*' if chars.get(i + 1) == Some(&'*') => {
                tokens.push(simple(Kind::Power, "^"));
                i += 1;
            }
            '*' | '×' | '·' => tokens.push(simple(Kind::Times, "*")),
            '/' if chars.get(i + 1) == Some(&'/') => {
                tokens.push(simple(Kind::FloorDivide, "//"));
                i += 1;
            }
            '/' | '÷' => tokens.push(simple(Kind::Divide, "/")),
            '%' => tokens.push(simple(Kind::Modulo, "%")),
            '^' => tokens.push(simple(Kind::Power, "^")),
            '(' => {
                let opens_call = matches!(
                    tokens.last(),
                    Some(Token {
                        kind: Kind::Name(_),
                        ..
                    })
                );
                calls.push(opens_call);
                tokens.push(simple(Kind::LParen, "("));
            }
            ')' => {
                calls.pop();
                tokens.push(simple(Kind::RParen, ")"));
            }
            ',' => tokens.push(simple(Kind::Comma, ",")),
            '=' => {
                return fail(
                    column,
                    "\"=\" is not supported; pass only the expression to calculate",
                );
            }
            other => return fail(column, format!("unexpected \"{other}\"")),
        }
        i += 1;
    }
    Ok(tokens)
}

/// A number: digits with `_` separators, an optional fraction, an optional
/// exponent, and thousands commas outside calls.
fn lex_number(chars: &[char], start: usize, in_call: bool) -> Result<(Token, usize), CalcError> {
    let column = start + 1;
    let mut i = start;
    let mut text = String::new();
    let mut digits_before_point = 0;
    let mut grouped = false;
    while i < chars.len() {
        match chars[i] {
            d @ '0'..='9' => {
                text.push(d);
                digits_before_point += 1;
            }
            '_' if i + 1 < chars.len() && chars[i + 1].is_ascii_digit() && !text.is_empty() => {}
            ',' if !in_call && thousands_group_follows(chars, i, digits_before_point, grouped) => {
                grouped = true;
                digits_before_point = 0;
            }
            _ => break,
        }
        i += 1;
    }
    if chars.get(i) == Some(&'.') {
        text.push('.');
        i += 1;
        while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '_') {
            if chars[i] != '_' {
                text.push(chars[i]);
            }
            i += 1;
        }
    }
    if text == "." || text.is_empty() {
        return fail(column, "a number needs digits, such as 0.5");
    }
    if matches!(chars.get(i), Some('e' | 'E'))
        && (chars.get(i + 1).is_some_and(char::is_ascii_digit)
            || (matches!(chars.get(i + 1), Some('+' | '-'))
                && chars.get(i + 2).is_some_and(char::is_ascii_digit)))
    {
        text.push('e');
        i += 1;
        if matches!(chars[i], '+' | '-') {
            text.push(chars[i]);
            i += 1;
        }
        while i < chars.len() && chars[i].is_ascii_digit() {
            text.push(chars[i]);
            i += 1;
        }
    }
    if chars.get(i) == Some(&',') && !in_call {
        return fail(
            i + 1,
            "unexpected \",\"; use . for decimals, and write thousands as 1234, 1_234 or 1,234",
        );
    }
    let value = parse_number(&text).map_err(|message| CalcError { column, message })?;
    Ok((
        Token {
            kind: Kind::Number(value),
            text,
            column,
        },
        i,
    ))
}

/// A comma groups thousands when 1-3 digits (or a previous group of 3) come
/// before it and exactly three digits follow it.
fn thousands_group_follows(chars: &[char], comma: usize, before: usize, grouped: bool) -> bool {
    let leading_ok = if grouped {
        before == 3
    } else {
        (1..=3).contains(&before)
    };
    let three = (1..=3).all(|offset| chars.get(comma + offset).is_some_and(char::is_ascii_digit));
    let ends = !chars.get(comma + 4).is_some_and(char::is_ascii_digit);
    leading_ok && three && ends
}

/// Parse a decimal literal exactly when it fits in 128-bit integers.
fn parse_number(text: &str) -> Result<Value, String> {
    let (mantissa, exponent) = match text.split_once('e') {
        Some((mantissa, exponent)) => (
            mantissa,
            exponent
                .parse::<i32>()
                .map_err(|_| format!("exponent in {text:?} is out of range"))?,
        ),
        None => (text, 0),
    };
    let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
    let digits = format!("{whole}{fraction}");
    let scale = i32::try_from(fraction.len())
        .unwrap_or(i32::MAX)
        .saturating_sub(exponent);
    let exact = digits.parse::<i128>().ok().and_then(|numerator| {
        let power = ten_pow(scale.unsigned_abs())?;
        if scale >= 0 {
            Value::exact(numerator, power)
        } else {
            Value::exact(numerator.checked_mul(power)?, 1)
        }
    });
    match exact {
        Some(value) => Ok(value),
        None => {
            let float: f64 = text
                .parse()
                .map_err(|_| format!("{text:?} is not a number"))?;
            Value::float(float).map_err(|error| error.message)
        }
    }
}

fn ten_pow(exponent: u32) -> Option<i128> {
    10i128.checked_pow(exponent)
}

fn normalise(tokens: &[Token]) -> String {
    let mut out = String::new();
    for (index, token) in tokens.iter().enumerate() {
        let previous = index.checked_sub(1).map(|i| &tokens[i].kind);
        let unary = matches!(token.kind, Kind::Plus | Kind::Minus)
            && matches!(
                previous,
                None | Some(
                    Kind::Plus
                        | Kind::Minus
                        | Kind::Times
                        | Kind::Divide
                        | Kind::FloorDivide
                        | Kind::Modulo
                        | Kind::Power
                        | Kind::LParen
                        | Kind::Comma
                )
            );
        match token.kind {
            Kind::Plus
            | Kind::Minus
            | Kind::Times
            | Kind::Divide
            | Kind::FloorDivide
            | Kind::Modulo
                if !unary =>
            {
                let _ = write!(out, " {} ", token.text);
            }
            Kind::Power => out.push('^'),
            Kind::Comma => out.push_str(", "),
            _ => out.push_str(&token.text),
        }
    }
    out
}

// ---------------------------------------------------------------- parsing

struct Parser<'a> {
    tokens: &'a [Token],
    position: usize,
    depth: usize,
    /// The column just past the end, for "ends early" errors.
    end: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.position)
    }

    fn next(&mut self) -> Option<&Token> {
        let token = self.tokens.get(self.position);
        self.position += 1;
        token
    }

    fn descend(&mut self, column: usize) -> Result<(), CalcError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return fail(column, "expression nests too deeply");
        }
        Ok(())
    }

    fn expression(&mut self) -> Result<Value, CalcError> {
        let mut value = self.term()?;
        while let Some(token) = self.peek() {
            let (kind, column) = (token.kind.clone(), token.column);
            let apply: fn(&Value, &Value) -> Result<Value, String> = match kind {
                Kind::Plus => Value::add,
                Kind::Minus => Value::sub,
                _ => break,
            };
            self.position += 1;
            let right = self.term()?;
            value = apply(&value, &right).map_err(|message| CalcError { column, message })?;
        }
        Ok(value)
    }

    fn term(&mut self) -> Result<Value, CalcError> {
        let mut value = self.unary()?;
        while let Some(token) = self.peek() {
            let (kind, column) = (token.kind.clone(), token.column);
            let apply: fn(&Value, &Value) -> Result<Value, String> = match kind {
                Kind::Times => Value::mul,
                Kind::Divide => Value::div,
                Kind::FloorDivide => Value::floor_div,
                Kind::Modulo => Value::modulo,
                _ => break,
            };
            self.position += 1;
            if kind == Kind::Modulo
                && matches!(
                    self.peek().map(|token| &token.kind),
                    None | Some(Kind::RParen | Kind::Comma)
                )
            {
                return fail(
                    column,
                    "\"%\" is modulo and needs a number after it; for a percentage write 10/100",
                );
            }
            let right = self.unary()?;
            value = apply(&value, &right).map_err(|message| CalcError { column, message })?;
        }
        Ok(value)
    }

    fn unary(&mut self) -> Result<Value, CalcError> {
        match self.peek().map(|token| (token.kind.clone(), token.column)) {
            Some((Kind::Minus, column)) => {
                self.position += 1;
                self.descend(column)?;
                let value = self.unary()?;
                self.depth -= 1;
                Ok(value.neg())
            }
            Some((Kind::Plus, column)) => {
                self.position += 1;
                self.descend(column)?;
                let value = self.unary();
                self.depth -= 1;
                value
            }
            _ => self.power(),
        }
    }

    fn power(&mut self) -> Result<Value, CalcError> {
        let base = self.atom()?;
        match self.peek() {
            Some(Token {
                kind: Kind::Power,
                column,
                ..
            }) => {
                let column = *column;
                self.position += 1;
                self.descend(column)?;
                // The exponent may carry its own sign: 2^-1.
                let exponent = self.unary()?;
                self.depth -= 1;
                base.pow(&exponent)
                    .map_err(|message| CalcError { column, message })
            }
            _ => Ok(base),
        }
    }

    fn atom(&mut self) -> Result<Value, CalcError> {
        let end = self.end;
        let Some(token) = self.next().cloned() else {
            return fail(end, "expression ends early; add a number");
        };
        match token.kind {
            Kind::Number(value) => Ok(value),
            Kind::LParen => {
                self.descend(token.column)?;
                let value = self.expression()?;
                self.depth -= 1;
                match self.next() {
                    Some(Token {
                        kind: Kind::RParen, ..
                    }) => Ok(value),
                    Some(other) => {
                        let column = other.column;
                        fail(column, "missing operator between values; use * to multiply")
                    }
                    None => fail(
                        end,
                        format!(
                            "missing \")\"; the \"(\" at column {} is never closed",
                            token.column
                        ),
                    ),
                }
            }
            Kind::Name(name) => self.name(&name, token.column),
            Kind::RParen => fail(token.column, "unexpected \")\"; check the parentheses"),
            Kind::Comma => fail(token.column, "unexpected \",\"; add a number before it"),
            Kind::Modulo => fail(
                token.column,
                "\"%\" is modulo and needs a number on each side; for a percentage write 10/100",
            ),
            _ => fail(
                token.column,
                format!("expected a number before \"{}\"", token.text),
            ),
        }
    }

    fn name(&mut self, name: &str, column: usize) -> Result<Value, CalcError> {
        match name {
            "pi" | "π" => return Ok(Value::Float(std::f64::consts::PI)),
            "e" => return Ok(Value::Float(std::f64::consts::E)),
            _ => {}
        }
        let known = matches!(
            name,
            "sqrt"
                | "abs"
                | "round"
                | "floor"
                | "ceil"
                | "min"
                | "max"
                | "ln"
                | "log10"
                | "log2"
                | "exp"
        );
        if !known {
            let hint = match name {
                "log" => "\"log\" is ambiguous; use ln, log10 or log2".to_owned(),
                "x" => "use * to multiply".to_owned(),
                "mod" => "use % for modulo".to_owned(),
                "pow" => "use ^ for powers, such as 2^10".to_owned(),
                _ => format!(
                    "unknown name \"{name}\"; calculate only evaluates numbers, so substitute the value"
                ),
            };
            return fail(column, hint);
        }
        match self.next() {
            Some(Token {
                kind: Kind::LParen, ..
            }) => {}
            _ => {
                return fail(
                    column,
                    format!("{name} needs parentheses, such as {name}(2)"),
                );
            }
        }
        self.descend(column)?;
        let mut arguments = vec![self.expression()?];
        loop {
            let end = self.end;
            match self.next() {
                Some(Token {
                    kind: Kind::Comma, ..
                }) => arguments.push(self.expression()?),
                Some(Token {
                    kind: Kind::RParen, ..
                }) => break,
                Some(other) => {
                    let column = other.column;
                    return fail(column, "missing operator between values; use * to multiply");
                }
                None => return fail(end, format!("missing \")\" to close {name}(")),
            }
        }
        self.depth -= 1;
        call(name, &arguments).map_err(|message| CalcError { column, message })
    }
}

fn call(name: &str, arguments: &[Value]) -> Result<Value, String> {
    let one = || match arguments {
        [value] => Ok(value),
        _ => Err(format!("{name} takes 1 argument")),
    };
    match name {
        "sqrt" => one()?.sqrt(),
        "abs" => Ok(one()?.abs()),
        "floor" => one()?.floor(),
        "ceil" => one()?.ceil(),
        "round" => match arguments {
            [value] => value.round(0),
            [value, digits] => {
                let digits = digits
                    .as_integer()
                    .filter(|digits| (-12..=12).contains(digits));
                value.round(digits.ok_or("round(x, n) needs a whole n from -12 to 12")?)
            }
            _ => Err("round takes 1 or 2 arguments, such as round(2.345, 2)".into()),
        },
        "min" | "max" => {
            let mut best = arguments[0].clone();
            for value in &arguments[1..] {
                let better = if name == "min" {
                    value.to_f64() < best.to_f64()
                } else {
                    value.to_f64() > best.to_f64()
                };
                if better {
                    best = value.clone();
                }
            }
            Ok(best)
        }
        "ln" => one()?.log(f64::ln),
        "log10" => one()?.log(f64::log10),
        "log2" => one()?.log(f64::log2),
        "exp" => Value::float(one()?.to_f64().exp()).map_err(|error| error.message),
        _ => Err(format!("unknown function {name}")),
    }
}

// ---------------------------------------------------------------- values

/// An exact rational, or an `f64` once exactness is lost.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    /// `numerator / denominator`, reduced, with a positive denominator.
    Exact(i128, i128),
    Float(f64),
}

fn gcd(mut a: i128, mut b: i128) -> i128 {
    a = a.abs();
    b = b.abs();
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Value {
    fn exact(numerator: i128, denominator: i128) -> Option<Value> {
        if denominator == 0 {
            return None;
        }
        let divisor = gcd(numerator, denominator).max(1);
        let sign = if denominator < 0 { -1 } else { 1 };
        Some(Value::Exact(
            numerator.checked_div(divisor)?.checked_mul(sign)?,
            denominator.checked_div(divisor)?.checked_mul(sign)?,
        ))
    }

    fn float(value: f64) -> Result<Value, CalcError> {
        if value.is_nan() {
            return fail(1, "the result is undefined");
        }
        if value.is_infinite() {
            return fail(1, "the result is too large to represent");
        }
        Ok(Value::Float(value))
    }

    fn to_f64(&self) -> f64 {
        match self {
            Value::Exact(numerator, denominator) => *numerator as f64 / *denominator as f64,
            Value::Float(value) => *value,
        }
    }

    fn as_integer(&self) -> Option<i128> {
        match self {
            Value::Exact(numerator, 1) => Some(*numerator),
            Value::Float(value) if value.fract() == 0.0 && value.abs() < 1e15 => {
                Some(*value as i128)
            }
            _ => None,
        }
    }

    fn is_zero(&self) -> bool {
        self.to_f64() == 0.0
    }

    /// Apply an exact operation, falling back to `f64` on overflow.
    fn combine(
        &self,
        other: &Value,
        exact: impl Fn(i128, i128, i128, i128) -> Option<(i128, i128)>,
        float: impl Fn(f64, f64) -> f64,
    ) -> Result<Value, String> {
        if let (Value::Exact(a, b), Value::Exact(c, d)) = (self, other)
            && let Some(value) = exact(*a, *b, *c, *d).and_then(|(n, m)| Value::exact(n, m))
        {
            return Ok(value);
        }
        Value::float(float(self.to_f64(), other.to_f64())).map_err(|error| error.message)
    }

    fn add(&self, other: &Value) -> Result<Value, String> {
        self.combine(
            other,
            |a, b, c, d| {
                Some((
                    a.checked_mul(d)?.checked_add(c.checked_mul(b)?)?,
                    b.checked_mul(d)?,
                ))
            },
            |x, y| x + y,
        )
    }

    fn sub(&self, other: &Value) -> Result<Value, String> {
        self.add(&other.neg())
    }

    fn mul(&self, other: &Value) -> Result<Value, String> {
        self.combine(
            other,
            |a, b, c, d| Some((a.checked_mul(c)?, b.checked_mul(d)?)),
            |x, y| x * y,
        )
    }

    fn div(&self, other: &Value) -> Result<Value, String> {
        if other.is_zero() {
            return Err("division by zero".into());
        }
        self.combine(
            other,
            |a, b, c, d| Some((a.checked_mul(d)?, b.checked_mul(c)?)),
            |x, y| x / y,
        )
    }

    fn floor_div(&self, other: &Value) -> Result<Value, String> {
        self.div(other)?.floor()
    }

    /// Floored modulo: the result takes the divisor's sign.
    fn modulo(&self, other: &Value) -> Result<Value, String> {
        if other.is_zero() {
            return Err("modulo by zero".into());
        }
        let quotient = self.floor_div(other)?;
        self.sub(&other.mul(&quotient)?)
    }

    fn neg(&self) -> Value {
        match self {
            Value::Exact(numerator, denominator) => match numerator.checked_neg() {
                Some(negated) => Value::Exact(negated, *denominator),
                None => Value::Float(-self.to_f64()),
            },
            Value::Float(value) => Value::Float(-value),
        }
    }

    fn abs(&self) -> Value {
        if self.to_f64() < 0.0 {
            self.neg()
        } else {
            self.clone()
        }
    }

    fn pow(&self, exponent: &Value) -> Result<Value, String> {
        if let (Value::Exact(numerator, denominator), Some(power)) = (self, exponent.as_integer())
            && matches!(exponent, Value::Exact(..))
            && power.abs() <= MAX_EXACT_EXPONENT
        {
            if power < 0 && *numerator == 0 {
                return Err("division by zero".into());
            }
            let magnitude = u32::try_from(power.unsigned_abs()).unwrap_or(u32::MAX);
            let exact = numerator
                .checked_pow(magnitude)
                .zip(denominator.checked_pow(magnitude))
                .and_then(|(n, d)| {
                    if power < 0 {
                        Value::exact(d, n)
                    } else {
                        Value::exact(n, d)
                    }
                });
            if let Some(value) = exact {
                return Ok(value);
            }
        }
        let (base, power) = (self.to_f64(), exponent.to_f64());
        if base == 0.0 && power < 0.0 {
            return Err("division by zero".into());
        }
        if base < 0.0 && power.fract() != 0.0 {
            return Err("a negative number to a fractional power is not a real number".into());
        }
        Value::float(base.powf(power)).map_err(|error| error.message)
    }

    fn sqrt(&self) -> Result<Value, String> {
        if self.to_f64() < 0.0 {
            return Err("sqrt of a negative number is not a real number".into());
        }
        if let Value::Exact(numerator, denominator) = self
            && let (Some(n), Some(d)) = (integer_sqrt(*numerator), integer_sqrt(*denominator))
        {
            return Value::exact(n, d).ok_or_else(|| "division by zero".into());
        }
        Value::float(self.to_f64().sqrt()).map_err(|error| error.message)
    }

    fn log(&self, function: fn(f64) -> f64) -> Result<Value, String> {
        if self.to_f64() <= 0.0 {
            return Err("logarithms need a positive number".into());
        }
        Value::float(function(self.to_f64())).map_err(|error| error.message)
    }

    fn floor(&self) -> Result<Value, String> {
        match self {
            Value::Exact(numerator, denominator) => {
                Ok(Value::Exact(numerator.div_euclid(*denominator), 1))
            }
            Value::Float(value) => Value::float(value.floor()).map_err(|error| error.message),
        }
    }

    fn ceil(&self) -> Result<Value, String> {
        Ok(self.neg().floor()?.neg())
    }

    /// Round half away from zero to `digits` decimal places.
    fn round(&self, digits: i128) -> Result<Value, String> {
        let scale = Value::Exact(10, 1).pow(&Value::Exact(digits, 1))?;
        let scaled = self.mul(&scale)?;
        let half = Value::Exact(1, 2);
        let rounded = if scaled.to_f64() < 0.0 {
            scaled.sub(&half)?.ceil()?
        } else {
            scaled.add(&half)?.floor()?
        };
        rounded.div(&scale)
    }
}

/// The exact square root of a non-negative integer, if it has one.
fn integer_sqrt(value: i128) -> Option<i128> {
    if value < 0 {
        return None;
    }
    let mut root = (value as f64).sqrt() as i128;
    while root > 0 && root.checked_mul(root).is_none_or(|square| square > value) {
        root -= 1;
    }
    while (root + 1)
        .checked_mul(root + 1)
        .is_some_and(|square| square <= value)
    {
        root += 1;
    }
    (root * root == value).then_some(root)
}

fn format_value(value: &Value) -> Result<String, CalcError> {
    match value {
        Value::Exact(numerator, 1) => Ok(numerator.to_string()),
        Value::Exact(numerator, denominator) => Ok(terminating_decimal(*numerator, *denominator)
            .unwrap_or_else(|| format_float(value.to_f64()))),
        Value::Float(float) => {
            Value::float(*float)?;
            Ok(format_float(*float))
        }
    }
}

/// The exact decimal for a fraction whose denominator divides a power of
/// ten, up to 18 decimal places.
fn terminating_decimal(numerator: i128, denominator: i128) -> Option<String> {
    let mut places = 0;
    let mut power: i128 = 1;
    while power % denominator != 0 {
        places += 1;
        power = power.checked_mul(10)?;
        if places > 18 {
            return None;
        }
    }
    let scaled = numerator.checked_mul(power / denominator)?;
    let sign = if scaled < 0 { "-" } else { "" };
    let digits = scaled.unsigned_abs().to_string();
    let digits = format!("{digits:0>width$}", width = places + 1);
    let (whole, fraction) = digits.split_at(digits.len() - places);
    Some(format!("{sign}{whole}.{fraction}"))
}

/// About 12 significant digits, trailing zeros trimmed; scientific notation
/// for very large or very small magnitudes.
fn format_float(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let magnitude = value.abs();
    if !(1e-6..1e15).contains(&magnitude) {
        let text = format!("{:.*e}", SIGNIFICANT_DIGITS - 1, value);
        let (mantissa, exponent) = text.split_once('e').unwrap_or((&text, "0"));
        let mantissa = trim_zeros(mantissa);
        return format!("{mantissa}e{exponent}");
    }
    // Digits before the point; zero or negative for magnitudes below 1.
    let integer_digits = magnitude.log10().floor() as i64 + 1;
    let decimals = (SIGNIFICANT_DIGITS as i64 - integer_digits).max(0) as usize;
    trim_zeros(&format!("{value:.decimals$}"))
}

fn trim_zeros(text: &str) -> String {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(expression: &str) -> String {
        evaluate(expression)
            .map(|(_, value)| value)
            .unwrap_or_else(|error| panic!("{expression}: {error:?}"))
    }

    fn error(expression: &str) -> CalcError {
        evaluate(expression).expect_err(expression)
    }

    #[test]
    fn decimals_stay_exact() {
        assert_eq!(value("0.1 + 0.2"), "0.3");
        assert_eq!(value("1474 / 1.1"), "1340");
        assert_eq!(value("1480/1.1"), "1345.45454545");
        assert_eq!(value("1354/1.1"), "1230.90909091");
        assert_eq!(value("1/3"), "0.333333333333");
        assert_eq!(value("2/3"), "0.666666666667");
        assert_eq!(value("1/8"), "0.125");
        assert_eq!(value("10 - 0.3 * 3"), "9.1");
    }

    #[test]
    fn the_real_pacing_sum_and_division() {
        let lines =
            evaluate_batch("140+90+70+120+350+60+60+154+60+280+90\n1474/1.1; 1474/1100").unwrap();
        let outcomes: Vec<_> = lines
            .iter()
            .map(|line| line.outcome.clone().unwrap())
            .collect();
        assert_eq!(
            outcomes,
            [
                "140 + 90 + 70 + 120 + 350 + 60 + 60 + 154 + 60 + 280 + 90 = 1474",
                "1474 / 1.1 = 1340",
                "1474 / 1100 = 1.34",
            ]
        );
    }

    #[test]
    fn precedence_and_associativity() {
        assert_eq!(value("2 + 3 * 4"), "14");
        assert_eq!(value("(2 + 3) * 4"), "20");
        assert_eq!(value("2^3^2"), "512");
        assert_eq!(value("2**3**2"), "512");
        assert_eq!(value("-2^2"), "-4");
        assert_eq!(value("(-2)^2"), "4");
        assert_eq!(value("2^-1"), "0.5");
        assert_eq!(value("10 - 4 - 3"), "3");
        assert_eq!(value("64 / 4 / 2"), "8");
        assert_eq!(value("--3"), "3");
        assert_eq!(value("+3"), "3");
    }

    #[test]
    fn modulo_and_floor_division_are_floored() {
        assert_eq!(value("7 % 3"), "1");
        assert_eq!(value("-7 % 3"), "2");
        assert_eq!(value("7 // 2"), "3");
        assert_eq!(value("-7 // 2"), "-4");
        assert_eq!(value("5.5 % 2"), "1.5");
    }

    #[test]
    fn functions_and_constants() {
        assert_eq!(value("sqrt(16)"), "4");
        assert_eq!(value("sqrt(2)"), "1.41421356237");
        assert_eq!(value("sqrt(0.25)"), "0.5");
        assert_eq!(value("abs(-3.5)"), "3.5");
        assert_eq!(value("round(2.5)"), "3");
        assert_eq!(value("round(-2.5)"), "-3");
        assert_eq!(value("round(1345.4545, 2)"), "1345.45");
        assert_eq!(value("round(1234, -2)"), "1200");
        assert_eq!(value("floor(-1.5)"), "-2");
        assert_eq!(value("ceil(-1.5)"), "-1");
        assert_eq!(value("ceil(1.2)"), "2");
        assert_eq!(value("min(3, 1, 2)"), "1");
        assert_eq!(value("max(3, 1.5, 2)"), "3");
        assert_eq!(value("ln(e)"), "1");
        assert_eq!(value("log10(1000)"), "3");
        assert_eq!(value("log2(1024)"), "10");
        assert_eq!(value("exp(0)"), "1");
        assert_eq!(value("pi"), "3.14159265359");
        assert_eq!(value("2 * π"), "6.28318530718");
        assert_eq!(
            value("max(1,234)"),
            "234",
            "commas separate arguments inside a call"
        );
    }

    #[test]
    fn small_model_spellings_are_accepted() {
        assert_eq!(value("6 × 7"), "42");
        assert_eq!(value("84 ÷ 2"), "42");
        assert_eq!(value("50 − 8"), "42");
        assert_eq!(value("1,474 + 1"), "1475");
        assert_eq!(value("1,234,567"), "1234567");
        assert_eq!(value("1_000_000 / 4"), "250000");
        assert_eq!(value("1.5e3"), "1500");
        assert_eq!(value("2.5E-3 * 4"), "0.01");
        assert_eq!(value("1474 / 1.1 ="), "1340");
        assert_eq!(value(".5 + .25"), "0.75");
        let (normalised, _) = evaluate("1,474×2").unwrap();
        assert_eq!(normalised, "1474 * 2");
    }

    #[test]
    fn large_and_small_values() {
        assert_eq!(value("2^100"), "1267650600228229401496703205376");
        assert_eq!(
            value("123456789012345678901234567890 + 1"),
            "123456789012345678901234567891"
        );
        assert_eq!(value("2^200"), "1.60693804426e60");
        assert_eq!(value("1e-9 * 3"), "0.000000003");
        assert_eq!(value("1 / 3e9"), "3.33333333333e-10");
        assert_eq!(value("0.000001"), "0.000001");
        assert_eq!(value("10^-18"), "0.000000000000000001");
        assert_eq!(value("10^-20"), "1e-20");
    }

    #[test]
    fn errors_name_the_column_and_suggest_a_fix() {
        let unclosed = error("(1 + 2");
        assert_eq!(unclosed.column, 7);
        assert!(unclosed.message.contains("column 1 is never closed"));
        let extra = error("12*(3+4))");
        assert_eq!(
            (extra.column, extra.message.as_str()),
            (9, "unexpected \")\"; check the parentheses")
        );
        let name = error("pace * 1.1");
        assert_eq!(name.column, 1);
        assert!(name.message.contains("substitute the value"));
        assert!(error("log(10)").message.contains("ln, log10 or log2"));
        assert_eq!(error("1 / 0").message, "division by zero");
        assert_eq!(error("5 % 0").message, "modulo by zero");
        assert!(error("0^-1").message.contains("division by zero"));
        assert!(error("sqrt(-1)").message.contains("not a real number"));
        assert!(error("ln(0)").message.contains("positive"));
        assert!(error("(-8)^0.5").message.contains("fractional power"));
        assert!(error("10^400 * 10^400").message.contains("too large"));
        assert!(error("2 3").message.contains("use * to multiply"));
        assert!(error("2(3)").message.contains("use * to multiply"));
        assert!(error("1 +").message.contains("ends early"));
        assert!(error("10%").message.contains("percentage"));
        assert!(error("1,23").message.contains("use . for decimals"));
        assert!(error("2 = 3").message.contains("\"=\" is not supported"));
        assert_eq!(error("3 $ 4").column, 3);
        assert!(error("round(1, 2, 3)").message.contains("1 or 2 arguments"));
        assert!(error("sqrt 4").message.contains("parentheses"));
        assert!(error("exp(1000)").message.contains("too large"));
    }

    #[test]
    fn batches_report_each_line_and_respect_limits() {
        let lines = evaluate_batch("1 + 1\n\n  2 * (3 ; 4 / 2").unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].outcome, Ok("1 + 1 = 2".into()));
        assert!(
            lines[1]
                .outcome
                .as_ref()
                .unwrap_err()
                .starts_with("error at column")
        );
        assert_eq!(lines[1].input, "2 * (3");
        assert_eq!(lines[2].outcome, Ok("4 / 2 = 2".into()));
        assert!(
            evaluate_batch(" \n ; ")
                .unwrap_err()
                .contains("no expression")
        );
        assert!(
            evaluate_batch(&"1+".repeat(2_500))
                .unwrap_err()
                .contains("longer than")
        );
        assert!(
            evaluate_batch(&"1;".repeat(51))
                .unwrap_err()
                .contains("limit is 50")
        );
    }

    #[test]
    fn deep_nesting_is_refused_without_overflowing_the_stack() {
        let deep = format!("{}1{}", "(".repeat(1_000), ")".repeat(1_000));
        assert!(error(&deep).message.contains("nests too deeply"));
        assert!(
            error(&"-".repeat(1_000))
                .message
                .contains("nests too deeply")
        );
        assert_eq!(
            value(&format!("{}1{}", "(".repeat(30), ")".repeat(30))),
            "1"
        );
    }

    #[test]
    fn arbitrary_input_never_panics() {
        // A small fixed-seed generator keeps this deterministic without a
        // new dependency.
        let alphabet: Vec<char> = "0123456789.,_eE+-−*/×÷%^()=; \nsqrtlogminaxπpi1e9$"
            .chars()
            .collect();
        let mut state: u64 = 0x2545_f491_4f6c_dd1d;
        for _ in 0..20_000 {
            let mut text = String::new();
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let length = (state % 40) as usize;
            for _ in 0..length {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                text.push(alphabet[(state % alphabet.len() as u64) as usize]);
            }
            let _ = evaluate_batch(&text);
        }
    }
}

/// The `calculate` tool: a thin shell over [`evaluate_batch`].
#[derive(Debug, Clone, Default)]
pub struct CalculateTool;

#[async_trait::async_trait]
impl mojentic::llm::tools::LlmTool for CalculateTool {
    async fn run(
        &self,
        args: &std::collections::HashMap<String, serde_json::Value>,
        _ctx: &mojentic::llm::tools::ToolRunCtx,
    ) -> mojentic::Result<serde_json::Value> {
        let tool_error = |message: String| mojentic::MojenticError::ToolError(message);
        let expression = args
            .get("expression")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                tool_error("pass the arithmetic as a string in \"expression\", such as {\"expression\": \"1474 / 1.1\"}".into())
            })?;
        let lines = evaluate_batch(expression).map_err(tool_error)?;
        let rendered: Vec<String> = lines
            .iter()
            .map(|line| match &line.outcome {
                Ok(result) => result.clone(),
                Err(error) => format!("{}  ->  {error}", line.input),
            })
            .collect();
        if lines.iter().all(|line| line.outcome.is_ok()) {
            Ok(serde_json::json!({ "results": rendered }))
        } else {
            Err(tool_error(rendered.join("\n")))
        }
    }

    fn descriptor(&self) -> mojentic::llm::tools::ToolDescriptor {
        mojentic::llm::tools::ToolDescriptor {
            r#type: "function".to_string(),
            function: mojentic::llm::tools::FunctionDescriptor {
                name: "calculate".to_string(),
                description: "Evaluate arithmetic exactly. Use it for any arithmetic with more than one trivial step instead of computing in your head. Pass one expression, or several separated by newlines or \";\". Supports + - * / // % ^ and parentheses, sqrt abs round floor ceil min max ln log10 log2 exp, and pi e. Only numbers: substitute values for names. Example: {\"expression\": \"140+90+70\\n1474/1.1\"} returns \"140 + 90 + 70 = 300\" and \"1474 / 1.1 = 1340\".".to_string(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "expression": {
                            "type": "string",
                            "description": "One or more arithmetic expressions, separated by newlines or \";\"."
                        }
                    },
                    "required": ["expression"]
                }),
            },
        }
    }

    fn clone_box(&self) -> Box<dyn mojentic::llm::tools::LlmTool> {
        Box::new(self.clone())
    }
}
