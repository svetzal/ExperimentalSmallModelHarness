//! Structural guard for `GENERALIZATION_PLAN.md` Slice 3 ("Extract Coding
//! Policy"): the runtime core must contain no Rust/Cargo/Bevy/test-runner
//! names. Domain-specific literals belong behind `crate::profile`.
//!
//! This test reads each core production source file, strips out
//! `#[cfg(test)] mod tests { ... }` blocks (whose unit tests are explicitly
//! allowed to reference languages/tools they exercise), and asserts the
//! absence of a fixed list of forbidden domain tokens using a
//! case-insensitive, word-boundary-aware substring search — so build-time
//! identifiers like `CARGO_MANIFEST_DIR`/`CARGO_PKG_VERSION` (which contain
//! "cargo" only as a sub-word inside a larger identifier, not as a
//! standalone word) do not false-positive.
//!
//! Exclusions, and why:
//! - `src/profile/**` — this *is* the coding domain profile; it is expected,
//!   required, and allowed to name Rust/Cargo/etc explicitly.
//! - `src/baseline.rs` — embeds the preserved 30-cell demo matrix, whose
//!   task identities (e.g. `"rust-binary-search-tree"`) are historical
//!   evidence data, not runtime policy.
//! - `fixtures/**`, `baseline/**` (the preserved-evidence directory) — data,
//!   not runtime source.
//! - Build/provenance metadata (`env!("CARGO_MANIFEST_DIR")`,
//!   `env!("CARGO_PKG_VERSION")`) — these are the harness's own Rust build
//!   identifiers, inherent to how *any* Rust crate is built, not domain
//!   policy about what a coding profile validates. They are naturally
//!   excluded by the word-boundary check rather than a path exclusion, since
//!   "cargo" only appears as a sub-word of a larger identifier.

use std::fs;
use std::path::Path;

/// Core production source files that must contain no domain-specific
/// literals. `src/profile/**`, `src/baseline.rs` are intentionally excluded
/// (see module doc).
const CORE_SOURCE_FILES: &[&str] = &[
    "src/agent.rs",
    "src/acceptance_plan.rs",
    "src/tools.rs",
    "src/contract.rs",
    "src/cli.rs",
    "src/trace.rs",
    "src/trace_analysis.rs",
    "src/runtime_events.rs",
    "src/runtime.rs",
    "src/initial_context.rs",
    "src/semantic_advisory.rs",
    "src/provenance.rs",
    "src/retry.rs",
    "src/lib.rs",
    "src/main.rs",
];

/// Forbidden domain tokens: coding-language/build-tool/test-runner names
/// that must not appear in the generic runtime core.
const FORBIDDEN_TOKENS: &[&str] = &[
    "cargo", "bevy", "rust", "rustc", "rustfmt", "clippy", "pytest", "npm", "pnpm", "yarn",
    "go test", "gradle", "mvn", "mix test", "rspec",
];

/// Remove every `#[cfg(test)]\nmod tests { ... }` block from `source`,
/// matching nested braces so the whole test module (including any nested
/// blocks) is stripped, not just up to the first inner `}`.
fn strip_test_modules(source: &str) -> String {
    let mut result = String::new();
    let mut rest = source;
    loop {
        let Some(attr_offset) = rest.find("#[cfg(test)]") else {
            result.push_str(rest);
            break;
        };
        // Find "mod tests" after the attribute (allow for other attributes
        // or whitespace/comments between the attribute and the mod, as long
        // as we land on this specific pattern; if not found treat as plain
        // text and move past the attribute to avoid an infinite loop).
        let after_attr = &rest[attr_offset..];
        let Some(mod_rel_offset) = after_attr.find("mod tests") else {
            result.push_str(&rest[..attr_offset + "#[cfg(test)]".len()]);
            rest = &rest[attr_offset + "#[cfg(test)]".len()..];
            continue;
        };
        let mod_offset = attr_offset + mod_rel_offset;
        let Some(brace_rel_offset) = rest[mod_offset..].find('{') else {
            result.push_str(&rest[..attr_offset + "#[cfg(test)]".len()]);
            rest = &rest[attr_offset + "#[cfg(test)]".len()..];
            continue;
        };
        let open_brace = mod_offset + brace_rel_offset;

        // Keep everything before the attribute.
        result.push_str(&rest[..attr_offset]);

        // Walk forward from the opening brace, tracking nesting depth
        // (ignoring braces inside string/char literals is unnecessary here:
        // this is our own well-formed source, and any mismatch just means
        // we consume to end-of-file, which still satisfies "stripped").
        let bytes = rest.as_bytes();
        let mut depth = 0usize;
        let mut idx = open_brace;
        let mut close_brace = rest.len();
        while idx < bytes.len() {
            match bytes[idx] {
                b'{' => depth += 1,
                b'}' => {
                    depth -= 1;
                    if depth == 0 {
                        close_brace = idx + 1;
                        break;
                    }
                }
                _ => {}
            }
            idx += 1;
        }

        rest = &rest[close_brace..];
    }
    result
}

#[test]
fn runtime_reducer_and_policy_are_effect_free() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let path = repo_root.join("src/runtime.rs");
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    let production = strip_test_modules(&source);
    let forbidden = [
        "use mojentic",
        "use tokio",
        "use std::fs",
        "use std::process",
        "use std::time",
        "use chrono",
        "use tracing",
        "crate::trace::",
        "crate::profile::",
        "crate::profile",
    ];
    let found = forbidden
        .iter()
        .filter(|token| production.contains(**token))
        .copied()
        .collect::<Vec<_>>();
    assert!(
        found.is_empty(),
        "runtime reducer/policy must not import provider, filesystem, subprocess, clock, tracing, or profile effects: {found:?}"
    );
}

#[test]
fn orchestration_adapters_do_not_reintroduce_transition_state() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let forbidden = [
        ("src/tools.rs", "struct ToolPolicyState"),
        ("src/agent.rs", "struct ActionBoundaryNoActionTracker"),
        ("src/agent.rs", "struct RepairNoActionTracker"),
    ];
    for (relative, token) in forbidden {
        let source = fs::read_to_string(repo_root.join(relative)).unwrap();
        assert!(
            !strip_test_modules(&source).contains(token),
            "{relative} must delegate transition ownership to RuntimeState: found {token}"
        );
    }
}

/// Case-insensitive, word-boundary-aware substring search: `needle` must be
/// preceded and followed by a non-alphanumeric, non-underscore character (or
/// the start/end of the haystack) to count as a match. This lets
/// `CARGO_MANIFEST_DIR` avoid matching `cargo` (no boundary before the `_`)
/// while `Cargo.toml`, `cargo test`, and `"npm run"` all still match.
fn contains_word(haystack: &str, needle: &str) -> bool {
    let haystack_lower = haystack.to_ascii_lowercase();
    let needle_lower = needle.to_ascii_lowercase();
    let hay = haystack_lower.as_bytes();
    let need = needle_lower.as_bytes();
    if need.is_empty() || need.len() > hay.len() {
        return false;
    }
    let is_word_byte = |byte: u8| byte.is_ascii_alphanumeric() || byte == b'_';
    for start in 0..=(hay.len() - need.len()) {
        if &hay[start..start + need.len()] != need {
            continue;
        }
        let before_ok = start == 0 || !is_word_byte(hay[start - 1]);
        let end = start + need.len();
        let after_ok = end == hay.len() || !is_word_byte(hay[end]);
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

#[test]
fn core_runtime_files_contain_no_domain_specific_literals() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut failures = Vec::new();

    for relative_path in CORE_SOURCE_FILES {
        let path = repo_root.join(relative_path);
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
        let stripped = strip_test_modules(&source);

        for token in FORBIDDEN_TOKENS {
            if contains_word(&stripped, token) {
                failures.push(format!("{relative_path}: forbidden token {token:?}"));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "core runtime files must contain no Rust/Cargo/Bevy/test-runner names \
         outside #[cfg(test)] mod tests blocks (GENERALIZATION_PLAN.md Slice 3):\n{}",
        failures.join("\n")
    );
}

#[test]
fn core_runtime_has_no_second_domain_identity_or_benchmark_literals() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let guarded = [
        "src/runtime.rs",
        "src/agent.rs",
        "src/tools.rs",
        "src/contract.rs",
        "src/trace_analysis.rs",
    ];
    let forbidden = ["text_transform", "Project Aurora", "brief.md", "Mia Chen"];
    let mut failures = Vec::new();
    for relative in guarded {
        let production = strip_test_modules(&fs::read_to_string(repo_root.join(relative)).unwrap());
        for literal in forbidden {
            if production.contains(literal) {
                failures.push(format!("{relative}: {literal:?}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "generic runtime contains second-domain coupling: {}",
        failures.join(", ")
    );
}

#[test]
fn resolved_run_orchestration_never_reselects_a_default_or_coding_profile() {
    let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let production =
        strip_test_modules(&fs::read_to_string(repo_root.join("src/agent.rs")).unwrap());
    let forbidden = [
        "default_profile(",
        "profile::coding",
        "run_coding_agent",
        "coding_tools",
    ];
    let found = forbidden
        .iter()
        .filter(|token| production.contains(**token))
        .copied()
        .collect::<Vec<_>>();
    assert!(
        found.is_empty(),
        "resolved run orchestration must use the contract-selected profile: {found:?}"
    );
}

#[test]
fn strip_test_modules_removes_nested_braces_and_keeps_surrounding_code() {
    let source = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn helper() { if true { 1 } else { 2 } }\n    const X: &str = \"cargo test\";\n}\nfn b() {}\n";
    let stripped = strip_test_modules(source);
    assert!(stripped.contains("fn a() {}"));
    assert!(stripped.contains("fn b() {}"));
    assert!(!stripped.contains("cargo"));
    assert!(!stripped.contains("mod tests"));
}

#[test]
fn word_boundary_search_ignores_cargo_as_a_sub_word_of_a_larger_identifier() {
    assert!(!contains_word("env!(\"CARGO_MANIFEST_DIR\")", "cargo"));
    assert!(!contains_word("env!(\"CARGO_PKG_VERSION\")", "cargo"));
    assert!(contains_word("run cargo test now", "cargo"));
    assert!(contains_word("Cargo.toml", "cargo"));
}
