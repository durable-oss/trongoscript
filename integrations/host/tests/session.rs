use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use pretty_assertions::assert_eq;
use serde_json::json;
use trongoscript_host::{
    fuel_limits, sandbox_limits, ErrorKind, Event, Events, Host, Markers, NoHost, Options,
    SandboxSpec, Session, Value,
};

fn session() -> Session {
    Session::new(Options::default()).unwrap()
}

fn eval(src: &str) -> Result<Value, trongoscript_host::HostError> {
    session().eval(src, &mut NoHost)
}

type HostFn = Box<dyn FnMut(Vec<Value>) -> Result<Value, String>>;

/// A host whose functions are Rust closures, recording every call.
#[derive(Default)]
struct Funcs {
    fns: HashMap<String, HostFn>,
    calls: Vec<(String, Vec<Value>)>,
}

impl Funcs {
    fn with(
        mut self,
        name: &str,
        f: impl FnMut(Vec<Value>) -> Result<Value, String> + 'static,
    ) -> Self {
        self.fns.insert(name.into(), Box::new(f));
        self
    }
}

impl Host for Funcs {
    fn call(&mut self, name: &str, args: Vec<Value>) -> Result<Value, String> {
        self.calls.push((name.into(), args.clone()));
        let f = self.fns.get_mut(name).ok_or("missing")?;
        f(args)
    }
}

fn scratch(name: &str) -> PathBuf {
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "{name}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

// ---------------------------------------------------------------- evaluation

#[test]
fn evaluates_a_document() {
    let v = eval(r#"{:a (+ 1 2) :b [true nil "s"] :c 1.5}"#).unwrap();
    assert_eq!(v, json!({"a": 3, "b": [true, null, "s"], "c": 1.5}));
}

#[test]
fn empty_source_is_nil() {
    assert_eq!(eval("").unwrap(), Value::Null);
    assert_eq!(eval("  ; only a comment\n").unwrap(), Value::Null);
}

#[test]
fn globals_and_definitions_persist_between_evaluations() {
    let mut s = session();
    s.eval("(def x 41) (defn inc1 [n] (+ n 1))", &mut NoHost)
        .unwrap();
    assert_eq!(s.eval("(inc1 x)", &mut NoHost).unwrap(), json!(42));
    assert_eq!(s.get("x").unwrap(), Some(json!(41)));
}

#[test]
fn macros_persist_between_evaluations() {
    let mut s = session();
    s.eval("(defmacro twice [x] `(* 2 ~x))", &mut NoHost)
        .unwrap();
    assert_eq!(s.eval("(twice 21)", &mut NoHost).unwrap(), json!(42));
}

#[test]
fn initial_globals_are_visible() {
    let opts = Options {
        globals: vec![("env".into(), json!("prod")), ("n".into(), json!(2))],
        ..Options::default()
    };
    let mut s = Session::new(opts).unwrap();
    assert_eq!(
        s.eval("(str env \"-\" n)", &mut NoHost).unwrap(),
        json!("prod-2")
    );
}

#[test]
fn set_get_unset_and_list_globals() {
    let mut s = session();
    s.set("b", json!([1])).unwrap();
    s.set("a", json!({"k": "v"})).unwrap();
    assert_eq!(
        s.globals().unwrap(),
        vec![("a".into(), json!({"k": "v"})), ("b".into(), json!([1]))]
    );
    assert!(s.contains("a"));
    assert!(s.unset("a"));
    assert!(!s.contains("a"));
    assert!(!s.unset("a"));
    assert_eq!(s.get("a").unwrap(), None);
}

#[test]
fn invalid_global_names_are_rejected() {
    let mut s = session();
    for bad in ["", "a b", "(x)", "x;y", "\"q\"", "[v]", "{m}", "a,b"] {
        let e = s.set(bad, json!(1)).unwrap_err();
        assert_eq!(e.kind, ErrorKind::Argument, "{bad:?}");
    }
    let opts = Options {
        globals: vec![("no good".into(), json!(1))],
        ..Options::default()
    };
    assert_eq!(Session::new(opts).err().unwrap().kind, ErrorKind::Argument);
}

#[test]
fn seed_makes_rng_reproducible() {
    let run = |seed| {
        let opts = Options {
            seed: Some(seed),
            ..Options::default()
        };
        Session::new(opts)
            .unwrap()
            .eval(
                "(use rng) [(rand-int 0 1000000) (rand-int 0 1000000)]",
                &mut NoHost,
            )
            .unwrap()
    };
    assert_eq!(run(7), run(7));
    assert_ne!(run(7), run(8));
}

#[test]
fn deep_recursion_runs_on_the_large_stack() {
    // A test thread has a 2 MiB stack; a debug build overflows that after a
    // few dozen nested calls. Reaching 1,500 proves evaluation happened
    // elsewhere.
    let v = eval("(defn down [n] (if (= n 0) 0 (+ 1 (down (- n 1))))) (down 1500)").unwrap();
    assert_eq!(v, json!(1500));
}

#[test]
fn usage_is_reported_and_reset_per_evaluation() {
    let mut s = session();
    s.eval("(reduce (fn [a x] (+ a x)) 0 (range 1000))", &mut NoHost)
        .unwrap();
    let big = s.usage();
    assert!(big.steps > 100, "{big:?}");
    s.eval("1", &mut NoHost).unwrap();
    let small = s.usage();
    assert!(small.steps < 10, "{small:?}");
    assert!(small.peak_call_depth < big.peak_call_depth, "{small:?}");
}

#[test]
fn each_evaluation_gets_a_fresh_budget() {
    let opts = Options {
        limits: fuel_limits(None, &[("max_steps".into(), 5_000)]).unwrap(),
        ..Options::default()
    };
    let mut s = Session::new(opts).unwrap();
    for _ in 0..10 {
        s.eval("(reduce (fn [a x] (+ a x)) 0 (range 100))", &mut NoHost)
            .unwrap();
    }
}

// ---------------------------------------------------------------- errors

#[test]
fn errors_are_classified() {
    let cases = [
        ("(+ 1", ErrorKind::Parse),
        ("undefined-thing", ErrorKind::Eval),
        ("(no-such-fn 1)", ErrorKind::Eval),
        ("(+ 1 \"a\")", ErrorKind::Eval),
        ("(read-file \"x\")", ErrorKind::Sandbox),
        ("(include \"x\")", ErrorKind::Sandbox),
    ];
    for (src, kind) in cases {
        let e = eval(src).unwrap_err();
        assert_eq!(e.kind, kind, "{src}: {}", e.message);
        assert!(!e.message.is_empty());
    }
}

#[test]
fn resource_limits_are_classified() {
    let strict = || Options {
        limits: fuel_limits(Some("strict"), &[]).unwrap(),
        ..Options::default()
    };
    let cases = [
        "(defn f [n] (f n)) (f 1)",
        "(defn f [n] (+ 1 (f n))) (f 1)",
        "(reduce (fn [a x] (+ a x)) 0 (range 10000000))",
    ];
    for src in cases {
        let e = Session::new(strict())
            .unwrap()
            .eval(src, &mut NoHost)
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::ResourceLimit, "{src}: {}", e.message);
    }
    // Too deeply nested to parse is a resource problem, not a syntax one.
    let deep = format!("{}1{}", "[".repeat(100_000), "]".repeat(100_000));
    assert_eq!(eval(&deep).unwrap_err().kind, ErrorKind::ResourceLimit);
}

#[test]
fn a_failed_evaluation_leaves_the_session_usable() {
    let mut s = session();
    s.eval("(def kept 1)", &mut NoHost).unwrap();
    assert!(s.eval("(+ 1", &mut NoHost).is_err());
    assert!(s.eval("(nope)", &mut NoHost).is_err());
    assert_eq!(s.eval("kept", &mut NoHost).unwrap(), json!(1));
}

// ---------------------------------------------------------------- nesting

/// Builds a value 2,000 levels deep while using almost no eval depth.
/// (Each step copies the accumulator, so this is quadratic; 2,000 keeps it
/// well under a second in a debug build.)
const DEEP: &str = "(reduce (fn [acc x] [acc]) [] (range 2000))";

/// Run `f` on a thread whose stack is far too small for a recursive drop of
/// a [`DEEP`] value, so passing proves nothing recursed on the caller.
fn on_small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(128 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn results_nested_too_deeply_are_refused() {
    let e = eval(DEEP).unwrap_err();
    assert_eq!(e.kind, ErrorKind::ResourceLimit, "{}", e.message);
    assert!(e.message.contains("1024"), "{}", e.message);
    let at_limit = format!("(reduce (fn [acc x] [acc]) [] (range {}))", 1023);
    assert!(eval(&at_limit).is_ok());
    let over = format!("(reduce (fn [acc x] [acc]) [] (range {}))", 1024);
    assert_eq!(eval(&over).unwrap_err().kind, ErrorKind::ResourceLimit);
}

#[test]
fn deep_globals_stay_inside_and_are_dropped_safely() {
    on_small_stack(deep_globals_body);
}

fn deep_globals_body() {
    let mut s = session();
    s.eval(&format!("(def deep {DEEP}) nil"), &mut NoHost)
        .unwrap();
    assert_eq!(s.get("deep").unwrap_err().kind, ErrorKind::ResourceLimit);
    assert_eq!(s.globals().unwrap_err().kind, ErrorKind::ResourceLimit);
    // Scripts can still use it.
    assert_eq!(s.eval("(count deep)", &mut NoHost).unwrap(), json!(1));
    s.eval(&format!("(def deep2 {DEEP}) nil"), &mut NoHost)
        .unwrap();
    assert!(s.unset("deep"));
    s.set("deep2", json!(1)).unwrap();
    s.eval(
        &format!("(def deep3 {DEEP}) (def f (let [d deep3] (fn [] d))) nil"),
        &mut NoHost,
    )
    .unwrap();
    drop(s);
}

#[test]
fn deep_values_are_refused_on_the_way_in() {
    on_small_stack(|| {
        let mut deep = json!(1);
        for _ in 0..2000 {
            deep = Value::Array(vec![deep]);
        }
        let mut s = session();
        assert_eq!(s.set("d", deep).unwrap_err().kind, ErrorKind::ResourceLimit);
    });
}

#[test]
fn callback_arguments_nested_too_deeply_are_refused() {
    let mut s = session();
    s.define("take").unwrap();
    let mut host = Funcs::default().with("take", |_| Ok(json!(null)));
    let e = s.eval(&format!("(take {DEEP})"), &mut host).unwrap_err();
    assert!(e.message.contains("1024"), "{}", e.message);
    assert!(host.calls.is_empty());
}

#[test]
fn deep_results_are_dropped_off_the_caller_stack() {
    on_small_stack(|| {
        let e = eval(DEEP).unwrap_err();
        assert_eq!(e.kind, ErrorKind::ResourceLimit);
    });
}

// ---------------------------------------------------------------- limits

#[test]
fn limit_presets() {
    let d = fuel_limits(None, &[]).unwrap();
    assert_eq!(d, fuel_limits(Some("default"), &[]).unwrap());
    assert_eq!(d.max_steps, 50_000_000);
    assert_eq!(
        fuel_limits(Some("strict"), &[]).unwrap().max_steps,
        1_000_000
    );
    assert_eq!(
        fuel_limits(Some("unlimited"), &[]).unwrap().max_steps,
        u64::MAX
    );
    let e = fuel_limits(Some("loose"), &[]).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Argument);
    assert!(e.message.contains("loose"));
}

#[test]
fn limit_overrides_and_clamping() {
    let l = fuel_limits(
        Some("strict"),
        &[
            ("max_steps".into(), 10),
            ("max_call_depth".into(), 1_000_000),
            ("max_eval_depth".into(), 1_000_000),
            ("max_value_len".into(), 5),
            ("max_total_alloc".into(), 6),
        ],
    )
    .unwrap();
    assert_eq!(l.max_steps, 10);
    assert_eq!(l.max_call_depth, 4_096);
    assert_eq!(l.max_eval_depth, 16_384);
    assert_eq!(l.max_value_len, 5);
    assert_eq!(l.max_total_alloc, 6);
    let e = fuel_limits(None, &[("max_stepz".into(), 1)]).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Argument);
    assert!(e.message.contains("max_stepz"));
}

#[test]
fn sandbox_limit_overrides() {
    let l = sandbox_limits(&[
        ("max_file_bytes".into(), 1),
        ("max_include_depth".into(), 2),
        ("max_list_entries".into(), 3),
        ("max_glob_depth".into(), 4),
    ])
    .unwrap();
    assert_eq!(
        (
            l.max_file_bytes,
            l.max_include_depth,
            l.max_list_entries,
            l.max_glob_depth
        ),
        (1, 2, 3, 4)
    );
    assert_eq!(
        sandbox_limits(&[("max_files".into(), 1)]).unwrap_err().kind,
        ErrorKind::Argument
    );
}

#[test]
fn max_value_len_is_enforced() {
    let opts = Options {
        limits: fuel_limits(None, &[("max_value_len".into(), 10)]).unwrap(),
        ..Options::default()
    };
    let e = Session::new(opts)
        .unwrap()
        .eval("(range 100)", &mut NoHost)
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::ResourceLimit, "{}", e.message);
}

// ---------------------------------------------------------------- sandbox

fn memory(files: &[(&str, &str)]) -> Session {
    let opts = Options {
        sandbox: SandboxSpec::Memory {
            files: files
                .iter()
                .map(|(p, c)| (p.to_string(), c.to_string()))
                .collect(),
        },
        ..Options::default()
    };
    Session::new(opts).unwrap()
}

#[test]
fn memory_sandbox_serves_include_read_and_list() {
    let mut s = memory(&[
        ("main.trongo", "{:db (include \"lib/db.trongo\")}"),
        ("/lib/db.trongo", "{:host \"localhost\" :port 5432}"),
        ("notes.txt", "hello"),
    ]);
    assert_eq!(
        s.eval("(include \"main.trongo\")", &mut NoHost).unwrap(),
        json!({"db": {"host": "localhost", "port": 5432}})
    );
    assert_eq!(
        s.eval("(read-file \"/notes.txt\")", &mut NoHost).unwrap(),
        json!("hello")
    );
    let listing = s.eval("(list-files \"lib\")", &mut NoHost).unwrap();
    assert!(listing.to_string().contains("db.trongo"), "{listing}");
}

#[test]
fn eval_file_resolves_includes_relative_to_the_file() {
    let mut s = memory(&[
        ("app/main.trongo", "(include \"part.trongo\")"),
        ("app/part.trongo", "{:part true}"),
    ]);
    assert_eq!(
        s.eval_file("app/main.trongo", &mut NoHost).unwrap(),
        json!({"part": true})
    );
    // A path with quotes or backslashes is data, not source text.
    let e = s.eval_file("a\"b\\c.trongo", &mut NoHost).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Sandbox, "{}", e.message);
}

#[test]
fn memory_sandbox_cannot_escape() {
    let mut s = memory(&[("a.txt", "a")]);
    for p in ["../a.txt", "/../../etc/passwd", "/etc/passwd"] {
        let e = s
            .eval(&format!("(read-file \"{p}\")"), &mut NoHost)
            .unwrap_err();
        assert_eq!(e.kind, ErrorKind::Sandbox, "{p}: {}", e.message);
    }
}

#[test]
fn memory_sandbox_validates_keys() {
    let bad = |files: Vec<(String, String)>| {
        Session::new(Options {
            sandbox: SandboxSpec::Memory { files },
            ..Options::default()
        })
        .err()
        .unwrap()
    };
    assert_eq!(bad(vec![]).kind, ErrorKind::Argument);
    for key in ["../x", "a/../../x", "", "/", "./"] {
        let e = bad(vec![(key.into(), "x".into())]);
        assert_eq!(e.kind, ErrorKind::Argument, "{key:?}");
    }
}

#[test]
fn include_cycles_are_refused() {
    let mut s = memory(&[
        ("a.trongo", "(include \"b.trongo\")"),
        ("b.trongo", "(include \"a.trongo\")"),
    ]);
    let e = s.eval("(include \"a.trongo\")", &mut NoHost).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Sandbox, "{}", e.message);
}

#[test]
fn sandbox_limits_apply() {
    let opts = Options {
        sandbox: SandboxSpec::Memory {
            files: vec![("big.txt".into(), "x".repeat(100))],
        },
        sandbox_limits: sandbox_limits(&[("max_file_bytes".into(), 10)]).unwrap(),
        ..Options::default()
    };
    let e = Session::new(opts)
        .unwrap()
        .eval("(read-file \"big.txt\")", &mut NoHost)
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Sandbox, "{}", e.message);
    assert!(e.message.contains("too large"), "{}", e.message);
}

#[test]
fn directory_sandbox_with_mounts() {
    let root = scratch("root");
    let extra = scratch("extra");
    fs::write(root.join("a.txt"), "root file").unwrap();
    fs::write(extra.join("b.txt"), "mounted file").unwrap();
    let opts = Options {
        sandbox: SandboxSpec::Dir {
            root: root.clone(),
            mounts: vec![("/vendor".into(), extra.clone())],
        },
        ..Options::default()
    };
    let mut s = Session::new(opts).unwrap();
    assert_eq!(
        s.eval(
            "[(read-file \"a.txt\") (read-file \"/vendor/b.txt\")]",
            &mut NoHost
        )
        .unwrap(),
        json!(["root file", "mounted file"])
    );
    let e = s
        .eval("(read-file \"../outside\")", &mut NoHost)
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Sandbox);
}

#[cfg(unix)]
#[test]
fn directory_sandbox_refuses_escaping_symlinks() {
    let root = scratch("symroot");
    let outside = scratch("outside");
    fs::write(outside.join("secret"), "nope").unwrap();
    std::os::unix::fs::symlink(outside.join("secret"), root.join("link")).unwrap();
    let opts = Options {
        sandbox: SandboxSpec::Dir {
            root,
            mounts: vec![],
        },
        ..Options::default()
    };
    let e = Session::new(opts)
        .unwrap()
        .eval("(read-file \"link\")", &mut NoHost)
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Sandbox, "{}", e.message);
}

#[test]
fn bad_sandbox_configuration_is_reported() {
    let missing = Options {
        sandbox: SandboxSpec::Dir {
            root: "/definitely/not/here".into(),
            mounts: vec![],
        },
        ..Options::default()
    };
    let e = Session::new(missing).err().unwrap();
    assert_eq!(e.kind, ErrorKind::Sandbox);
    assert!(e.message.contains("/definitely/not/here"), "{}", e.message);

    let root = scratch("mounts");
    for mount in ["relative", "/"] {
        let opts = Options {
            sandbox: SandboxSpec::Dir {
                root: root.clone(),
                mounts: vec![(mount.into(), "/definitely/not/here".into())],
            },
            ..Options::default()
        };
        assert_eq!(Session::new(opts).err().unwrap().kind, ErrorKind::Sandbox);
    }
    let dup = Options {
        sandbox: SandboxSpec::Dir {
            root: root.clone(),
            mounts: vec![("/x".into(), root.clone()), ("/x/".into(), root.clone())],
        },
        ..Options::default()
    };
    assert_eq!(Session::new(dup).err().unwrap().kind, ErrorKind::Sandbox);
}

// ---------------------------------------------------------------- host functions

#[test]
fn host_functions_receive_arguments_and_return_values() {
    let mut s = session();
    s.define("add-host").unwrap();
    let mut host = Funcs::default().with("add-host", |args| {
        Ok(json!(args.iter().map(|a| a.as_i64().unwrap()).sum::<i64>()))
    });
    let v = s.eval("(add-host 1 2 (add-host 3 4))", &mut host).unwrap();
    assert_eq!(v, json!(10));
    assert_eq!(
        host.calls,
        vec![
            ("add-host".to_string(), vec![json!(3), json!(4)]),
            ("add-host".to_string(), vec![json!(1), json!(2), json!(7)]),
        ]
    );
}

#[test]
fn host_functions_work_inside_higher_order_builtins() {
    let mut s = session();
    s.define("sq").unwrap();
    let mut host = Funcs::default().with("sq", |a| Ok(json!(a[0].as_i64().unwrap().pow(2))));
    let v = s.eval("(map (fn [x] (sq x)) [1 2 3])", &mut host).unwrap();
    assert_eq!(v, json!([1, 4, 9]));
}

#[test]
fn host_function_failure_is_a_callback_error() {
    let mut s = session();
    s.define("boom").unwrap();
    let mut host = Funcs::default().with("boom", |_| Err("kaboom".into()));
    let e = s.eval("(+ 1 (boom))", &mut host).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Callback);
    assert!(e.message.contains("kaboom"), "{}", e.message);
    assert!(e.message.contains("boom"), "{}", e.message);
}

#[test]
fn host_functions_shadow_builtins_and_can_be_removed() {
    let mut s = session();
    s.define("upcase").unwrap();
    let mut host = Funcs::default().with("upcase", |_| Ok(json!("mine")));
    assert_eq!(s.eval("(upcase \"a\")", &mut host).unwrap(), json!("mine"));
    assert_eq!(s.functions(), vec!["upcase".to_string()]);
    assert!(s.undefine("upcase"));
    assert!(!s.undefine("upcase"));
    assert_eq!(s.eval("(upcase \"a\")", &mut host).unwrap(), json!("A"));
}

#[test]
fn invalid_host_function_names_are_rejected() {
    assert_eq!(
        session().define("a b").unwrap_err().kind,
        ErrorKind::Argument
    );
}

#[test]
fn host_functions_can_evaluate_in_another_session() {
    struct Nested;
    impl Host for Nested {
        fn call(&mut self, _: &str, args: Vec<Value>) -> Result<Value, String> {
            let src = args[0].as_str().unwrap().to_string();
            session().eval(&src, &mut NoHost).map_err(|e| e.message)
        }
    }
    let mut s = session();
    s.define("inner").unwrap();
    let v = s.eval("(+ 1 (inner \"(* 6 7)\"))", &mut Nested).unwrap();
    assert_eq!(v, json!(43));
}

#[test]
fn a_wake_refuses_later_callbacks() {
    struct Interrupting {
        woke: bool,
        calls: usize,
    }
    impl Host for Interrupting {
        fn call(&mut self, _: &str, _: Vec<Value>) -> Result<Value, String> {
            self.calls += 1;
            Ok(json!(1))
        }
        fn wait(&mut self, events: &Events) -> Event {
            if !self.woke {
                self.woke = true;
                events.waker().wake();
            }
            events.recv()
        }
    }
    let mut s = session();
    s.define("tick").unwrap();
    let mut host = Interrupting {
        woke: false,
        calls: 0,
    };
    let e = s.eval("(tick) (tick)", &mut host).unwrap_err();
    assert!(e.message.contains("interrupted"), "{}", e.message);
    assert_eq!(host.calls, 0);
    // The next evaluation starts clean.
    let mut host = Funcs::default().with("tick", |_| Ok(json!(1)));
    assert_eq!(s.eval("(tick)", &mut host).unwrap(), json!(1));
}

#[test]
fn a_wake_without_callbacks_does_not_change_the_result() {
    struct WakeOnce(bool);
    impl Host for WakeOnce {
        fn call(&mut self, _: &str, _: Vec<Value>) -> Result<Value, String> {
            unreachable!()
        }
        fn wait(&mut self, events: &Events) -> Event {
            if !self.0 {
                self.0 = true;
                events.waker().wake();
            }
            events.recv()
        }
    }
    assert_eq!(
        session().eval("(+ 1 2)", &mut WakeOnce(false)).unwrap(),
        json!(3)
    );
}

#[test]
fn sessions_run_concurrently() {
    let handles: Vec<_> = (0..16)
        .map(|i| {
            std::thread::spawn(move || {
                let mut s = session();
                s.define("id").unwrap();
                let mut host = Funcs::default().with("id", |a| Ok(a[0].clone()));
                for j in 0..20 {
                    let v = s.eval(&format!("(+ (id {i}) {j})"), &mut host).unwrap();
                    assert_eq!(v, json!(i + j));
                }
            })
        })
        .collect();
    for h in handles {
        h.join().unwrap();
    }
}

// ---------------------------------------------------------------- calling

#[test]
fn call_by_name_reaches_builtins_defns_and_host_functions() {
    let mut s = session();
    s.eval("(defn greet [who] (str \"hi \" who))", &mut NoHost)
        .unwrap();
    s.define("twice").unwrap();
    let mut host = Funcs::default().with("twice", |a| Ok(json!(a[0].as_i64().unwrap() * 2)));
    assert_eq!(
        s.call("greet", &[json!("bo")], &mut host).unwrap(),
        json!("hi bo")
    );
    assert_eq!(
        s.call("upcase", &[json!("x")], &mut host).unwrap(),
        json!("X")
    );
    assert_eq!(s.call("twice", &[json!(21)], &mut host).unwrap(), json!(42));
    assert_eq!(
        s.call("nope", &[], &mut host).unwrap_err().kind,
        ErrorKind::Eval
    );
    assert_eq!(
        s.call("a b", &[], &mut host).unwrap_err().kind,
        ErrorKind::Argument
    );
}

#[test]
fn call_arguments_are_data_not_code() {
    let mut s = session();
    s.eval("(defn ident [x] x)", &mut NoHost).unwrap();
    for v in [
        json!("(+ 1 2)"),
        json!("#{x}"),
        json!("${x}"),
        json!({"k": "(boom)", "nested": [1, "two", null, {"x": false}]}),
        json!([]),
        json!({}),
        json!(-0.5),
        json!(i64::MIN),
        json!(i64::MAX),
    ] {
        assert_eq!(
            s.call("ident", std::slice::from_ref(&v), &mut NoHost)
                .unwrap(),
            v
        );
    }
}

#[test]
fn call_value_invokes_function_values() {
    let mut s = session();
    let f = s
        .eval("(let [k 10] (fn [x] (+ x k)))", &mut NoHost)
        .unwrap();
    assert_eq!(
        s.call_value(&f, &[json!(5)], &mut NoHost).unwrap(),
        json!(15)
    );
    let e = s.call_value(&json!("nope"), &[], &mut NoHost).unwrap_err();
    assert_eq!(e.kind, ErrorKind::Eval);
}

// ---------------------------------------------------------------- render / embed

#[test]
fn renders_templates() {
    let mut s = session();
    s.set("name", json!("Ada")).unwrap();
    let out = s
        .render(
            "Hello <%= (upcase name) %>!<% (def n 3) %> n=<%= n %>",
            &mut NoHost,
        )
        .unwrap();
    assert_eq!(out, "Hello ADA! n=3");
    assert_eq!(s.render("plain", &mut NoHost).unwrap(), "plain");
    assert_eq!(
        s.render("<%= (+ 1 %>", &mut NoHost).unwrap_err().kind,
        ErrorKind::Parse
    );
}

#[test]
fn embeds_into_parsed_documents() {
    let mut s = session();
    let doc = json!({
        "port": "!trongo (+ 8000 80)",
        "list": ["!trongo (upcase \"a\")", "plain", 1],
        "map": {"!trongo": "{:x 1}"},
        "untouched": "!trongoish",
    });
    let out = s.embed(doc, &Markers::default(), &mut NoHost).unwrap();
    assert_eq!(
        out,
        json!({"port": 8080, "list": ["A", "plain", 1], "map": {"x": 1}, "untouched": "!trongoish"})
    );
}

#[test]
fn embed_uses_custom_markers() {
    let markers = Markers {
        prefix: "=".into(),
        key: "$eval".into(),
        tag: "!calc".into(),
    };
    let mut s = session();
    let out = s
        .embed(
            json!({"a": "=(+ 1 1)", "b": {"$eval": "(+ 2 2)"}, "c": "!trongo (+ 1 1)"}),
            &markers,
            &mut NoHost,
        )
        .unwrap();
    assert_eq!(out, json!({"a": 2, "b": 4, "c": "!trongo (+ 1 1)"}));
    let out = s
        .embed_yaml("a: !calc (+ 1 2)\nb: !other x\n", &markers, &mut NoHost)
        .unwrap();
    assert_eq!(out, json!({"a": 3, "b": "x"}));
}

#[test]
fn embeds_yaml_with_tags() {
    let mut s = session();
    let yaml = "\
port: !trongo (+ 8000 80)
name: \"!trongo (upcase \\\"x\\\")\"
nested:
  - !trongo '{:a 1}'
  - keep
";
    let out = s
        .embed_yaml(yaml, &Markers::default(), &mut NoHost)
        .unwrap();
    assert_eq!(
        out,
        json!({"port": 8080, "name": "X", "nested": [{"a": 1}, "keep"]})
    );
    let e = s
        .embed_yaml("a: [1", &Markers::default(), &mut NoHost)
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Parse);
    assert!(e.message.starts_with("yaml:"), "{}", e.message);
}

#[test]
fn embed_errors_carry_the_snippet_error() {
    let mut s = session();
    let e = s
        .embed(
            json!({"a": "!trongo (+ 1"}),
            &Markers::default(),
            &mut NoHost,
        )
        .unwrap_err();
    assert_eq!(e.kind, ErrorKind::Parse);
}

#[test]
fn to_trongo_round_trips() {
    let v = json!({"a": [1, 2.5, "s\"#{x}", null, true], "b": {}});
    for pretty in [false, true] {
        let src = trongoscript_host::to_trongo(&v, pretty);
        assert_eq!(eval(&src).unwrap(), v, "{src}");
    }
}

// Copyright (c) 2026 Durable Programming, LLC. All rights reserved.
