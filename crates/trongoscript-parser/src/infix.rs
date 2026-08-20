//! `${ ... }` infix groups.
//!
//! Inside a group, operators sit between operands and parse with Ruby's
//! precedence. Outside one, Trongoscript has no infix at all.
//!
//! ```text
//! ${a + b * c}          => (+ a (* b c))
//! ${(price * qty) / 2}  => (/ (* price qty) 2)
//! ```
//!
//! The desugaring is the definition. `${a + b}` *is* `(+ a b)` — the same
//! [`Expr`], indistinguishable after read, so macros and `macroexpand` need
//! to know nothing about this syntax.
//!
//! The feature is affordable only because **the operator set is closed**.
//! Users cannot define new infix operators or change precedence, so the
//! parser never has to answer "what precedence does a user-defined `<~>`
//! get?" — a question with no good answer, and the reason general Lisps do
//! not ship a precedence table.

use nom::{character::complete::char as nom_char, error::VerboseError};

use crate::ast::Expr;
use crate::{form, ws, PResult};

/// One row of the precedence table: `(spelling, target, precedence)`.
///
/// `target` is the Trongoscript function the operator desugars to, and it is
/// not always the operator's own spelling. Trongoscript's stdlib names modulo
/// `mod` and inequality `not=`, while the conventional *infix* spellings are
/// `%` and `!=`. Both spellings are accepted and both produce the stdlib
/// name, so there is exactly one function underneath either way.
///
/// Higher precedence binds tighter.
type Op = (&'static str, &'static str, u8);

/// Ruby's precedence table, trimmed to operators Trongoscript's stdlib
/// actually has.
///
/// Omitted deliberately: `**`, bitwise operators, `<=>`, `=~`, `..`, and
/// every assignment operator. Trongoscript has no assignment, no regex, no
/// ranges, and no mutation, so those rows have nothing to desugar to.
///
/// Order is longest-match: `<=` must be tried before `<`, and `>=` before
/// `>`, or the shorter operator would win and leave a stray `=`.
#[rustfmt::skip]
const OPERATORS: &[Op] = &[
    ("*",    "*",    5),
    ("/",    "/",    5),
    ("%",    "mod",  5),
    ("mod",  "mod",  5),
    ("+",    "+",    4),
    ("-",    "-",    4),
    (">=",   ">=",   3),
    ("<=",   "<=",   3),
    (">",    ">",    3),
    ("<",    "<",    3),
    ("!=",   "not=", 2),
    ("not=", "not=", 2),
    ("=",    "=",    2),
    ("and",  "and",  1),
    ("or",   "or",   0),
];

fn err<'a>(input: &'a str, msg: &'static str) -> nom::Err<VerboseError<&'a str>> {
    nom::Err::Error(VerboseError {
        errors: vec![(input, nom::error::VerboseErrorKind::Context(msg))],
    })
}

fn fail<'a>(input: &'a str, msg: &'static str) -> nom::Err<VerboseError<&'a str>> {
    nom::Err::Failure(VerboseError {
        errors: vec![(input, nom::error::VerboseErrorKind::Context(msg))],
    })
}

/// Match an infix operator at the head of `input`.
///
/// Word operators (`and`, `or`, `mod`, `not=`) must not match a prefix of a
/// longer symbol: `${android or x}` has to read `android` as an operand, not
/// as `and` followed by `roid`. Symbolic operators need no such guard, since
/// no symbol may begin with one in operator position.
fn match_operator(input: &str) -> Option<(&'static str, u8, &str)> {
    for (spelling, target, prec) in OPERATORS {
        let Some(rest) = input.strip_prefix(spelling) else {
            continue;
        };
        let is_word = spelling.chars().all(|c| c.is_ascii_alphabetic());
        if is_word {
            // `not=` ends in `=`, so it is not all-alphabetic and skips this
            // check, which is correct — `not=x` is unambiguous.
            match rest.chars().next() {
                Some(c) if crate::is_symbol_char(c) => continue,
                _ => {}
            }
        }
        return Some((*target, *prec, rest));
    }
    None
}

/// True if `e` is a bare symbol whose name is an infix operator spelling.
///
/// This is the discriminator for the parenthesis rule in [`paren_operand`].
fn is_operator_symbol(e: &Expr) -> bool {
    let Expr::Symbol(s) = e else { return false };
    OPERATORS.iter().any(|(spelling, _, _)| spelling == s)
}

/// Parse `${ ... }` — one infix group.
///
/// Exactly one group per `${...}`. djrb allows several because it has
/// assignment and constructors full of `@x = x` lines; Trongoscript has
/// neither, and forbidding it deletes the group-boundary rule, the
/// "every group must contain an operator" rule, and the `${@x = 1  -y}`
/// hazard along with them.
pub(crate) fn infix_group(input: &str) -> PResult<Expr> {
    let Some(rest) = input.strip_prefix("${") else {
        return Err(err(input, "expected `${`"));
    };
    let (rest, _) = ws(rest)?;
    // Everything past the opening `${` is committed: a malformed group is a
    // hard error, not a backtrack into `symbol_form` that would munch the
    // `$` and leave a baffling message.
    let (rest, e) = climb(rest, 0)?;
    let (rest, _) = ws(rest)?;
    let Ok((rest, _)) = nom_char::<&str, VerboseError<&str>>('}')(rest) else {
        return Err(fail(
            rest,
            "expected `}` — an infix group holds exactly one expression",
        ));
    };
    Ok((rest, e))
}

/// Precedence climbing. `min_prec` is the lowest binding power this call
/// will accept; operators below it belong to an enclosing call.
///
/// Every operator here is left-associative, so the recursive call takes
/// `prec + 1`.
fn climb(input: &str, min_prec: u8) -> PResult<Expr> {
    let (mut rest, mut lhs) = operand(input)?;

    loop {
        let (after_ws, _) = ws(rest)?;
        let Some((target, prec, after_op)) = match_operator(after_ws) else {
            break;
        };
        if prec < min_prec {
            break;
        }
        let (after_op, _) = ws(after_op)?;
        let (next, rhs) = climb(after_op, prec + 1)?;
        lhs = Expr::List(vec![Expr::Symbol(target.to_string()), lhs, rhs]);
        rest = next;
    }

    Ok((rest, lhs))
}

/// One operand, plus any leading unary operators.
///
/// Unary minus parses **only** here — at the start of a group or straight
/// after another operator. A `-` with an operand to its left is binary
/// subtraction, always. That keeps the group whitespace-insensitive, which
/// matters because Trongoscript is whitespace-insensitive everywhere else
/// and an `a -b` / `a - b` distinction would be semantic whitespace under
/// another name.
fn operand(input: &str) -> PResult<Expr> {
    if let Some(rest) = input.strip_prefix('-') {
        let (rest, _) = ws(rest)?;
        let (rest, e) = operand(rest)?;
        // `(- x)` is the stdlib's negation: `-` with one argument.
        return Ok((rest, Expr::List(vec![Expr::Symbol("-".to_string()), e])));
    }
    if let Some(rest) = input.strip_prefix("not") {
        if !rest.starts_with(crate::is_symbol_char) {
            let (rest, _) = ws(rest)?;
            let (rest, e) = operand(rest)?;
            return Ok((rest, Expr::List(vec![Expr::Symbol("not".to_string()), e])));
        }
    }

    // A nested group is an ordinary operand, so `${a + ${b}}` works, though
    // nobody should write it.
    if input.starts_with("${") {
        return infix_group(input);
    }

    if input.starts_with('(') {
        return paren_operand(input);
    }

    let (rest, e) = form(input)?;
    Ok((rest, e))
}

/// A parenthesised operand.
///
/// Inside `${...}`, parentheses **group**; they do not call. `${(a + b) / 2}`
/// is `(/ (+ a b) 2)`.
///
/// djrb learned this rule from a real bug: without it, `${(a + b) / 2}` reads
/// as a call to the function `a` with arguments `+` and `b`, which parses,
/// round-trips, and silently loses the arithmetic. The discriminator is what
/// sits in second position — an infix operator there means grouping, anything
/// else keeps the ordinary call reading, so `${(max 1 cpu) * 4}` still calls
/// `max`.
fn paren_operand(input: &str) -> PResult<Expr> {
    // Try the grouping reading first, and only commit to it if the contents
    // actually look infix. `probe` is a cheap lookahead: read one form, then
    // check whether an operator follows.
    let (after_open, _) = nom_char::<&str, VerboseError<&str>>('(')(input)?;
    let (probe, _) = ws(after_open)?;

    if let Ok((after_first, _first)) = form(probe) {
        let (after_first_ws, _) = ws(after_first)?;
        // What sits in *second* position decides. An infix operator there
        // means grouping; anything else keeps the ordinary call reading, so
        // `${(max 1 cpu) * 4}` still calls `max`. The check reads the second
        // form rather than string-matching, so a symbol that merely starts
        // with an operator's spelling cannot fool it.
        let second_is_operator = form(after_first_ws)
            .map(|(_, second)| is_operator_symbol(&second))
            .unwrap_or(false);
        if second_is_operator {
            // Infix in second position: this is a grouping paren.
            let (rest, e) = climb(probe, 0)?;
            let (rest, _) = ws(rest)?;
            let Ok((rest, _)) = nom_char::<&str, VerboseError<&str>>(')')(rest) else {
                return Err(fail(
                    rest,
                    "expected `)` closing a grouped infix expression",
                ));
            };
            return Ok((rest, e));
        }
    }

    // A unary operator directly after `(` also means grouping: `${(-x) + 1}`.
    if probe.starts_with('-')
        || (probe.starts_with("not") && !probe[3..].starts_with(crate::is_symbol_char))
    {
        let (rest, e) = climb(probe, 0)?;
        let (rest, _) = ws(rest)?;
        let Ok((rest, _)) = nom_char::<&str, VerboseError<&str>>(')')(rest) else {
            return Err(fail(
                rest,
                "expected `)` closing a grouped infix expression",
            ));
        };
        return Ok((rest, e));
    }

    // Otherwise it is an ordinary call, parsed by the ordinary reader.
    form(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_expr;
    use pretty_assertions::assert_eq;

    /// The desugaring is the definition, so every case asserts the sugar and
    /// its s-expression form parse to the *same* AST.
    fn same(sugar: &str, sexp: &str) {
        let lhs = parse_expr(sugar).unwrap_or_else(|e| panic!("{:?} failed: {}", sugar, e));
        let rhs = parse_expr(sexp).unwrap_or_else(|e| panic!("{:?} failed: {}", sexp, e));
        assert_eq!(lhs, rhs, "{:?} should equal {:?}", sugar, sexp);
    }

    #[test]
    fn precedence() {
        same("${a + b * c}", "(+ a (* b c))");
        same("${a * b + c}", "(+ (* a b) c)");
        same("${a + b + c}", "(+ (+ a b) c)");
        same("${a - b - c}", "(- (- a b) c)");
        same("${a < b and c < d}", "(and (< a b) (< c d))");
        same("${a and b or c}", "(or (and a b) c)");
        same("${total-ram * 3 / 4}", "(/ (* total-ram 3) 4)");
    }

    #[test]
    fn stdlib_spellings() {
        // `%` and `!=` are the infix spellings; the stdlib names are `mod`
        // and `not=`, and both surface forms produce the stdlib name.
        same("${a % b}", "(mod a b)");
        same("${a mod b}", "(mod a b)");
        same("${a != b}", "(not= a b)");
        same("${a not= b}", "(not= a b)");
        same("${a = b}", "(= a b)");
    }

    #[test]
    fn parens_group_they_do_not_call() {
        same("${(a + b) / 2}", "(/ (+ a b) 2)");
        same("${(price * qty) / 2}", "(/ (* price qty) 2)");
    }

    #[test]
    fn parens_still_call_when_head_is_not_infix() {
        same("${(max 1 cpu-count) * 4}", "(* (max 1 cpu-count) 4)");
        same("${(f x) + (g y)}", "(+ (f x) (g y))");
    }

    #[test]
    fn minus_is_binary_whenever_it_has_a_left_operand() {
        same("${a - b}", "(- a b)");
        // A space on only one side must not change the reading, or the
        // group would have semantic whitespace, which Trongoscript has
        // nowhere else.
        same("${a -b}", "(- a b)");
        same("${a - -b}", "(- a (- b))");
        same("${a - b - c}", "(- (- a b) c)");
    }

    /// The one place this diverges from djrb's whitespace-insensitivity
    /// rule, and it is forced: Trongoscript identifiers are kebab-case, so
    /// `-` is a legal symbol character and `a-b` is a *name*.
    ///
    /// `total-ram`, `cpu-count`, and `parse-int` all have to keep working
    /// inside a group, so a `-` with no space around it belongs to the
    /// symbol. Subtraction between two bare names needs a space, which is
    /// how anyone would write it anyway.
    #[test]
    fn kebab_case_names_survive_inside_a_group() {
        assert_eq!(parse_expr("${a-b}").unwrap(), Expr::Symbol("a-b".into()));
        assert_eq!(
            parse_expr("${total-ram}").unwrap(),
            Expr::Symbol("total-ram".into())
        );
        // And they still take part in arithmetic.
        same("${total-ram * 3 / 4}", "(/ (* total-ram 3) 4)");
        same("${max-conn - min-conn}", "(- max-conn min-conn)");
        same("${cpu-count - 1}", "(- cpu-count 1)");
        // Even with a digit on the right: `a-1` is a legal symbol name and
        // the lexer takes it whole. Subtraction needs the space.
        assert_eq!(parse_expr("${a-1}").unwrap(), Expr::Symbol("a-1".into()));
    }

    #[test]
    fn unary_at_group_start_and_after_operator() {
        same("${-a}", "(- a)");
        same("${-a + b}", "(+ (- a) b)");
        same("${a * -b}", "(* a (- b))");
        same("${not a}", "(not a)");
        same("${not a and b}", "(and (not a) b)");
    }

    #[test]
    fn word_operators_do_not_eat_symbol_prefixes() {
        // `android` is one symbol, not `and` + `roid`.
        assert_eq!(
            parse_expr("${android}").unwrap(),
            Expr::Symbol("android".into())
        );
        same("${ordinal + 1}", "(+ ordinal 1)");
        same("${modulus + 1}", "(+ modulus 1)");
        same("${nothing + 1}", "(+ nothing 1)");
    }

    #[test]
    fn nested_groups_and_calls() {
        same("${(f (g x)) + 1}", "(+ (f (g x)) 1)");
        same("${${a + b} * 2}", "(* (+ a b) 2)");
    }

    #[test]
    fn one_group_per_dollar_brace() {
        // Two expressions in one group is a parse error, not a sequence.
        assert!(parse_expr("${a + b  c + d}").is_err());
        assert!(parse_expr("${a b}").is_err());
    }

    #[test]
    fn a_group_may_hold_a_single_operand() {
        // Not required to contain an operator; `${x}` is just `x`.
        assert_eq!(parse_expr("${x}").unwrap(), Expr::Symbol("x".into()));
        assert_eq!(parse_expr("${42}").unwrap(), Expr::Int(42));
    }

    #[test]
    fn dollar_is_still_a_symbol_char_on_its_own() {
        // `${` is a two-char check, so a bare `$` keeps its old reading.
        assert_eq!(parse_expr("$").unwrap(), Expr::Symbol("$".into()));
        assert_eq!(parse_expr("$foo").unwrap(), Expr::Symbol("$foo".into()));
    }

    #[test]
    fn unterminated_group_is_an_error() {
        assert!(parse_expr("${a + b").is_err());
        assert!(parse_expr("${}").is_err());
    }

    #[test]
    fn groups_nest_in_ordinary_forms() {
        same("(def x ${a + 1})", "(def x (+ a 1))");
        same("{:n ${a * 2}}", "{:n (* a 2)}");
        same("[${a + 1} ${b + 2}]", "[(+ a 1) (+ b 2)]");
    }

    /// The full precedence ladder in one expression, so a change to any row
    /// of `OPERATORS` that shifts a level relative to its neighbours fails
    /// here rather than silently changing what a config computes.
    ///
    /// Levels, tightest first: `* / %`, `+ -`, `< <= > >=`, `= !=`, `and`,
    /// `or`.
    #[test]
    fn the_whole_precedence_ladder() {
        same(
            "${a or b and c = d < e + f * g}",
            "(or a (and b (= c (< d (+ e (* f g))))))",
        );
        // And the same shape written in the other direction.
        same(
            "${a * b + c < d = e and f or g}",
            "(or (and (= (< (+ (* a b) c) d) e) f) g)",
        );
    }

    /// Each adjacent pair of levels, checked on its own. A single ladder
    /// test says the order is right; these say *which* pair broke when it
    /// is not.
    #[test]
    fn adjacent_precedence_levels() {
        same("${a * b + c}", "(+ (* a b) c)"); // * over +
        same("${a + b < c}", "(< (+ a b) c)"); // + over <
        same("${a < b = c}", "(= (< a b) c)"); // < over =
        same("${a = b and c}", "(and (= a b) c)"); // = over and
        same("${a and b or c}", "(or (and a b) c)"); // and over or
                                                     // `%` sits with `*`, not between it and `+`.
        same("${a % b + c}", "(+ (mod a b) c)");
        same("${a + b % c}", "(+ a (mod b c))");
    }

    /// Every operator is left-associative, including the ones where it
    /// matters least — a right-associative `-` or `/` would quietly change
    /// arithmetic.
    #[test]
    fn every_operator_is_left_associative() {
        same("${a - b - c}", "(- (- a b) c)");
        same("${a / b / c}", "(/ (/ a b) c)");
        same("${a % b % c}", "(mod (mod a b) c)");
        same("${a + b + c}", "(+ (+ a b) c)");
        same("${a * b * c}", "(* (* a b) c)");
        same("${a and b and c}", "(and (and a b) c)");
        same("${a or b or c}", "(or (or a b) c)");
        // Comparisons chain structurally too. `${1 < 2 < 3}` reads as
        // `(< (< 1 2) 3)` and is a *runtime* type error, not a parse error
        // — the parser has no opinion about chained comparison.
        same("${a < b < c}", "(< (< a b) c)");
        same("${a = b = c}", "(= (= a b) c)");
    }

    /// Parenthesised grouping overrides precedence, which is the reason to
    /// write it.
    #[test]
    fn grouping_parens_override_precedence() {
        same("${(a + b) * c}", "(* (+ a b) c)");
        same("${a * (b + c)}", "(* a (+ b c))");
        same("${(a or b) and c}", "(and (or a b) c)");
        same("${((a + b) * c) - d}", "(- (* (+ a b) c) d)");
    }

    /// Every operand kind, not just symbols. A group whose operands are
    /// literals is the common case in a config file.
    #[test]
    fn operands_may_be_any_literal() {
        same("${1 + 2}", "(+ 1 2)");
        same("${1.5 * 2}", "(* 1.5 2)");
        same(r#"${"a" = "b"}"#, r#"(= "a" "b")"#);
        same("${:a = :b}", "(= :a :b)");
        same("${true and false}", "(and true false)");
        same("${nil = nil}", "(= nil nil)");
        same("${[1 2] = [1 2]}", "(= [1 2] [1 2])");
        same("${{:a 1} = {:a 1}}", "(= {:a 1} {:a 1})");
    }

    /// Unary stacks, and binds tighter than every binary operator.
    #[test]
    fn unary_stacks_and_binds_tightest() {
        same("${- -a}", "(- (- a))");
        same("${not not a}", "(not (not a))");
        same("${-a * b}", "(* (- a) b)");
        same("${not a and not b}", "(and (not a) (not b))");
        // A unary directly after `(` means the parens group.
        same("${(-a) + b}", "(+ (- a) b)");
        same("${(not a) or b}", "(or (not a) b)");
    }

    /// An operator needs whitespace **on its left** unless the left operand
    /// ends in a character a symbol cannot contain.
    ///
    /// This generalizes `kebab_case_names_survive_inside_a_group`, and the
    /// generalization is the surprising part: the rule is not specific to
    /// `-`. Every symbolic operator (`+ * / % < > =`) is a legal symbol
    /// character, and so is every digit, so the lexer takes `a+b` and `1+2`
    /// whole for exactly the reason it takes `total-ram` whole.
    ///
    /// The consequence is the one way a group can quietly mean something
    /// other than it looks like, so it is stated here rather than left to be
    /// discovered: **`${a+b}` and `${1+2}` are names, not sums.**
    #[test]
    fn an_operator_needs_space_on_its_left_after_a_symbol_char() {
        for src in [
            "${a+b}", "${a*b}", "${a/b}", "${a%b}", "${a<b}",
            "${a=b}", // Digits are symbol characters too, so numeric operands are no
            // different.
            "${1+2}", "${1<2}", "${1.5+2}",
        ] {
            let parsed = parse_expr(src).unwrap();
            assert!(
                matches!(parsed, Expr::Symbol(_)),
                "{} should read as one symbol, got {:?}",
                src,
                parsed
            );
        }
        // Spaced, they are the operators they look like.
        same("${a + b}", "(+ a b)");
        same("${1 + 2}", "(+ 1 2)");
    }

    /// Space on the *left* alone is enough: once the left operand has ended,
    /// the operator is unambiguous and its right side may run straight into
    /// the next operand.
    #[test]
    fn space_on_the_left_of_an_operator_is_sufficient() {
        same("${1 +2}", "(+ 1 2)");
        same("${a +b}", "(+ a b)");
        same("${a *b}", "(* a b)");
    }

    /// Space on the *right* alone is not enough, and it does not silently
    /// misparse either — `1+` becomes a symbol, leaving `2` with no operator
    /// before it, and a group holding two expressions is an error.
    ///
    /// Failing loudly here is the point: the alternative is `${a+ b}`
    /// quietly reading as something other than a sum.
    #[test]
    fn space_on_only_the_right_of_an_operator_is_an_error() {
        assert!(parse_expr("${1+ 2}").is_err());
        assert!(parse_expr("${a+ b}").is_err());
    }

    /// When the left operand ends in a character no symbol may contain — a
    /// closing paren, or a closing quote — the operator is unambiguous with
    /// no space at all.
    #[test]
    fn no_space_is_needed_after_a_paren_or_string() {
        same("${(a + b)*2}", "(* (+ a b) 2)");
        same("${(a + b)/2}", "(/ (+ a b) 2)");
        same(r#"${"x"="y"}"#, r#"(= "x" "y")"#);
    }

    /// Extra and irregular whitespace, including newlines, never changes a
    /// reading.
    #[test]
    fn extra_whitespace_never_matters() {
        same("${  a  +  b  *  c  }", "(+ a (* b c))");
        same("${\n  a\n  +\n  b\n}", "(+ a b)");
    }

    /// Comments and `#_` discards are ordinary whitespace to the reader, and
    /// the reader runs inside a group too.
    #[test]
    fn comments_and_discards_inside_a_group() {
        same("${1 + ; trailing\n 2}", "(+ 1 2)");
        same("${1 + #_ 99 2}", "(+ 1 2)");
    }

    /// A word operator must not be eaten by an adjacent symbol on the
    /// *right* either, and a symbol that merely ends with an operator's
    /// spelling stays one symbol.
    #[test]
    fn word_operators_are_bounded_on_both_sides() {
        same("${a and bandage}", "(and a bandage)");
        same("${command + 1}", "(+ command 1)");
        assert_eq!(parse_expr("${band}").unwrap(), Expr::Symbol("band".into()));
        assert_eq!(
            parse_expr("${normal}").unwrap(),
            Expr::Symbol("normal".into())
        );
    }

    /// Malformed groups are hard errors rather than a backtrack that leaves
    /// the `$` to be munched into a symbol and produces a baffling message
    /// much later.
    #[test]
    fn malformed_groups_are_errors() {
        // A trailing operator with no right operand.
        assert!(parse_expr("${a +}").is_err());
        assert!(parse_expr("${a and}").is_err());
        // A leading binary operator with no left operand.
        assert!(parse_expr("${* a}").is_err());
        assert!(parse_expr("${/ a}").is_err());
        // An unclosed grouping paren.
        assert!(parse_expr("${(a + b}").is_err());
        // Nothing at all.
        assert!(parse_expr("${ }").is_err());
    }

    /// Groups appear anywhere a form does, including binding positions and
    /// nested data — which is most of a config document.
    #[test]
    fn groups_appear_wherever_a_form_does() {
        same("(let [x ${a + 1}] x)", "(let [x (+ a 1)] x)");
        same("(f ${a + 1} ${b * 2})", "(f (+ a 1) (* b 2))");
        same("{:a {:b ${n / 2}}}", "{:a {:b (/ n 2)}}");
        same("(if ${a > b} x y)", "(if (> a b) x y)");
    }

    /// A group's output is an ordinary call, so it prints and re-reads as
    /// one. The sugar leaves no trace in the AST to survive a round trip.
    #[test]
    fn a_group_round_trips_as_its_desugaring() {
        for src in [
            "${a + b}",
            "${a + b * c}",
            "${(a + b) / 2}",
            "${-a}",
            "${not a}",
        ] {
            let parsed = parse_expr(src).unwrap();
            let printed = parsed.to_string();
            assert!(
                !printed.contains("${"),
                "{} printed as {}, which still contains sugar",
                src,
                printed
            );
            assert_eq!(
                parse_expr(&printed).unwrap(),
                parsed,
                "{} printed as {}",
                src,
                printed
            );
        }
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
