//! End-to-end tests covering parse → evaluate → sandboxed I/O →
//! template rendering → embedded substitution.

use std::collections::HashMap;
use std::path::PathBuf;

use trongoscript::{
    eval_source, evaluate_embedded_yaml, render_template, Context, EvalConfig, EvalError, Sandbox,
    Value,
};

fn examples_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is `crates/trongoscript`.
    let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    p.pop();
    p.pop();
    p.push("examples");
    p
}

fn eval(src: &str) -> Value {
    let mut ctx = Context::new(EvalConfig::default());
    eval_source(src, &mut ctx).expect("eval failed")
}

#[test]
fn evaluates_config_example() {
    let src = std::fs::read_to_string(examples_dir().join("config.trongo")).unwrap();
    let v = eval(&src);
    let obj = v.as_object().unwrap();
    assert_eq!(obj["app"], Value::String("trongoscript-demo".into()));
    assert_eq!(obj["env"], Value::String("production".into()));
    assert_eq!(obj["port"], Value::Number(443.into()));
    assert_eq!(obj["hosts"].as_array().unwrap().len(), 3);
    assert_eq!(obj["auth"]["enabled"], Value::Bool(true));
    assert_eq!(obj["tags"].as_array().unwrap().len(), 3);
}

#[test]
fn include_with_sandbox() {
    let root = examples_dir().join("with_includes");
    let sb = Sandbox::new(&root).expect("sandbox");
    let mut ctx = Context::new(EvalConfig::default().with_sandbox(sb));
    let src = std::fs::read_to_string(root.join("main.trongo")).unwrap();

    // Tell the evaluator which directory "main" lives in so relative
    // includes resolve.
    ctx.current_file_dir = Some(root.clone());

    let v = eval_source(&src, &mut ctx).expect("eval");
    assert_eq!(v["name"], Value::String("PRODUCTION".into()));
    assert_eq!(v["db"]["host"], Value::String("db.internal".into()));
    assert_eq!(v["db"]["port"], Value::Number(5432.into()));
    assert_eq!(v["cache"]["backend"], Value::String("redis".into()));
}

#[test]
fn include_blocked_without_sandbox() {
    let mut ctx = Context::new(EvalConfig::default());
    let r = eval_source(r#"(include "anything.trongo")"#, &mut ctx);
    assert!(matches!(r, Err(EvalError::NoSandbox)));
}

#[test]
fn include_blocked_outside_root() {
    let root = examples_dir().join("with_includes");
    let sb = Sandbox::new(&root).unwrap();
    let mut ctx = Context::new(EvalConfig::default().with_sandbox(sb));
    ctx.current_file_dir = Some(root);

    // Try to escape via `..` — should be rejected.
    let r = eval_source(r#"(include "../config.trongo")"#, &mut ctx);
    assert!(r.is_err(), "expected sandbox rejection, got {:?}", r);
}

#[test]
fn include_absolute_path_cannot_reach_host_root() {
    let root = examples_dir().join("with_includes");
    let sb = Sandbox::new(&root).unwrap();
    let mut ctx = Context::new(EvalConfig::default().with_sandbox(sb));
    // `/etc/passwd` is absolute in *sandbox* space: it means
    // `<root>/etc/passwd`, which doesn't exist. The host's real
    // /etc/passwd does exist, so a leak would succeed rather than fail.
    let r = eval_source(r#"(include "/etc/passwd")"#, &mut ctx);
    assert!(r.is_err());

    // Read the host file directly to be sure the failure above isn't just
    // this machine lacking /etc/passwd.
    if let Ok(host) = std::fs::read_to_string("/etc/passwd") {
        let mut ctx =
            Context::new(EvalConfig::default().with_sandbox(Sandbox::new(&root).unwrap()));
        let v = eval_source(r#"(read-file "/etc/passwd")"#, &mut ctx);
        match v {
            Err(_) => {}
            Ok(got) => panic!(
                "sandbox leaked the host /etc/passwd (matches host: {})",
                got == serde_json::Value::String(host)
            ),
        }
    }
}

#[test]
fn bind_mount_exposes_a_second_tree() {
    // ./bob is the sandbox root; ./tom is mounted at /tom. Scripts reach
    // ./bob/x as /x and ./tom/y as /tom/y.
    let tmp = std::env::temp_dir().join(format!(
        "trongo-mount-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(tmp.join("bob")).unwrap();
    std::fs::create_dir_all(tmp.join("tom")).unwrap();
    std::fs::write(tmp.join("bob/x"), "from-bob").unwrap();
    std::fs::write(tmp.join("tom/y"), "from-tom").unwrap();
    std::fs::write(tmp.join("tom/lib.trongo"), r#"{:who "tom"}"#).unwrap();

    let sb = Sandbox::new(tmp.join("bob"))
        .unwrap()
        .mount("/tom", tmp.join("tom"))
        .unwrap();
    let mut ctx = Context::new(EvalConfig::default().with_sandbox(sb));

    let v = eval_source(
        r#"{:x (read-file "/x") :y (read-file "/tom/y") :lib (include "/tom/lib.trongo")}"#,
        &mut ctx,
    )
    .expect("eval");
    assert_eq!(v["x"], serde_json::Value::String("from-bob".into()));
    assert_eq!(v["y"], serde_json::Value::String("from-tom".into()));
    assert_eq!(v["lib"]["who"], serde_json::Value::String("tom".into()));

    // A script in the root mount cannot climb sideways into ./tom on disk.
    let r = eval_source(r#"(read-file "../tom/y")"#, &mut ctx);
    assert!(matches!(r, Err(EvalError::Sandbox(_))), "got {:?}", r);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn include_inside_a_mount_stays_in_that_mount() {
    let tmp = std::env::temp_dir().join(format!(
        "trongo-mount-esc-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(tmp.join("bob")).unwrap();
    std::fs::create_dir_all(tmp.join("tom")).unwrap();
    std::fs::write(tmp.join("bob/secret.trongo"), r#"{:leaked true}"#).unwrap();
    // A file in the mount that tries to relative-include out of it.
    std::fs::write(
        tmp.join("tom/evil.trongo"),
        r#"(include "../bob/secret.trongo")"#,
    )
    .unwrap();

    let sb = Sandbox::new(tmp.join("bob"))
        .unwrap()
        .mount("/tom", tmp.join("tom"))
        .unwrap();
    let mut ctx = Context::new(EvalConfig::default().with_sandbox(sb));

    let r = eval_source(r#"(include "/tom/evil.trongo")"#, &mut ctx);
    assert!(matches!(r, Err(EvalError::Sandbox(_))), "got {:?}", r);

    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn template_renders_with_loop_like_structure() {
    let src = std::fs::read_to_string(examples_dir().join("template.erb")).unwrap();
    let mut ctx = Context::new(EvalConfig::default());
    let out = render_template(&src, &mut ctx).expect("render");
    assert!(out.contains("Hello, Alice!"));
    assert!(out.contains("apples, oranges, pears"));
    assert!(out.contains("Total fruits: 3"));
}

#[test]
fn embedded_yaml() {
    let raw = std::fs::read_to_string(examples_dir().join("embedded.yaml")).unwrap();
    let root: serde_yaml::Value = serde_yaml::from_str(&raw).unwrap();
    let mut ctx = Context::new(EvalConfig::default());
    let v = evaluate_embedded_yaml(root, &mut ctx).expect("embed eval");
    assert_eq!(v["title"], Value::String("PRODUCTION ENVIRONMENT".into()));
    assert_eq!(v["counts"]["servers"], Value::Number(12.into()));
    assert_eq!(v["counts"]["shards"], Value::Number(16.into()));
}

#[test]
fn host_builtin_can_be_registered() {
    let host = std::sync::Arc::new(|_ctx: &mut Context, args: &[Value]| {
        let n = args.iter().filter_map(|v| v.as_i64()).sum::<i64>();
        Ok(Value::Number((n * 100).into()))
    });
    let cfg = EvalConfig::default().with_host_builtin("scale-sum", host);
    let mut ctx = Context::new(cfg);
    let v = eval_source("(scale-sum 1 2 3)", &mut ctx).unwrap();
    assert_eq!(v, Value::Number(600.into()));
}

#[test]
fn top_level_data_passes_in() {
    let mut data = HashMap::new();
    data.insert("user-name".to_string(), Value::String("bob".into()));
    let mut ctx = Context::new(EvalConfig::default()).with_data(data);
    let v = eval_source(r#"(upcase user-name)"#, &mut ctx).unwrap();
    assert_eq!(v, Value::String("BOB".into()));
}

#[test]
fn include_cycle_detected() {
    use std::fs;
    let tmp = std::env::temp_dir().join(format!(
        "trongo-cycle-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&tmp).unwrap();
    fs::write(tmp.join("a.trongo"), r#"(include "b.trongo")"#).unwrap();
    fs::write(tmp.join("b.trongo"), r#"(include "a.trongo")"#).unwrap();

    let sb = Sandbox::new(&tmp).unwrap();
    let mut ctx = Context::new(EvalConfig::default().with_sandbox(sb));
    ctx.current_file_dir = Some(tmp.clone());

    let r = eval_source(r#"(include "a.trongo")"#, &mut ctx);
    assert!(matches!(r, Err(EvalError::Sandbox(_))));

    let _ = fs::remove_dir_all(&tmp);
}

// ---------------------------------------------------------------------------
// `(use ...)` module system
// ---------------------------------------------------------------------------

#[test]
fn module_function_hidden_until_use() {
    let mut ctx = Context::new(EvalConfig::default());
    // sqrt lives in the `math` module — not callable up front.
    let err = eval_source("(sqrt 9)", &mut ctx).unwrap_err();
    assert!(matches!(err, EvalError::UndefinedFunction(_)));

    // After `(use math)` it resolves.
    let v = eval_source("(use math) (sqrt 16)", &mut ctx).unwrap();
    assert_eq!(v, Value::Number(4.into()));
}

#[test]
fn use_accepts_quoted_keyword_string_forms() {
    for src in [
        "(use math) (pow 2 8)",
        "(use 'math) (pow 2 8)",
        "(use :math) (pow 2 8)",
        r#"(use "math") (pow 2 8)"#,
    ] {
        let mut ctx = Context::new(EvalConfig::default());
        let v = eval_source(src, &mut ctx).expect(src);
        assert_eq!(v, Value::Number(256.into()), "form: {}", src);
    }
}

#[test]
fn use_unknown_module_errors() {
    let mut ctx = Context::new(EvalConfig::default());
    let err = eval_source("(use definitely-not-real)", &mut ctx).unwrap_err();
    assert!(matches!(err, EvalError::InvalidForm(_)));
}

#[test]
fn math_module_basic() {
    let mut ctx = Context::new(EvalConfig::default());
    let v = eval_source(
        "(use math) [(sqrt 25) (pow 2 10) (gcd 12 18) (lcm 4 6)]",
        &mut ctx,
    )
    .unwrap();
    assert_eq!(
        v,
        Value::Array(vec![
            Value::Number(5.into()),
            Value::Number(1024.into()),
            Value::Number(6.into()),
            Value::Number(12.into()),
        ])
    );
}

#[test]
fn strings_module_basic() {
    let mut ctx = Context::new(EvalConfig::default());
    let v = eval_source(
        r#"(use strings)
           {:trunc (truncate "hello world" 5)
            :pad   (pad-left "7" 3 "0")
            :rep   (repeat "ab" 3)
            :idx   (index-of "hello" "llo")
            :rev   (str-reverse "abc")}"#,
        &mut ctx,
    )
    .unwrap();
    assert_eq!(v["trunc"], Value::String("hello".into()));
    assert_eq!(v["pad"], Value::String("007".into()));
    assert_eq!(v["rep"], Value::String("ababab".into()));
    assert_eq!(v["idx"], Value::Number(2.into()));
    assert_eq!(v["rev"], Value::String("cba".into()));
}

#[test]
fn serde_module_emits_all_three_formats() {
    let mut ctx = Context::new(EvalConfig::default());
    let v = eval_source(
        r#"(use serde)
           (let [doc {:name "x" :ver 2}]
             {:json   (to-json doc)
              :pretty (to-json {:a 1} true)
              :yaml   (to-yaml doc)
              :toml   (to-toml doc)})"#,
        &mut ctx,
    )
    .unwrap();
    assert_eq!(v["json"], Value::String(r#"{"name":"x","ver":2}"#.into()));
    assert_eq!(v["pretty"], Value::String("{\n  \"a\": 1\n}".into()));
    assert_eq!(v["yaml"], Value::String("name: x\nver: 2\n".into()));
    assert_eq!(v["toml"], Value::String("name = \"x\"\nver = 2\n".into()));
}

#[test]
fn serde_module_parses_all_three_formats() {
    let mut ctx = Context::new(EvalConfig::default());
    let v = eval_source(
        r#"(use serde)
           {:json (parse-json "{\"a\":[1,2]}")
            :yaml (parse-yaml "a:\n  - 1\n  - 2\n")
            :toml (parse-toml "a = [1, 2]\n")}"#,
        &mut ctx,
    )
    .unwrap();
    let expected = serde_json::json!({"a": [1, 2]});
    assert_eq!(v["json"], expected);
    assert_eq!(v["yaml"], expected);
    assert_eq!(v["toml"], expected);
}

#[test]
fn serde_round_trips_through_each_format() {
    let mut ctx = Context::new(EvalConfig::default());
    let v = eval_source(
        r#"(use serde)
           (let [doc {:a 1 :b ["x" true] :c {:d 1.5}}]
             {:json (parse-json (to-json doc))
              :yaml (parse-yaml (to-yaml doc))
              :toml (parse-toml (to-toml doc))})"#,
        &mut ctx,
    )
    .unwrap();
    let expected = serde_json::json!({"a": 1, "b": ["x", true], "c": {"d": 1.5}});
    assert_eq!(v["json"], expected);
    assert_eq!(v["yaml"], expected);
    assert_eq!(v["toml"], expected);
}

#[test]
fn serde_functions_hidden_until_use() {
    let mut ctx = Context::new(EvalConfig::default());
    let err = eval_source("(to-json {:a 1})", &mut ctx).unwrap_err();
    assert!(
        err.to_string().contains("to-json"),
        "expected an undefined-function error, got: {err}"
    );
}

#[test]
fn serde_to_toml_rejects_unrepresentable_documents() {
    let mut ctx = Context::new(EvalConfig::default());

    // TOML documents must be tables.
    let err = eval_source("(use serde) (to-toml [1 2])", &mut ctx).unwrap_err();
    assert!(err.to_string().contains("map"), "got: {err}");

    // TOML has no null, and the error names the offending path.
    let err = eval_source("(use serde) (to-toml {:pkg {:name nil}})", &mut ctx).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("no null"), "got: {msg}");
    assert!(msg.contains("pkg.name"), "got: {msg}");
}

#[test]
fn collections_module_basic() {
    let mut ctx = Context::new(EvalConfig::default());
    let v = eval_source(
        "(use collections)
         {:sum  (sum [1 2 3 4])
          :avg  (avg [2 4 6])
          :sort (sort [3 1 2])
          :uniq (uniq [1 1 2 2 3])
          :part (partition [1 2 3 4 5] 2)
          :take (take 2 [10 20 30 40])}",
        &mut ctx,
    )
    .unwrap();
    assert_eq!(v["sum"], Value::Number(10.into()));
    assert_eq!(v["avg"], Value::Number(4.into()));
    assert_eq!(v["sort"], serde_json::json!([1, 2, 3]));
    assert_eq!(v["uniq"], serde_json::json!([1, 2, 3]));
    assert_eq!(v["part"], serde_json::json!([[1, 2], [3, 4], [5]]));
    assert_eq!(v["take"], serde_json::json!([10, 20]));
}

#[test]
fn collections_pluck_and_group_by() {
    let mut ctx = Context::new(EvalConfig::default());
    let v = eval_source(
        r#"(use collections)
           (let [xs [{:t "a" :n 1} {:t "b" :n 2} {:t "a" :n 3}]]
             {:names (pluck "t" xs)
              :grp   (group-by "t" xs)})"#,
        &mut ctx,
    )
    .unwrap();
    assert_eq!(v["names"], serde_json::json!(["a", "b", "a"]));
    assert_eq!(v["grp"]["a"].as_array().unwrap().len(), 2);
    assert_eq!(v["grp"]["b"].as_array().unwrap().len(), 1);
}

#[test]
fn finance_module_pmt_matches_excel() {
    // 30-year monthly mortgage, $200k @ 6% APR -> ~ -$1199.10/mo.
    let mut ctx = Context::new(EvalConfig::default());
    let v = eval_source("(use finance) (pmt (/ 0.06 12) 360 200000)", &mut ctx).unwrap();
    let p = v.as_f64().expect("number");
    assert!(
        (p - (-1199.10)).abs() < 0.05,
        "expected ~ -1199.10, got {}",
        p
    );
}

#[test]
fn finance_npv_basic() {
    let mut ctx = Context::new(EvalConfig::default());
    // NPV with rate=0 is just the sum of cashflows.
    let v = eval_source("(use finance) (npv 0 100 200 300)", &mut ctx).unwrap();
    assert_eq!(v.as_f64().unwrap(), 600.0);
}

#[test]
fn predicates_module_macros() {
    let mut ctx = Context::new(EvalConfig::default());
    let v = eval_source(
        "(use predicates)
         [(inc 5) (dec 5) (even? 4) (odd? 3) (zero? 0)
          (between? 5 1 10) (coalesce nil 7) (default 3 99)]",
        &mut ctx,
    )
    .unwrap();
    assert_eq!(
        v,
        Value::Array(vec![
            Value::Number(6.into()),
            Value::Number(4.into()),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Bool(true),
            Value::Number(7.into()),
            Value::Number(3.into()),
        ])
    );
}

#[test]
fn module_does_not_leak_across_contexts() {
    let mut a = Context::new(EvalConfig::default());
    let _ = eval_source("(use math)", &mut a).unwrap();

    let mut b = Context::new(EvalConfig::default());
    // sqrt should still be hidden in the fresh context.
    let err = eval_source("(sqrt 9)", &mut b).unwrap_err();
    assert!(matches!(err, EvalError::UndefinedFunction(_)));
}

#[test]
fn glob_returns_matching_paths() {
    use std::fs;
    let tmp = std::env::temp_dir().join(format!(
        "trongo-glob-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(tmp.join("sub")).unwrap();
    fs::write(tmp.join("a.yaml"), "").unwrap();
    fs::write(tmp.join("b.yaml"), "").unwrap();
    fs::write(tmp.join("sub/c.yaml"), "").unwrap();
    fs::write(tmp.join("ignore.txt"), "").unwrap();

    let sb = Sandbox::new(&tmp).unwrap();
    let mut ctx = Context::new(EvalConfig::default().with_sandbox(sb));

    let v = eval_source(r#"(glob "**/*.yaml")"#, &mut ctx).unwrap();
    let arr = v.as_array().unwrap();
    let mut paths: Vec<String> = arr
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    paths.sort();
    assert_eq!(paths, vec!["a.yaml", "b.yaml", "sub/c.yaml"]);

    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn glob_requires_sandbox() {
    let mut ctx = Context::new(EvalConfig::default());
    let err = eval_source(r#"(glob "*.txt")"#, &mut ctx).unwrap_err();
    assert!(matches!(err, EvalError::NoSandbox));
}

// ---------------------------------------------------------------------------
// `rng` module
// ---------------------------------------------------------------------------

#[test]
fn rng_requires_use() {
    let mut ctx = Context::new(EvalConfig::default());
    let err = eval_source("(rand)", &mut ctx).unwrap_err();
    assert!(matches!(err, EvalError::UndefinedFunction(_)));
}

#[test]
fn rng_is_deterministic_with_fixed_seed() {
    let cfg_a = EvalConfig::new().with_rng_seed(42);
    let cfg_b = EvalConfig::new().with_rng_seed(42);
    let mut a = Context::new(cfg_a);
    let mut b = Context::new(cfg_b);
    let va = eval_source(
        "(use rng) [(rand) (rand-int 0 100) (dice-roll 3 6)]",
        &mut a,
    )
    .unwrap();
    let vb = eval_source(
        "(use rng) [(rand) (rand-int 0 100) (dice-roll 3 6)]",
        &mut b,
    )
    .unwrap();
    assert_eq!(va, vb);
}

#[test]
fn rng_default_seed_is_zero() {
    // EvalConfig::default() with no seed → seed 0 → predictable stream.
    let mut ctx = Context::new(EvalConfig::default());
    let v = eval_source("(use rng) (rand-int 0 1000000)", &mut ctx).unwrap();
    // Just verify the call succeeds and returns an int in range; the
    // specific value is whatever splitmix64(0) yields and is allowed to
    // change if we ever swap algorithms.
    let n = v.as_i64().expect("int");
    assert!((0..1_000_000).contains(&n));
}

#[test]
fn rng_rand_in_unit_interval() {
    let mut ctx = Context::new(EvalConfig::default().with_rng_seed(7));
    for _ in 0..32 {
        let v = eval_source("(use rng) (rand)", &mut ctx).unwrap();
        let f = v.as_f64().expect("float");
        assert!((0.0..1.0).contains(&f), "rand out of [0,1): {}", f);
    }
}

#[test]
fn rng_rand_int_respects_bounds() {
    let mut ctx = Context::new(EvalConfig::default().with_rng_seed(11));
    eval_source("(use rng)", &mut ctx).unwrap();
    for _ in 0..64 {
        let v = eval_source("(rand-int 10 20)", &mut ctx).unwrap();
        let n = v.as_i64().expect("int");
        assert!((10..20).contains(&n), "out of [10,20): {}", n);
    }
}

#[test]
fn rng_rand_int_rejects_empty_range() {
    let mut ctx = Context::new(EvalConfig::default().with_rng_seed(0));
    let err = eval_source("(use rng) (rand-int 5 5)", &mut ctx).unwrap_err();
    assert!(matches!(err, EvalError::Builtin(_)));
}

#[test]
fn rng_choose_picks_from_list() {
    let mut ctx = Context::new(EvalConfig::default().with_rng_seed(3));
    let v = eval_source(r#"(use rng) (choose ["red" "green" "blue"])"#, &mut ctx).unwrap();
    let s = v.as_str().unwrap();
    assert!(["red", "green", "blue"].contains(&s));
}

#[test]
fn rng_choose_rejects_empty_list() {
    let mut ctx = Context::new(EvalConfig::default().with_rng_seed(0));
    let err = eval_source("(use rng) (choose [])", &mut ctx).unwrap_err();
    assert!(matches!(err, EvalError::Builtin(_)));
}

#[test]
fn rng_dice_roll_in_range() {
    let mut ctx = Context::new(EvalConfig::default().with_rng_seed(99));
    eval_source("(use rng)", &mut ctx).unwrap();
    for _ in 0..32 {
        let v = eval_source("(dice-roll 4 6)", &mut ctx).unwrap();
        let n = v.as_i64().expect("int");
        // 4d6 → [4, 24]
        assert!((4..=24).contains(&n), "4d6 out of range: {}", n);
    }
}

#[test]
fn rng_dice_roll_zero_dice_is_zero() {
    let mut ctx = Context::new(EvalConfig::default().with_rng_seed(0));
    let v = eval_source("(use rng) (dice-roll 0 20)", &mut ctx).unwrap();
    assert_eq!(v, Value::Number(0.into()));
}

#[test]
fn rng_shuffle_preserves_elements() {
    let mut ctx = Context::new(EvalConfig::default().with_rng_seed(5));
    let v = eval_source("(use rng) (shuffle [1 2 3 4 5 6 7 8 9 10])", &mut ctx).unwrap();
    let arr = v.as_array().unwrap();
    let mut sorted: Vec<i64> = arr.iter().map(|x| x.as_i64().unwrap()).collect();
    sorted.sort();
    assert_eq!(sorted, (1..=10).collect::<Vec<_>>());
}

#[test]
fn rng_set_seed_resets_state() {
    let mut ctx = Context::new(EvalConfig::default().with_rng_seed(100));
    eval_source("(use rng)", &mut ctx).unwrap();
    let _ = eval_source("(rand) (rand) (rand)", &mut ctx).unwrap();
    let a = eval_source("(set-seed 7) (rand-int 0 10000)", &mut ctx).unwrap();
    let b = eval_source("(set-seed 7) (rand-int 0 10000)", &mut ctx).unwrap();
    assert_eq!(a, b, "set-seed should make subsequent calls reproducible");
}

#[test]
fn rng_calls_share_state() {
    // (rand-int) and (choose) consuming from the same stream means
    // their sequence is the same across runs with the same seed.
    let run = |seed: u64| -> Value {
        let mut ctx = Context::new(EvalConfig::default().with_rng_seed(seed));
        eval_source(
            r#"(use rng)
               [(rand-int 0 1000)
                (choose ["a" "b" "c" "d"])
                (rand-int 0 1000)]"#,
            &mut ctx,
        )
        .unwrap()
    };
    assert_eq!(run(123), run(123));
    assert_ne!(run(123), run(124));
}

#[test]
fn spintax_one_arg_uses_context_seed() {
    let mut a = Context::new(EvalConfig::default().with_rng_seed(7));
    let mut b = Context::new(EvalConfig::default().with_rng_seed(7));
    let va = eval_source(r#"(use strings rng) (spintax "{a|b|c|d|e}")"#, &mut a).unwrap();
    let vb = eval_source(r#"(use strings rng) (spintax "{a|b|c|d|e}")"#, &mut b).unwrap();
    assert_eq!(va, vb);
}

#[test]
fn spintax_two_arg_still_works() {
    // 2-arg form keeps its old behavior: explicit seed, no context dep.
    let mut ctx = Context::new(EvalConfig::default());
    let v = eval_source(r#"(use strings) (spintax "{a|b|c}" 1)"#, &mut ctx).unwrap();
    assert!(["a", "b", "c"].contains(&v.as_str().unwrap()));
}

#[test]
fn modules_have_no_io_capability() {
    // None of the new modules should expose anything that touches the FS
    // or shells out, even without a sandbox configured.
    let mut ctx = Context::new(EvalConfig::default());
    let _ = eval_source(
        "(use math strings collections finance predicates)",
        &mut ctx,
    )
    .unwrap();
    // include / read-file / list-files still require a sandbox to do
    // anything — confirm they're untouched and still gated.
    let r = eval_source(r#"(read-file "x")"#, &mut ctx);
    assert!(matches!(r, Err(EvalError::NoSandbox)));
}

// ---------------------------------------------------------------------------
// First-class functions: fn, defn, map, filter, reduce
// ---------------------------------------------------------------------------

#[test]
fn defn_and_call_by_name() {
    let v = eval(r#"(defn double [x] (* x 2)) (double 21)"#);
    assert_eq!(v, Value::Number(42.into()));
}

#[test]
fn fn_immediately_invoked() {
    let v = eval(r#"((fn [x y] (+ x y)) 3 4)"#);
    assert_eq!(v, Value::Number(7.into()));
}

#[test]
fn map_over_vector() {
    let v = eval(r#"(map (fn [x] (* x x)) [1 2 3 4])"#);
    let arr = v.as_array().unwrap();
    assert_eq!(arr[0], Value::Number(1.into()));
    assert_eq!(arr[1], Value::Number(4.into()));
    assert_eq!(arr[2], Value::Number(9.into()));
    assert_eq!(arr[3], Value::Number(16.into()));
}

#[test]
fn filter_with_named_predicate() {
    let v = eval(
        r#"(defn even? [n] (= 0 (mod n 2)))
           (filter even? [1 2 3 4 5 6])"#,
    );
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0], Value::Number(2.into()));
    assert_eq!(arr[2], Value::Number(6.into()));
}

#[test]
fn reduce_with_seed_sums_list() {
    let v = eval(r#"(reduce (fn [acc x] (+ acc x)) 0 [1 2 3 4 5])"#);
    assert_eq!(v, Value::Number(15.into()));
}

#[test]
fn reduce_without_seed_uses_first() {
    let v = eval(r#"(reduce (fn [acc x] (+ acc x)) [10 20 30])"#);
    assert_eq!(v, Value::Number(60.into()));
}

#[test]
fn closure_captures_lexical_binding() {
    // `make-adder` returns a closure that should still see `n` after the
    // outer `let` frame is gone.
    let v = eval(
        r#"(defn make-adder [n] (fn [x] (+ x n)))
           (let [add5 (make-adder 5)]
             (add5 37))"#,
    );
    assert_eq!(v, Value::Number(42.into()));
}

#[test]
fn closures_see_later_def_globals() {
    // Closures should resolve globals live (not from a snapshot), so a
    // `def` made after the closure is created is still visible.
    let v = eval(
        r#"(defn use-g [] g)
           (def g 99)
           (use-g)"#,
    );
    assert_eq!(v, Value::Number(99.into()));
}

#[test]
fn fn_arity_error() {
    let mut ctx = Context::new(EvalConfig::default());
    let r = eval_source(r#"((fn [x y] (+ x y)) 1)"#, &mut ctx);
    assert!(matches!(r, Err(EvalError::Arity { .. })), "got {:?}", r);
}

#[test]
fn fn_rest_args_collect_into_list() {
    let v = eval(r#"((fn [head & tail] tail) 1 2 3 4)"#);
    let arr = v.as_array().unwrap();
    assert_eq!(arr.len(), 3);
    assert_eq!(arr[0], Value::Number(2.into()));
    assert_eq!(arr[2], Value::Number(4.into()));
}

#[test]
fn map_filter_compose() {
    let v = eval(
        r#"(map (fn [x] (* x 10))
                (filter (fn [x] (> x 2)) [1 2 3 4 5]))"#,
    );
    let arr = v.as_array().unwrap();
    assert_eq!(
        arr,
        &vec![
            Value::Number(30.into()),
            Value::Number(40.into()),
            Value::Number(50.into()),
        ]
    );
}

// ---------------------------------------------------------------------------
// CL-style higher-order builtins
// ---------------------------------------------------------------------------

fn ints(xs: &[i64]) -> Value {
    Value::Array(
        xs.iter()
            .copied()
            .map(|n| Value::Number(n.into()))
            .collect(),
    )
}

#[test]
fn mapcar_is_alias_of_map() {
    let v = eval(r#"(mapcar (fn [x] (+ x 1)) [10 20 30])"#);
    assert_eq!(v, ints(&[11, 21, 31]));
}

#[test]
fn mapc_returns_input_list() {
    let v = eval(r#"(mapc (fn [x] (* x 2)) [1 2 3])"#);
    assert_eq!(v, ints(&[1, 2, 3]));
}

#[test]
fn maplist_passes_successive_suffixes() {
    let v = eval(r#"(maplist (fn [tail] (count tail)) [10 20 30 40])"#);
    assert_eq!(v, ints(&[4, 3, 2, 1]));
}

#[test]
fn mapl_returns_input_list() {
    let v = eval(r#"(mapl (fn [tail] (count tail)) [10 20 30])"#);
    assert_eq!(v, ints(&[10, 20, 30]));
}

#[test]
fn mapcan_concatenates_results() {
    let v = eval(r#"(mapcan (fn [x] [x x]) [1 2 3])"#);
    assert_eq!(v, ints(&[1, 1, 2, 2, 3, 3]));
}

#[test]
fn mapcon_concatenates_suffix_results() {
    let v = eval(r#"(mapcon (fn [tail] [(first tail)]) [10 20 30])"#);
    assert_eq!(v, ints(&[10, 20, 30]));
}

#[test]
fn every_true_when_all_match() {
    let v = eval(r#"(every (fn [x] (> x 0)) [1 2 3])"#);
    assert_eq!(v, Value::Bool(true));
    let v = eval(r#"(every (fn [x] (> x 0)) [1 -1 3])"#);
    assert_eq!(v, Value::Bool(false));
}

#[test]
fn some_returns_first_truthy() {
    let v = eval(r#"(some (fn [x] (if (> x 2) x nil)) [1 2 3 4])"#);
    assert_eq!(v, Value::Number(3.into()));
    let v = eval(r#"(some (fn [x] (> x 100)) [1 2 3])"#);
    assert_eq!(v, Value::Null);
}

#[test]
fn notany_and_notevery() {
    assert_eq!(
        eval(r#"(notany (fn [x] (> x 100)) [1 2 3])"#),
        Value::Bool(true)
    );
    assert_eq!(
        eval(r#"(notany (fn [x] (> x 2))   [1 2 3])"#),
        Value::Bool(false)
    );
    assert_eq!(
        eval(r#"(notevery (fn [x] (> x 0)) [1 -1])"#),
        Value::Bool(true)
    );
    assert_eq!(
        eval(r#"(notevery (fn [x] (> x 0)) [1 2])"#),
        Value::Bool(false)
    );
}

#[test]
fn count_if_and_not() {
    assert_eq!(
        eval(r#"(count-if     (fn [x] (> x 2)) [1 2 3 4 5])"#),
        Value::Number(3.into())
    );
    assert_eq!(
        eval(r#"(count-if-not (fn [x] (> x 2)) [1 2 3 4 5])"#),
        Value::Number(2.into())
    );
}

#[test]
fn find_if_and_not() {
    assert_eq!(
        eval(r#"(find-if     (fn [x] (> x 2)) [1 2 3 4])"#),
        Value::Number(3.into())
    );
    assert_eq!(
        eval(r#"(find-if-not (fn [x] (> x 2)) [1 2 3 4])"#),
        Value::Number(1.into())
    );
    assert_eq!(eval(r#"(find-if (fn [x] (> x 100)) [1 2 3])"#), Value::Null);
}

#[test]
fn position_if_and_not() {
    assert_eq!(
        eval(r#"(position-if     (fn [x] (> x 2)) [1 2 3 4])"#),
        Value::Number(2.into())
    );
    assert_eq!(
        eval(r#"(position-if-not (fn [x] (> x 2)) [3 4 1 2])"#),
        Value::Number(2.into())
    );
    assert_eq!(
        eval(r#"(position-if (fn [x] (> x 100)) [1 2 3])"#),
        Value::Null
    );
}

#[test]
fn member_if_returns_tail() {
    let v = eval(r#"(member-if (fn [x] (> x 2)) [1 2 3 4 5])"#);
    assert_eq!(v, ints(&[3, 4, 5]));
    let v = eval(r#"(member-if (fn [x] (> x 100)) [1 2 3])"#);
    assert_eq!(v, Value::Null);
}

#[test]
fn remove_if_and_not() {
    assert_eq!(
        eval(r#"(remove-if     (fn [x] (= 0 (mod x 2))) [1 2 3 4 5])"#),
        ints(&[1, 3, 5])
    );
    assert_eq!(
        eval(r#"(remove-if-not (fn [x] (= 0 (mod x 2))) [1 2 3 4 5])"#),
        ints(&[2, 4])
    );
}

#[test]
fn assoc_if_finds_pair_by_key() {
    let v = eval(r#"(assoc-if (fn [k] (= k "b")) [["a" 1] ["b" 2] ["c" 3]])"#);
    assert_eq!(
        v,
        Value::Array(vec![Value::String("b".into()), Value::Number(2.into()),])
    );
    assert_eq!(
        eval(r#"(assoc-if (fn [k] (= k "z")) [["a" 1]])"#),
        Value::Null
    );
}

#[test]
fn assoc_if_not_skips_matches() {
    let v = eval(r#"(assoc-if-not (fn [k] (= k "a")) [["a" 1] ["b" 2]])"#);
    assert_eq!(
        v,
        Value::Array(vec![Value::String("b".into()), Value::Number(2.into()),])
    );
}

#[test]
fn rassoc_if_finds_pair_by_value() {
    let v = eval(r#"(rassoc-if (fn [v] (= v 2)) [["a" 1] ["b" 2] ["c" 3]])"#);
    assert_eq!(
        v,
        Value::Array(vec![Value::String("b".into()), Value::Number(2.into()),])
    );
}

#[test]
fn rassoc_if_not_skips_matching_values() {
    let v = eval(r#"(rassoc-if-not (fn [v] (= v 1)) [["a" 1] ["b" 2]])"#);
    assert_eq!(
        v,
        Value::Array(vec![Value::String("b".into()), Value::Number(2.into()),])
    );
}

#[test]
fn subst_if_replaces_leaves_in_tree() {
    let v = eval(r#"(subst-if "X" (fn [n] (= n 2)) [1 [2 3] {:k 2 :j [2]}])"#);
    // 2 was replaced everywhere it appeared as a leaf.
    let outer = v.as_array().unwrap();
    assert_eq!(outer[0], Value::Number(1.into()));
    let inner = outer[1].as_array().unwrap();
    assert_eq!(inner[0], Value::String("X".into()));
    assert_eq!(inner[1], Value::Number(3.into()));
    let m = outer[2].as_object().unwrap();
    assert_eq!(m["k"], Value::String("X".into()));
    assert_eq!(m["j"].as_array().unwrap()[0], Value::String("X".into()));
}

#[test]
fn subst_if_not_replaces_non_matches() {
    // CL semantics: every node (including the root container) is tested.
    // The root array is not equal to 1, so the entire tree is replaced
    // wholesale.
    let v = eval(r#"(subst-if-not 0 (fn [n] (= n 1)) [1 2 1 3])"#);
    assert_eq!(v, Value::Number(0.into()));

    // Atom-level behavior: matching root is preserved, non-matching root
    // is replaced.
    assert_eq!(
        eval(r#"(subst-if-not "x" (fn [n] (= n 1)) 1)"#),
        Value::Number(1.into())
    );
    assert_eq!(
        eval(r#"(subst-if-not "x" (fn [n] (= n 1)) 2)"#),
        Value::String("x".into())
    );
}

#[test]
fn substitute_if_top_level_only() {
    let v = eval(r#"(substitute-if 0 (fn [n] (> n 2)) [1 2 3 4 5])"#);
    assert_eq!(v, ints(&[1, 2, 0, 0, 0]));
}

#[test]
fn substitute_if_not_top_level_only() {
    let v = eval(r#"(substitute-if-not 0 (fn [n] (> n 2)) [1 2 3 4 5])"#);
    assert_eq!(v, ints(&[0, 0, 3, 4, 5]));
}

#[test]
fn sort_ascending_via_lt_predicate() {
    let v = eval(r#"(sort (fn [a b] (< a b)) [3 1 4 1 5 9 2 6])"#);
    assert_eq!(v, ints(&[1, 1, 2, 3, 4, 5, 6, 9]));
}

#[test]
fn stable_sort_is_alias_of_sort() {
    let v = eval(r#"(stable-sort (fn [a b] (< a b)) [3 1 2])"#);
    assert_eq!(v, ints(&[1, 2, 3]));
}

#[test]
fn funcall_invokes_function() {
    let v = eval(r#"(funcall (fn [x y] (* x y)) 6 7)"#);
    assert_eq!(v, Value::Number(42.into()));
}

#[test]
fn apply_splices_last_arg() {
    assert_eq!(
        eval(r#"(apply (fn [a b c] (+ a b c)) [1 2 3])"#),
        Value::Number(6.into())
    );
    assert_eq!(
        eval(r#"(apply (fn [a b c] (+ a b c)) 1 [2 3])"#),
        Value::Number(6.into())
    );
    assert_eq!(
        eval(r#"(apply (fn [a b c] (+ a b c)) 1 2 [3])"#),
        Value::Number(6.into())
    );
}

#[test]
fn identity_returns_arg() {
    assert_eq!(eval(r#"(identity 42)"#), Value::Number(42.into()));
    assert_eq!(eval(r#"(identity "hi")"#), Value::String("hi".into()));
}

#[test]
fn constantly_ignores_args() {
    let v = eval(
        r#"(let [k (constantly 99)]
             [(k) (k 1) (k 1 2 3)])"#,
    );
    let arr = v.as_array().unwrap();
    for x in arr {
        assert_eq!(x, &Value::Number(99.into()));
    }
}

#[test]
fn complement_inverts_predicate() {
    let v = eval(r#"(filter (complement (fn [x] (> x 2))) [1 2 3 4 5])"#);
    assert_eq!(v, ints(&[1, 2]));
}

#[test]
fn remove_duplicates_keeps_first_occurrence() {
    let v = eval(r#"(remove-duplicates [1 2 1 3 2 4])"#);
    assert_eq!(v, ints(&[1, 2, 3, 4]));
}

#[test]
fn tree_equal_compares_structurally() {
    assert_eq!(
        eval(r#"(tree-equal [1 [2 3]] [1 [2 3]])"#),
        Value::Bool(true)
    );
    assert_eq!(
        eval(r#"(tree-equal [1 [2 3]] [1 [2 4]])"#),
        Value::Bool(false)
    );
}

#[test]
fn set_operations() {
    assert_eq!(eval(r#"(union [1 2 3] [3 4 5])"#), ints(&[1, 2, 3, 4, 5]));
    assert_eq!(eval(r#"(intersection [1 2 3 4] [2 4 6])"#), ints(&[2, 4]));
    assert_eq!(eval(r#"(set-difference [1 2 3 4] [2 4])"#), ints(&[1, 3]));
    assert_eq!(eval(r#"(set-exclusive-or [1 2 3] [2 3 4])"#), ints(&[1, 4]));
}

// ---------------------------------------------------------------------------
// Store-backed evaluation: the same `include` machinery with no disk at all.
// ---------------------------------------------------------------------------

/// Build a context whose sandbox reads out of a `MemoryFs`, with
/// `current_file_dir` set to the root the way a host loading an entry file
/// would set it.
fn memory_ctx(files: &[(&str, &str)]) -> Context {
    use std::sync::Arc;
    use trongoscript::vfs::{MemoryFs, MEMORY_FS_ROOT};

    let mut fs = MemoryFs::new();
    for (path, contents) in files {
        fs.insert(format!("{MEMORY_FS_ROOT}/{path}"), *contents);
    }
    let sb = Sandbox::with_store(MEMORY_FS_ROOT, Arc::new(fs)).expect("store sandbox");
    let mut ctx = Context::new(EvalConfig::default().with_sandbox(sb));
    ctx.current_file_dir = Some(PathBuf::from(MEMORY_FS_ROOT));
    ctx
}

#[test]
fn include_chain_runs_entirely_in_memory() {
    let mut ctx = memory_ctx(&[
        ("palette.trongo", r#"(def accent "teal")"#),
        (
            "sub/depth.trongo",
            r#"(do (include "../palette.trongo") (def depth 3))"#,
        ),
    ]);

    let v = eval_source(
        r#"(do (include "sub/depth.trongo") {:accent accent :depth depth})"#,
        &mut ctx,
    )
    .expect("eval");
    assert_eq!(v["accent"], Value::String("teal".into()));
    assert_eq!(v["depth"], Value::Number(3.into()));
}

#[test]
fn include_cycle_is_caught_across_two_spellings_of_one_path() {
    // The cycle detector keys on the resolved path. Under a store those are
    // lexically normalized rather than canonicalized, so `render/../a` and
    // `a` have to collapse to the same key or this cycle slips through and
    // recurses until the depth limit — a much worse error message.
    let mut ctx = memory_ctx(&[
        ("a.trongo", r#"(include "sub/../a.trongo")"#),
        ("sub/keep.trongo", ""),
    ]);

    let err = eval_source(r#"(include "a.trongo")"#, &mut ctx).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("cycle"), "expected a cycle error, got {text}");
}

#[test]
fn store_backed_include_cannot_escape_the_root() {
    let mut ctx = memory_ctx(&[("main.trongo", "1")]);
    let r = eval_source(r#"(include "../outside.trongo")"#, &mut ctx);
    assert!(r.is_err(), "expected sandbox rejection, got {r:?}");
}

#[test]
fn store_backed_read_file_and_list_files() {
    let mut ctx = memory_ctx(&[("notes.txt", "hello"), ("sub/a.txt", "")]);

    assert_eq!(
        eval_source(r#"(read-file "notes.txt")"#, &mut ctx).unwrap(),
        Value::String("hello".into())
    );

    let listed = eval_source(r#"(list-files ".")"#, &mut ctx).unwrap();
    let names: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["notes.txt", "sub"]);
}

// ---------------------------------------------------------------------------
// Emitting Trongoscript source (to_trongo_source) — the inverse of reading.
// ---------------------------------------------------------------------------

/// Emit `v` as Trongoscript, read it back, and return the result. Anything
/// the emitter writes must be something the reader accepts, so this closes
/// the loop that the emitter's own unit tests cannot.
fn round_trip(v: &Value) -> Value {
    let src = trongoscript::to_trongo_source(v);
    eval(&src)
}

fn round_trip_pretty(v: &Value) -> Value {
    let src = trongoscript::to_trongo_source_pretty(v);
    eval(&src)
}

#[test]
fn emitted_source_reparses_to_the_same_value() {
    let cases = vec![
        serde_json::json!(null),
        serde_json::json!(true),
        serde_json::json!(42),
        serde_json::json!(-7),
        serde_json::json!(3.25),
        serde_json::json!("plain"),
        serde_json::json!([]),
        serde_json::json!({}),
        serde_json::json!([1, 2, 3]),
        serde_json::json!({"a": 1, "b": [1, {"c": null}]}),
    ];
    for v in cases {
        assert_eq!(round_trip(&v), v, "compact round trip failed for {v}");
        assert_eq!(round_trip_pretty(&v), v, "pretty round trip failed for {v}");
    }
}

#[test]
fn emitted_strings_survive_escaping() {
    let v = serde_json::json!({
        "newline": "a\nb",
        "tab": "a\tb",
        "quote": "say \"hi\"",
        "backslash": "a\\b",
        "control": "a\u{1}b",
        "unicode": "héllo 世界",
        "empty": "",
    });
    assert_eq!(round_trip(&v), v);
}

#[test]
fn emitted_keys_survive_quoting() {
    // Keys that cannot be keywords must come back as the same strings.
    let v = serde_json::json!({
        "full name": 1,
        "has:colon": 2,
        "has{brace": 3,
        "has;semi": 4,
        "": 5,
        "ok-key": 6,
        "42": 7,
    });
    assert_eq!(round_trip(&v), v);
}

#[test]
fn emitted_extreme_floats_survive() {
    let v = serde_json::json!({
        "big": 1e300,
        "small": 1e-300,
        "neg": -1e300,
        "max": f64::MAX,
    });
    assert_eq!(round_trip(&v), v);
}

#[test]
fn to_trongo_builtin_output_reparses() {
    // The in-language path: parse JSON, emit Trongoscript, read it back.
    let src = r#"(use serde) (to-trongo (parse-json "{\"a\":[1,2],\"b c\":null}"))"#;
    let emitted = eval(src);
    let text = emitted.as_str().expect("to-trongo returns a string");
    assert_eq!(eval(text), serde_json::json!({"a": [1, 2], "b c": null}));
}

// ---------------------------------------------------------------------------
// The emitter/reader seam, for values that look like sugar
//
// `to_trongo_source` and the reader live in different crates and are unit
// tested separately: the emitter asserts what it writes, the reader asserts
// what it accepts. Neither can catch a disagreement between them, and string
// interpolation created a way for them to disagree — `#{` and `${` acquired
// reader significance that the emitter had no reason to escape before.
//
// A value round-tripping through *both* is the only assertion that closes
// that gap, so the sigil cases are pinned here rather than on either side.
// ---------------------------------------------------------------------------

/// A string value whose content looks like an interpolation must come back
/// as the same string. If the emitter under-escapes, `#{x}` re-reads as a
/// `(str x)` call and a data value has silently become a computation.
#[test]
fn emitted_strings_that_look_like_sugar_survive() {
    let v = serde_json::json!({
        "interp": "#{x}",
        "group": "${a + b}",
        "bare-hash": "#",
        "bare-dollar": "$",
        "doubled": "##{x}",
        "brace-only": "{}",
        "open-interp": "#{",
        "open-group": "${",
        "mixed": "US$5 and #1",
        "escaped": "\\#{x}",
        "inner": "path/#{a}/#{b}",
    });
    assert_eq!(round_trip(&v), v);
    assert_eq!(round_trip_pretty(&v), v);
}

/// The same, in key position. A key that would read as an infix group
/// cannot be emitted bare and has to fall back to a quoted string.
#[test]
fn emitted_keys_that_look_like_sugar_survive() {
    let v = serde_json::json!({
        "${x}": 1,
        "#{x}": 2,
        "a$b": 3,
        "$": 4,
        "#": 5,
        "a${b": 6,
    });
    assert_eq!(round_trip(&v), v);
    assert_eq!(round_trip_pretty(&v), v);
}

/// The in-language path for the same property: a document containing sigils
/// makes the full trip through `to-trongo` and back.
///
/// Writing this test requires `\#` and `\$` in the *source*, because a bare
/// `#{x}` in a Trongoscript string literal interpolates at read time and
/// would never reach the value at all. That is the escape hatch working as
/// designed, and it is asserted first so the rest of the test is clearly
/// about the emitter and not about the reader.
#[test]
fn to_trongo_output_with_sigils_reparses() {
    // The escape hatch: `\#{x}` is the literal text, not an interpolation.
    assert_eq!(eval(r##""\#{x}""##), serde_json::json!("#{x}"));
    assert_eq!(eval(r##""\${k}""##), serde_json::json!("${k}"));

    let src = r##"(use serde) (to-trongo {:a "\#{x}" "\${k}" "US\$5"})"##;
    let emitted = eval(src);
    let text = emitted.as_str().expect("to-trongo returns a string");

    // The emitted source re-escapes both sigils, so reading it back gives
    // the strings rather than the calls they would otherwise become.
    assert_eq!(eval(text), serde_json::json!({"a": "#{x}", "${k}": "US$5"}));
}

// ---------------------------------------------------------------------------
// The new sugar, end to end through each entry point
//
// `eval_source` is covered by the unit tests. These pin that the other three
// front doors — templates, embedded documents, and the sandboxed include
// path — reach the same reader, since each parses source through its own
// wrapper and could plausibly have missed the new forms.
// ---------------------------------------------------------------------------

#[test]
fn templates_accept_the_new_sugar() {
    let mut ctx = Context::new(EvalConfig::default());
    let tmpl = concat!(
        "<% (def env \"prod\") (def ram 16) %>",
        "level: <%= (cond (= env \"prod\") \"warn\" :else \"debug\") %>\n",
        "heap: <%= ${ram * 3 / 4} %>\n",
        "url: <%= \"https://#{env}.example.com\" %>\n",
    );
    let out = render_template(tmpl, &mut ctx).expect("render failed");
    assert_eq!(
        out,
        "level: warn\nheap: 12\nurl: https://prod.example.com\n"
    );
}

#[test]
fn templates_accept_destructuring() {
    let mut ctx = Context::new(EvalConfig::default());
    let tmpl = concat!(
        "<% (def server {:name \"web\" :port 443}) %>",
        "<%= (let [{:name n :port p} server] \"#{n}:#{p}\") %>\n",
    );
    let out = render_template(tmpl, &mut ctx).expect("render failed");
    assert_eq!(out, "web:443\n");
}

#[test]
fn embedded_documents_accept_the_new_sugar() {
    let mut ctx = Context::new(EvalConfig::default());
    let doc: serde_yaml::Value = serde_yaml::from_str(concat!(
        "port: \"!trongo ${8000 + 80}\"\n",
        "level: \"!trongo (cond false \\\"a\\\" :else \\\"b\\\")\"\n",
        "name: \"!trongo (let [{:a x} {:a \\\"web\\\"}] x)\"\n",
    ))
    .unwrap();
    let v = evaluate_embedded_yaml(doc, &mut ctx).expect("embed failed");
    let obj = v.as_object().unwrap();
    assert_eq!(obj.get("port"), Some(&serde_json::json!(8080)));
    assert_eq!(obj.get("level"), Some(&serde_json::json!("b")));
    assert_eq!(obj.get("name"), Some(&serde_json::json!("web")));
}

/// An included file is read by the same reader, so sugar written in a
/// library file works the same as sugar written inline.
#[test]
fn included_files_accept_the_new_sugar() {
    let dir = std::env::temp_dir().join(format!(
        "trongo-sugar-include-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let lib = dir.join("lib.trongo");
    std::fs::write(
        &lib,
        r##"(defn label [{:name n :port p}] "#{n}:${p + 1}")"##,
    )
    .unwrap();

    let sb = Sandbox::new(&dir).unwrap();
    let mut ctx = Context::new(EvalConfig::default().with_sandbox(sb));
    let v = eval_source(
        r#"(include "/lib.trongo") (label {:name "web" :port 442})"#,
        &mut ctx,
    )
    .expect("include failed");
    assert_eq!(v, serde_json::json!("web:443"));

    std::fs::remove_dir_all(&dir).ok();
}

/// The shape all of this exists to serve: one document using every new form
/// together, evaluated the way a user would evaluate it.
#[test]
fn a_config_document_using_every_new_form() {
    let src = r##"
        (def env "production")
        (def servers [{:name "web" :ram 16} {:name "worker" :ram 8}])

        (defn plan [{:name n :ram r}]
          {:name  n
           :heap  "${r * 3 / 4}G"
           :tier  (cond ${r >= 16} "large" ${r >= 8} "medium" :else "small")})

        {:env      env
         :log      (if (= env "production") "warn"
                     :elsif (= env "staging") "info"
                     :else "debug")
         :endpoint "https://#{env}.example.com:${8000 + 443}"
         :plans    (map plan servers)}
    "##;
    let v = eval(src);
    let obj = v.as_object().unwrap();

    assert_eq!(obj.get("env"), Some(&serde_json::json!("production")));
    assert_eq!(obj.get("log"), Some(&serde_json::json!("warn")));
    assert_eq!(
        obj.get("endpoint"),
        Some(&serde_json::json!("https://production.example.com:8443"))
    );
    assert_eq!(
        obj.get("plans"),
        Some(&serde_json::json!([
            {"name": "web",    "heap": "12G", "tier": "large"},
            {"name": "worker", "heap": "6G",  "tier": "medium"},
        ]))
    );

    // And the whole result survives the emitter/reader round trip, so a
    // document built with sugar can be written out and read back.
    assert_eq!(round_trip(&v), v);
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
