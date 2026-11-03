//! The interpreter must not pretend an unsupported call succeeded.
//!
//! Mods whose client API surface is not implemented still return normally from
//! their entrypoints, because every unresolved class and method yields `null`.
//! `trace_missing` makes that visible so a caller can tell "ran" from "did
//! nothing", which is the difference between a mod being loaded and a mod
//! being supported.

use hfc_runtime::interp::Interpreter;
use hfc_runtime::value::Value;

#[test]
fn an_unimplemented_class_is_null_and_recorded() {
    let mut interp = Interpreter::new();
    interp.trace_missing = true;

    let value = interp
        .execute_method(
            "hfc/runtime/NotImplemented",
            "run",
            "()Lhfc/runtime/NotImplemented;",
            &[],
        )
        .expect("an unknown class is a null result, not a failure");

    assert_eq!(value, Value::NULL);
    assert_eq!(interp.missing, ["class hfc/runtime/NotImplemented"]);
}

#[test]
fn nothing_is_recorded_when_tracing_is_off() {
    let mut interp = Interpreter::new();

    let value = interp
        .execute_method("hfc/runtime/NotImplemented", "run", "()V", &[])
        .expect("still null");

    assert_eq!(value, Value::NULL);
    assert!(
        interp.missing.is_empty(),
        "the default interpreter pays no allocation for symbols it skips"
    );
}
