//! 与判定器共用 JSON 归一化表；不接受表外开关。
use serde_json::Value;
use std::collections::BTreeSet;
use std::sync::LazyLock;

static RULES: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../command-normalization.json")).unwrap()
});
fn contains(key: &str, value: &str) -> bool {
    RULES[key].as_array().unwrap().iter().any(|x| x == value)
}
pub fn normalize(argv: &[String]) -> Vec<String> {
    let original = || argv.to_vec();
    if argv.len() < 2 || argv[0] != "cargo" || argv[1] != "test" {
        return original();
    }
    let mut scope = BTreeSet::new();
    let mut section = "cargo";
    let mut filters = 0;
    let mut i = 2;
    while i < argv.len() {
        let arg = &argv[i];
        i += 1;
        if section == "cargo" && arg == "--" {
            section = "test";
            filters = 0;
        } else if contains(&format!("{section}_ignored_flags"), arg) {
            continue;
        } else if section == "cargo" && contains("cargo_scope_flags", arg) {
            scope.insert(arg.clone());
        } else if section == "test"
            && contains(
                "test_positive_integer_options",
                arg.split('=').next().unwrap(),
            )
        {
            let value = if let Some((_, value)) = arg.split_once('=') {
                value
            } else if i < argv.len() {
                i += 1;
                &argv[i - 1]
            } else {
                return original();
            };
            if value.is_empty()
                || !value.bytes().all(|b| b.is_ascii_digit())
                || value.bytes().all(|b| b == b'0')
            {
                return original();
            }
        } else if !arg.is_empty()
            && !arg.starts_with('-')
            && filters < RULES[format!("{section}_filter_limit")].as_u64().unwrap()
        {
            filters += 1;
        } else {
            return original();
        }
    }
    argv[..2].iter().cloned().chain(scope).collect()
}
