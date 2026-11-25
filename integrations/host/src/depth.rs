//! Keeping deeply nested values away from small stacks.
//!
//! `serde_json::Value`'s `Drop` and `Clone` recurse once per nesting level,
//! and so do PHP's array destructor and the naive conversions a binding
//! would write. A script can cheaply build a value far deeper than any of
//! those survive on an ordinary thread:
//!
//! ```text
//! (reduce (fn [acc _] [acc]) [] (range 100000))
//! ```
//!
//! costs almost no eval depth, because the nesting is built iteratively, but
//! produces a list 100,000 levels deep. Values crossing into the host are
//! therefore capped at [`MAX_NESTING`] levels, checked without recursion,
//! and anything rejected is dropped on the evaluator's large stack.

use trongoscript::Value;

/// Deepest a value handed to or received from the host may nest.
///
/// Generous for configuration data (`JSON.parse` stops at 100 by default,
/// PHP's `json_decode` at 512), and shallow enough that recursive code in a
/// host interpreter handles it on a 1 MiB stack.
pub const MAX_NESTING: usize = 1024;

/// Whether `v` nests more than `max` containers deep. A scalar has depth 0;
/// `[]` and `[1]` have depth 1.
pub fn exceeds_nesting(v: &Value, max: usize) -> bool {
    let mut stack: Vec<(&Value, usize)> = vec![(v, 0)];
    while let Some((v, depth)) = stack.pop() {
        let children: Box<dyn Iterator<Item = &Value>> = match v {
            Value::Array(items) => Box::new(items.iter()),
            Value::Object(map) => Box::new(map.values()),
            _ => continue,
        };
        let depth = depth + 1;
        if depth > max {
            return true;
        }
        stack.extend(children.map(|c| (c, depth)));
    }
    false
}

/// Drop `v` without recursing, however deeply it nests.
pub fn drop_flat(v: Value) {
    let mut stack = vec![v];
    while let Some(mut v) = stack.pop() {
        match &mut v {
            Value::Array(items) => stack.append(items),
            Value::Object(map) => {
                stack.extend(std::mem::take(map).into_iter().map(|(_, v)| v));
            }
            _ => {}
        }
        // `v` is now an empty container or a scalar; dropping it is shallow.
    }
}

/// The error for a value that nests too deeply to hand over.
pub(crate) fn too_deep(what: &str) -> crate::HostError {
    crate::HostError::new(
        crate::ErrorKind::ResourceLimit,
        format!("{what} nests deeper than {MAX_NESTING} levels"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn nested(depth: usize) -> Value {
        let mut v = json!(1);
        for i in 0..depth {
            // Not `json!`: it re-serializes its argument, which is
            // quadratic here and recursive besides.
            v = if i % 2 == 0 {
                Value::Array(vec![v])
            } else {
                Value::Object([("k".to_string(), v)].into_iter().collect())
            };
        }
        v
    }

    #[test]
    fn measures_depth() {
        assert!(!exceeds_nesting(&json!(1), 0));
        assert!(exceeds_nesting(&json!([]), 0));
        assert!(!exceeds_nesting(&json!([]), 1));
        assert!(!exceeds_nesting(&nested(10), 10));
        assert!(exceeds_nesting(&nested(11), 10));
        // Depth is the deepest branch, not the first one.
        assert!(exceeds_nesting(&json!([1, {"a": [[2]]}]), 3));
        assert!(!exceeds_nesting(&json!([1, {"a": [[2]]}]), 4));
    }

    #[test]
    fn drops_values_too_deep_for_the_test_stack() {
        // Built on a thread with room for it, so the recursive `Drop`
        // this replaces is the only thing that could overflow here.
        let v = std::thread::Builder::new()
            .stack_size(256 * 1024 * 1024)
            .spawn(|| nested(1_000_000))
            .unwrap()
            .join()
            .unwrap();
        assert!(exceeds_nesting(&v, MAX_NESTING));
        drop_flat(v);
    }
}

// Copyright (c) 2026 Durable Programming, LLC.
