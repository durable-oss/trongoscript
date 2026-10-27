//! Lookup behavior when a host registers builtins through inventory.
//!
//! These live in an integration test so the submissions come from outside
//! the crate, the way a real host's would; inside the crate they would trip
//! the guard that every crate builtin is in `CRATE_BUILTINS`.

use trongoscript_stdlib::{find_builtin, find_builtin_for, Builtin, BuiltinResult, Value};

fn host_impl(_: &[Value]) -> BuiltinResult {
    Ok(Value::String("host".into()))
}

// Same name as a core builtin, in a module the host enables.
trongoscript_stdlib::inventory::submit! {
    Builtin::in_module("host_mod", "upcase", 1, 1, host_impl)
}

// A name only the host provides.
trongoscript_stdlib::inventory::submit! {
    Builtin::new("host_only", 1, 1, host_impl)
}

#[test]
fn a_host_builtin_with_a_crate_name_does_not_shadow_it() {
    let b = find_builtin_for("upcase", |_| true).expect("upcase resolves");
    assert_eq!(b.module, None);
    let r = (b.func)(&[Value::String("hi".into())]).unwrap();
    assert_eq!(r, Value::String("HI".into()));
    assert_eq!(find_builtin("upcase").unwrap().module, None);
}

#[test]
fn a_host_only_builtin_resolves() {
    let b = find_builtin_for("host_only", |_| false).expect("host_only resolves");
    assert_eq!(
        (b.func)(&[Value::Null]).unwrap(),
        Value::String("host".into())
    );
}

#[test]
fn a_builtin_in_a_disabled_module_is_not_found() {
    // `sort` exists only in the `collections` module, never in core.
    let sort = find_builtin("sort").expect("sort is registered");
    let module = sort.module.expect("sort is module-scoped");
    assert!(find_builtin_for("sort", |_| false).is_none());
    assert!(find_builtin_for("sort", |m| m == module).is_some());
}

#[test]
fn an_unknown_name_is_not_found() {
    assert!(find_builtin("no-such-builtin").is_none());
    assert!(find_builtin_for("no-such-builtin", |_| true).is_none());
}
