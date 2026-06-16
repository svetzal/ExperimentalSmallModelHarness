use crate::trace::TraceRecorder;
use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use mojentic::llm::tools::{FunctionDescriptor, LlmTool, ToolDescriptor, ToolRunCtx};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::UNIX_EPOCH;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::time::{Duration, timeout};

const MAX_CONSECUTIVE_WRITES_WITHOUT_SHELL: usize = 3;
const MAX_CAPTURED_OUTPUT_BYTES: usize = 20_000;
const DEFAULT_READ_MAX_BYTES: usize = 20_000;
const DEFAULT_TREE_MAX_DEPTH: usize = 4;
const DEFAULT_TREE_MAX_ENTRIES: usize = 200;
const DEFAULT_SHELL_TIMEOUT_SECS: u64 = 300;
const MAX_SHELL_TIMEOUT_SECS: u64 = 1800;
const PATCH_TIMEOUT_SECS: u64 = 300;
const APPROX_CHARS_PER_TOKEN: usize = 4;
const MAX_SHELL_MUTATION_HASH_BYTES: u64 = 2_000_000;

#[derive(Debug, Clone)]
pub struct ToolScope {
    root: Arc<PathBuf>,
    trace: Arc<TraceRecorder>,
    policy: Arc<Mutex<ToolPolicyState>>,
    access: Arc<AccessPolicy>,
}

#[derive(Debug, Default)]
struct ToolPolicyState {
    total_tool_calls: usize,
    consecutive_writes_without_shell: usize,
    writes_since_shell_probe: usize,
    writes_since_shell_probe_paths: BTreeMap<String, usize>,
    total_write_operations: usize,
    total_shell_probes: usize,
    validation_repair: Option<ValidationRepairState>,
    validation_repair_read_paths: BTreeMap<String, usize>,
    repeated_command_failures: HashMap<String, usize>,
    repeated_failure_summaries: HashMap<String, usize>,
    patch_fallbacks_by_file: BTreeMap<String, PatchFallbackState>,
    total_tool_result_chars: usize,
    max_tool_result_chars: usize,
    max_tool_result_kind: Option<String>,
    tool_result_chars_by_kind: BTreeMap<String, usize>,
    emitted_first_source_mutation: bool,
    emitted_first_validation_probe: bool,
    emitted_first_post_validation_repair_action: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ToolPolicySnapshot {
    pub total_tool_calls: usize,
    pub consecutive_writes_without_shell: usize,
    pub writes_since_shell_probe: usize,
    pub writes_since_shell_probe_paths: BTreeMap<String, usize>,
    pub validation_required_after_write: bool,
    pub total_write_operations: usize,
    pub total_shell_probes: usize,
    pub validation_repair: Option<ValidationRepairSnapshot>,
    pub validation_repair_read_paths: BTreeMap<String, usize>,
    pub patch_fallbacks: Vec<PatchFallbackSnapshot>,
    pub total_tool_result_chars: usize,
    pub total_tool_result_estimated_tokens: usize,
    pub max_tool_result_chars: usize,
    pub max_tool_result_estimated_tokens: usize,
    pub max_tool_result_kind: Option<String>,
    pub tool_result_chars_by_kind: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ValidationRepairSnapshot {
    pub active: bool,
    pub command: String,
    pub command_family: String,
    pub status: Option<i32>,
    pub failure_text: String,
    pub failure_details: Vec<String>,
    pub repeated_command_family_count: usize,
    pub repeated_failure_summary_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ValidationRepairState {
    command: String,
    command_family: String,
    status: Option<i32>,
    failure_text: String,
    failure_details: Vec<String>,
    repeated_command_family_count: usize,
    repeated_failure_summary_count: usize,
}

impl From<&ValidationRepairState> for ValidationRepairSnapshot {
    fn from(state: &ValidationRepairState) -> Self {
        Self {
            active: true,
            command: state.command.clone(),
            command_family: state.command_family.clone(),
            status: state.status,
            failure_text: state.failure_text.clone(),
            failure_details: state.failure_details.clone(),
            repeated_command_family_count: state.repeated_command_family_count,
            repeated_failure_summary_count: state.repeated_failure_summary_count,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PatchFallbackSnapshot {
    pub path: String,
    pub attempts: usize,
    pub reason: String,
    pub guidance: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PatchFallbackState {
    attempts: usize,
    reason: String,
    guidance: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct ToolPayloadMeasurement {
    kind: String,
    result_chars: usize,
    result_estimated_tokens: usize,
    total_tool_result_chars: usize,
    total_tool_result_estimated_tokens: usize,
    max_tool_result_chars: usize,
    max_tool_result_estimated_tokens: usize,
    max_tool_result_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    len: u64,
    modified_nanos: Option<u128>,
    content_hash: Option<u64>,
}

#[derive(Debug, Default)]
struct AccessPolicy {
    read_allow: Vec<PathRule>,
    write_allow: Vec<PathRule>,
}

#[derive(Debug)]
struct PathRule {
    path: PathBuf,
    recursive: bool,
}

impl ToolScope {
    pub fn new(root: PathBuf, trace: Arc<TraceRecorder>) -> Result<Self> {
        Self::new_with_policy(root, trace, AccessPolicy::default())
    }

    pub fn new_restricted(
        root: PathBuf,
        trace: Arc<TraceRecorder>,
        read_allow: Vec<String>,
        write_allow: Vec<String>,
    ) -> Result<Self> {
        Self::new_with_policy(
            root,
            trace,
            AccessPolicy {
                read_allow: parse_path_rules(read_allow)?,
                write_allow: parse_path_rules(write_allow)?,
            },
        )
    }

    fn new_with_policy(
        root: PathBuf,
        trace: Arc<TraceRecorder>,
        access: AccessPolicy,
    ) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("canonicalizing tool root {}", root.display()))?;
        Ok(Self {
            root: Arc::new(root),
            trace,
            policy: Arc::new(Mutex::new(ToolPolicyState::default())),
            access: Arc::new(access),
        })
    }

    fn resolve_existing_or_new(&self, relative: &str) -> Result<PathBuf> {
        let path = Path::new(relative);
        if path.as_os_str().is_empty() {
            bail!("path must not be empty");
        }
        if path.is_absolute() {
            bail!("absolute paths are outside the tool scope");
        }
        for component in path.components() {
            match component {
                Component::Normal(_) | Component::CurDir => {}
                _ => bail!("path escapes the tool scope: {relative}"),
            }
        }
        Ok(self.root.join(path))
    }

    fn resolve_scoped_path_input(&self, input: &str) -> Result<PathBuf> {
        let trimmed = input.trim();
        if trimmed.is_empty() || trimmed == "." || trimmed == "/" {
            return Ok((*self.root).clone());
        }
        let path = Path::new(trimmed);
        if path.is_absolute() {
            let canonical = path
                .canonicalize()
                .with_context(|| format!("canonicalizing scoped path {}", path.display()))?;
            if !canonical.starts_with(&*self.root) {
                bail!("path escapes the tool scope: {}", canonical.display());
            }
            return Ok(canonical);
        }
        self.resolve_existing_or_new(trimmed)
    }

    fn check_read(&self, path: &Path) -> Result<()> {
        self.check_access("read", path, &self.access.read_allow)
    }

    fn check_write(&self, path: &Path) -> Result<()> {
        self.check_access("write", path, &self.access.write_allow)
    }

    fn check_access(&self, operation: &str, path: &Path, rules: &[PathRule]) -> Result<()> {
        if rules.is_empty() {
            return Ok(());
        }
        let relative = self.relative_path(path)?;
        if rules.iter().any(|rule| rule.matches(&relative)) {
            return Ok(());
        }
        let payload = json!({
            "operation": operation,
            "path": relative.display().to_string(),
            "reason": "path is outside the active packet scope",
        });
        let _ = self.trace.value_event("tool.access.denied", payload);
        bail!("{operation} denied by packet scope: {}", relative.display())
    }

    fn is_read_visible(&self, path: &Path) -> bool {
        if self.access.read_allow.is_empty() {
            return true;
        }
        let Ok(relative) = self.relative_path(path) else {
            return false;
        };
        if relative == Path::new(".") {
            return true;
        }
        self.access
            .read_allow
            .iter()
            .any(|rule| rule.matches(&relative) || rule.is_descendant_of(&relative))
    }

    fn relative_path(&self, path: &Path) -> Result<PathBuf> {
        let relative = path
            .strip_prefix(&*self.root)
            .with_context(|| format!("path escapes the tool scope: {}", path.display()))?;
        Ok(if relative.as_os_str().is_empty() {
            PathBuf::from(".")
        } else {
            relative.to_path_buf()
        })
    }

    fn resolve_existing_dir(&self, relative: Option<&str>) -> Result<PathBuf> {
        let dir = match relative {
            Some(value) if !value.trim().is_empty() => self.resolve_scoped_path_input(value)?,
            _ => (*self.root).clone(),
        };
        let canonical = dir
            .canonicalize()
            .with_context(|| format!("canonicalizing directory {}", dir.display()))?;
        if !canonical.starts_with(&*self.root) {
            bail!("directory escapes the tool scope: {}", canonical.display());
        }
        if !canonical.is_dir() {
            bail!("not a directory: {}", canonical.display());
        }
        Ok(canonical)
    }

    fn resolve_shell_cwd(&self, relative: Option<&str>) -> Result<PathBuf> {
        if relative.is_some_and(|value| !value.trim().is_empty()) {
            return self.resolve_existing_dir(relative);
        }
        if let Some(dir) = self.default_shell_cwd() {
            return Ok(dir);
        }
        self.resolve_existing_dir(None)
    }

    fn default_shell_cwd(&self) -> Option<PathBuf> {
        self.access
            .write_allow
            .iter()
            .filter(|rule| rule.recursive)
            .map(|rule| self.root.join(&rule.path))
            .find(|path| path.is_dir())
    }

    fn relative_display(&self, path: &Path) -> String {
        path.strip_prefix(&*self.root)
            .unwrap_or(path)
            .display()
            .to_string()
    }

    fn note_write_intent(&self, paths: &[PathBuf]) -> Result<()> {
        let relative_paths = paths
            .iter()
            .map(|path| self.relative_display(path))
            .collect::<Vec<_>>();
        let source_paths = relative_paths
            .iter()
            .filter(|path| path_requires_validation_after_write(path))
            .cloned()
            .collect::<Vec<_>>();
        let mut first_source_mutation = false;
        let mut first_post_validation_repair_action = false;
        let mut active_repair = None;
        let total_write_operations;
        let mut policy = self.policy.lock().expect("tool policy mutex poisoned");
        if policy.consecutive_writes_without_shell >= MAX_CONSECUTIVE_WRITES_WITHOUT_SHELL {
            bail!("write budget exhausted: run a shell validation probe before editing again");
        }
        if !source_paths.is_empty() && !policy.emitted_first_source_mutation {
            policy.emitted_first_source_mutation = true;
            first_source_mutation = true;
        }
        if !source_paths.is_empty()
            && policy.validation_repair.is_some()
            && !policy.emitted_first_post_validation_repair_action
        {
            policy.emitted_first_post_validation_repair_action = true;
            first_post_validation_repair_action = true;
            active_repair = policy
                .validation_repair
                .as_ref()
                .map(ValidationRepairSnapshot::from);
        }
        policy.consecutive_writes_without_shell += 1;
        policy.writes_since_shell_probe += 1;
        for relative in relative_paths {
            *policy
                .writes_since_shell_probe_paths
                .entry(relative)
                .or_insert(0) += 1;
        }
        policy.total_write_operations += 1;
        total_write_operations = policy.total_write_operations;
        drop(policy);
        if first_source_mutation {
            self.trace.event(
                "agent.stage.first_source_mutation",
                json!({
                    "action": "write_intent",
                    "paths": source_paths,
                    "total_write_operations": total_write_operations,
                }),
            )?;
        }
        if first_post_validation_repair_action {
            self.trace.event(
                "agent.stage.first_post_validation_repair_action",
                json!({
                    "action": "write_intent",
                    "paths": source_paths,
                    "total_write_operations": total_write_operations,
                    "active_repair": active_repair,
                }),
            )?;
        }
        Ok(())
    }

    fn note_sensed_shell_mutation(&self, paths: &[String]) {
        if paths.is_empty() {
            return;
        }
        let source_paths = paths
            .iter()
            .filter(|path| path_requires_validation_after_write(path))
            .cloned()
            .collect::<Vec<_>>();
        let mut first_source_mutation = false;
        let mut first_post_validation_repair_action = false;
        let mut active_repair = None;
        let total_write_operations;
        let mut policy = self.policy.lock().expect("tool policy mutex poisoned");
        if !source_paths.is_empty() && !policy.emitted_first_source_mutation {
            policy.emitted_first_source_mutation = true;
            first_source_mutation = true;
        }
        if !source_paths.is_empty()
            && policy.validation_repair.is_some()
            && !policy.emitted_first_post_validation_repair_action
        {
            policy.emitted_first_post_validation_repair_action = true;
            first_post_validation_repair_action = true;
            active_repair = policy
                .validation_repair
                .as_ref()
                .map(ValidationRepairSnapshot::from);
        }
        policy.consecutive_writes_without_shell += 1;
        policy.writes_since_shell_probe += 1;
        for relative in paths {
            *policy
                .writes_since_shell_probe_paths
                .entry(relative.clone())
                .or_insert(0) += 1;
        }
        policy.total_write_operations += 1;
        total_write_operations = policy.total_write_operations;
        drop(policy);
        if first_source_mutation {
            let _ = self.trace.event(
                "agent.stage.first_source_mutation",
                json!({
                    "action": "shell_mutation",
                    "paths": source_paths,
                    "total_write_operations": total_write_operations,
                }),
            );
        }
        if first_post_validation_repair_action {
            let _ = self.trace.event(
                "agent.stage.first_post_validation_repair_action",
                json!({
                    "action": "shell_mutation",
                    "paths": source_paths,
                    "total_write_operations": total_write_operations,
                    "active_repair": active_repair,
                }),
            );
        }
    }

    fn shell_mutation_snapshot(&self) -> Result<BTreeMap<String, FileFingerprint>> {
        let gitignore = load_gitignore(&self.root)?;
        let mut snapshot = BTreeMap::new();
        collect_shell_mutation_snapshot(self, &self.root, &gitignore, &mut snapshot)?;
        Ok(snapshot)
    }

    fn note_read_target(&self, path: &Path) {
        let relative = self.relative_display(path);
        let mut policy = self.policy.lock().expect("tool policy mutex poisoned");
        if policy.validation_repair.is_some() {
            *policy
                .validation_repair_read_paths
                .entry(relative)
                .or_insert(0) += 1;
        }
    }

    #[cfg(test)]
    fn note_validation_probe(&self) {
        let mut policy = self.policy.lock().expect("tool policy mutex poisoned");
        policy.consecutive_writes_without_shell = 0;
        policy.writes_since_shell_probe = 0;
        policy.writes_since_shell_probe_paths.clear();
        policy.total_shell_probes += 1;
    }

    fn note_validation_probe_result(
        &self,
        command: &str,
        output: &std::process::Output,
        stdout: &CapturedOutput,
        stderr: &CapturedOutput,
    ) -> Result<Option<ValidationRepairSnapshot>> {
        let command_family = command_family(command);
        let failure_text = failure_summary(&stderr.content, &stdout.content);
        let failure_details = failure_details(&stderr.content, &stdout.content);
        let mut policy = self.policy.lock().expect("tool policy mutex poisoned");
        let first_validation_probe = !policy.emitted_first_validation_probe;
        if first_validation_probe {
            policy.emitted_first_validation_probe = true;
        }
        let first_post_validation_repair_action = policy.validation_repair.is_some()
            && !policy.emitted_first_post_validation_repair_action;
        let active_repair = if first_post_validation_repair_action {
            policy.emitted_first_post_validation_repair_action = true;
            policy
                .validation_repair
                .as_ref()
                .map(ValidationRepairSnapshot::from)
        } else {
            None
        };
        policy.consecutive_writes_without_shell = 0;
        policy.writes_since_shell_probe = 0;
        policy.writes_since_shell_probe_paths.clear();
        policy.total_shell_probes += 1;
        let total_shell_probes = policy.total_shell_probes;
        drop(policy);

        if first_validation_probe {
            self.trace.event(
                "agent.stage.first_validation_probe",
                json!({
                    "command": command,
                    "command_family": &command_family,
                    "status": output.status.code(),
                    "success": output.status.success(),
                    "total_shell_probes": total_shell_probes,
                }),
            )?;
        }
        if first_post_validation_repair_action {
            self.trace.event(
                "agent.stage.first_post_validation_repair_action",
                json!({
                    "action": "validation_probe",
                    "command": command,
                    "command_family": &command_family,
                    "status": output.status.code(),
                    "success": output.status.success(),
                    "total_shell_probes": total_shell_probes,
                    "active_repair": active_repair,
                }),
            )?;
        }

        let mut policy = self.policy.lock().expect("tool policy mutex poisoned");

        if output.status.success() {
            let previous = policy.validation_repair.take();
            policy.validation_repair_read_paths.clear();
            drop(policy);
            if previous.is_some() {
                self.trace.event(
                    "agent.validation.repair_resolved",
                    json!({
                        "command": command,
                        "command_family": command_family,
                    }),
                )?;
            }
            return Ok(None);
        }

        let command_count = {
            let count = policy
                .repeated_command_failures
                .entry(command_family.clone())
                .or_insert(0);
            *count += 1;
            *count
        };
        let summary_count = {
            let count = policy
                .repeated_failure_summaries
                .entry(failure_text.clone())
                .or_insert(0);
            *count += 1;
            *count
        };
        let repair = ValidationRepairState {
            command: command.to_string(),
            command_family,
            status: output.status.code(),
            failure_text,
            failure_details,
            repeated_command_family_count: command_count,
            repeated_failure_summary_count: summary_count,
        };
        let snapshot = ValidationRepairSnapshot::from(&repair);
        policy.validation_repair = Some(repair);
        policy.validation_repair_read_paths.clear();
        drop(policy);
        self.trace
            .event("agent.validation.repair_required", &snapshot)?;
        Ok(Some(snapshot))
    }

    fn note_tool_call(&self) {
        let mut policy = self.policy.lock().expect("tool policy mutex poisoned");
        policy.total_tool_calls += 1;
    }

    fn trace_tool_event(&self, kind: &str, payload: Value) {
        let result_chars = serde_json::to_string(&payload)
            .map(|content| content.len())
            .unwrap_or_default();
        let snapshot = {
            let mut policy = self.policy.lock().expect("tool policy mutex poisoned");
            policy.total_tool_result_chars += result_chars;
            if result_chars > policy.max_tool_result_chars {
                policy.max_tool_result_chars = result_chars;
                policy.max_tool_result_kind = Some(kind.to_string());
            }
            *policy
                .tool_result_chars_by_kind
                .entry(kind.to_string())
                .or_insert(0) += result_chars;
            ToolPayloadMeasurement {
                kind: kind.to_string(),
                result_chars,
                result_estimated_tokens: estimate_tokens(result_chars),
                total_tool_result_chars: policy.total_tool_result_chars,
                total_tool_result_estimated_tokens: estimate_tokens(policy.total_tool_result_chars),
                max_tool_result_chars: policy.max_tool_result_chars,
                max_tool_result_estimated_tokens: estimate_tokens(policy.max_tool_result_chars),
                max_tool_result_kind: policy.max_tool_result_kind.clone(),
            }
        };
        let _ = self.trace.event("tool.payload.measured", &snapshot);
        let _ = self.trace.value_event(kind, payload);
    }

    pub fn policy_snapshot(&self) -> ToolPolicySnapshot {
        let policy = self.policy.lock().expect("tool policy mutex poisoned");
        ToolPolicySnapshot {
            total_tool_calls: policy.total_tool_calls,
            consecutive_writes_without_shell: policy.consecutive_writes_without_shell,
            writes_since_shell_probe: policy.writes_since_shell_probe,
            writes_since_shell_probe_paths: policy.writes_since_shell_probe_paths.clone(),
            validation_required_after_write: policy
                .writes_since_shell_probe_paths
                .keys()
                .any(|path| path_requires_validation_after_write(path)),
            total_write_operations: policy.total_write_operations,
            total_shell_probes: policy.total_shell_probes,
            validation_repair: policy
                .validation_repair
                .as_ref()
                .map(ValidationRepairSnapshot::from),
            validation_repair_read_paths: policy.validation_repair_read_paths.clone(),
            patch_fallbacks: policy
                .patch_fallbacks_by_file
                .iter()
                .map(|(path, state)| PatchFallbackSnapshot {
                    path: path.clone(),
                    attempts: state.attempts,
                    reason: state.reason.clone(),
                    guidance: state.guidance.clone(),
                })
                .collect(),
            total_tool_result_chars: policy.total_tool_result_chars,
            total_tool_result_estimated_tokens: estimate_tokens(policy.total_tool_result_chars),
            max_tool_result_chars: policy.max_tool_result_chars,
            max_tool_result_estimated_tokens: estimate_tokens(policy.max_tool_result_chars),
            max_tool_result_kind: policy.max_tool_result_kind.clone(),
            tool_result_chars_by_kind: policy.tool_result_chars_by_kind.clone(),
        }
    }

    fn note_patch_fallback_choice(
        &self,
        paths: &[PathBuf],
        choice: &str,
        patch_bytes: Option<usize>,
    ) -> Result<()> {
        let active = self.patch_fallbacks_for(paths)?;
        if active.is_empty() {
            return Ok(());
        }
        self.trace.event(
            "tool.patch_file.fallback_choice",
            json!({
                "choice": choice,
                "patch_bytes": patch_bytes,
                "fallbacks": active,
            }),
        )
    }

    fn note_patch_failure(
        &self,
        paths: &[PathBuf],
        reason: &str,
    ) -> Result<Vec<PatchFallbackSnapshot>> {
        let relative_paths = paths
            .iter()
            .map(|path| self.relative_display(path))
            .collect::<Vec<_>>();
        let mut policy = self.policy.lock().expect("tool policy mutex poisoned");
        for path in &relative_paths {
            let fallback =
                policy
                    .patch_fallbacks_by_file
                    .entry(path.clone())
                    .or_insert_with(|| PatchFallbackState {
                        attempts: 0,
                        reason: String::new(),
                        guidance: "Retry with a smaller unified diff. write_file replaces the entire file; use it on existing source only after reading the complete current file and preserving unrelated content.".to_string(),
                    });
            fallback.attempts += 1;
            fallback.reason = reason.to_string();
        }
        let snapshots = policy
            .patch_fallbacks_by_file
            .iter()
            .filter(|(path, _)| relative_paths.contains(path))
            .map(|(path, state)| PatchFallbackSnapshot {
                path: path.clone(),
                attempts: state.attempts,
                reason: state.reason.clone(),
                guidance: state.guidance.clone(),
            })
            .collect::<Vec<_>>();
        drop(policy);
        self.trace.event(
            "tool.patch_file.fallback_recommended",
            json!({
                "paths": relative_paths,
                "reason": reason,
                "fallbacks": snapshots,
            }),
        )?;
        Ok(snapshots)
    }

    fn patch_fallbacks_for(&self, paths: &[PathBuf]) -> Result<Vec<PatchFallbackSnapshot>> {
        let relative_paths = paths
            .iter()
            .map(|path| self.relative_display(path))
            .collect::<Vec<_>>();
        let policy = self.policy.lock().expect("tool policy mutex poisoned");
        Ok(policy
            .patch_fallbacks_by_file
            .iter()
            .filter(|(path, _)| relative_paths.contains(path))
            .map(|(path, state)| PatchFallbackSnapshot {
                path: path.clone(),
                attempts: state.attempts,
                reason: state.reason.clone(),
                guidance: state.guidance.clone(),
            })
            .collect())
    }
}

pub fn coding_tools(scope: &ToolScope) -> Vec<Box<dyn LlmTool>> {
    vec![
        Box::new(ListTreeTool {
            scope: scope.clone(),
        }),
        Box::new(ReadFileTool {
            scope: scope.clone(),
        }),
        Box::new(WriteFileTool {
            scope: scope.clone(),
        }),
        Box::new(EditFileTool {
            scope: scope.clone(),
        }),
        Box::new(ShellCommandTool {
            scope: scope.clone(),
        }),
    ]
}

#[derive(Debug, Clone)]
pub struct ReadFileTool {
    scope: ToolScope,
}

#[async_trait]
impl LlmTool for ReadFileTool {
    async fn run(
        &self,
        args: &HashMap<String, Value>,
        _ctx: &ToolRunCtx,
    ) -> mojentic::Result<Value> {
        self.scope.note_tool_call();
        let result = self.read(args).await.map_err(to_mojentic_error);
        let payload = match &result {
            Ok(value) => value.clone(),
            Err(error) => json!({ "error": error.to_string() }),
        };
        self.scope.trace_tool_event("tool.read_file", payload);
        result
    }

    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            r#type: "function".to_string(),
            function: FunctionDescriptor {
                name: "read_file".to_string(),
                description: "Read a UTF-8 file under the active experiment root.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "line_start": { "type": "integer", "minimum": 1 },
                        "line_end": { "type": "integer", "minimum": 1 },
                        "max_bytes": { "type": "integer", "minimum": 1 }
                    },
                    "required": ["path"]
                }),
            },
        }
    }

    fn clone_box(&self) -> Box<dyn LlmTool> {
        Box::new(self.clone())
    }
}

impl ReadFileTool {
    async fn read(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let path_arg = required_str(args, "path")?;
        let max_bytes = args
            .get("max_bytes")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_READ_MAX_BYTES as u64) as usize;
        let line_start = args.get("line_start").and_then(Value::as_u64);
        let line_end = args.get("line_end").and_then(Value::as_u64);
        validate_line_range(line_start, line_end)?;
        let path = self.scope.resolve_existing_or_new(path_arg)?;
        self.scope.check_read(&path)?;
        self.scope.note_read_target(&path);
        let content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        let lines = select_lines(&content, line_start, line_end);
        let byte_limited = limit_text(&lines.content, max_bytes);
        Ok(json!({
            "path": self.scope.relative_display(&path),
            "content": byte_limited.content,
            "line_start": lines.selected_start,
            "line_end": lines.selected_end,
            "total_lines": lines.total_lines,
            "truncated_by_lines": lines.truncated,
            "truncated_by_bytes": byte_limited.truncated
        }))
    }
}

#[derive(Debug, Clone)]
pub struct ListTreeTool {
    scope: ToolScope,
}

#[async_trait]
impl LlmTool for ListTreeTool {
    async fn run(
        &self,
        args: &HashMap<String, Value>,
        _ctx: &ToolRunCtx,
    ) -> mojentic::Result<Value> {
        self.scope.note_tool_call();
        let result = self.list(args).map_err(to_mojentic_error);
        let payload = match &result {
            Ok(value) => value.clone(),
            Err(error) => json!({ "error": error.to_string() }),
        };
        self.scope.trace_tool_event("tool.list_tree", payload);
        result
    }

    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            r#type: "function".to_string(),
            function: FunctionDescriptor {
                name: "list_tree".to_string(),
                description: "List files and directories under the active experiment root with depth and entry limits. Respects the workspace .gitignore.".to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "max_depth": { "type": "integer", "minimum": 0, "maximum": 12 },
                        "max_entries": { "type": "integer", "minimum": 1, "maximum": 2000 },
                        "include_hidden": { "type": "boolean" }
                    }
                }),
            },
        }
    }

    fn clone_box(&self) -> Box<dyn LlmTool> {
        Box::new(self.clone())
    }
}

impl ListTreeTool {
    fn list(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let root = self
            .scope
            .resolve_existing_dir(args.get("path").and_then(Value::as_str))?;
        if !self.scope.is_read_visible(&root) {
            self.scope.check_read(&root)?;
        }
        let max_depth = args
            .get("max_depth")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TREE_MAX_DEPTH as u64)
            .min(12) as usize;
        let max_entries = args
            .get("max_entries")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TREE_MAX_ENTRIES as u64)
            .min(2000) as usize;
        let include_hidden = args
            .get("include_hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let mut entries = Vec::new();
        let mut truncated = false;
        let gitignore = load_gitignore(&self.scope.root)?;
        let mut context = TreeCollectContext {
            scope: &self.scope,
            max_depth,
            max_entries,
            include_hidden,
            gitignore,
        };
        collect_tree(&mut context, &root, 0, &mut entries, &mut truncated)?;
        Ok(json!({
            "path": self.scope.relative_display(&root),
            "max_depth": max_depth,
            "max_entries": max_entries,
            "include_hidden": include_hidden,
            "entries": entries,
            "entry_count": entries.len(),
            "truncated": truncated
        }))
    }
}

struct TreeCollectContext<'a> {
    scope: &'a ToolScope,
    max_depth: usize,
    max_entries: usize,
    include_hidden: bool,
    gitignore: Gitignore,
}

fn collect_tree(
    context: &mut TreeCollectContext<'_>,
    dir: &Path,
    depth: usize,
    entries: &mut Vec<Value>,
    truncated: &mut bool,
) -> Result<()> {
    if depth > context.max_depth || entries.len() >= context.max_entries {
        *truncated = true;
        return Ok(());
    }
    let mut children = fs::read_dir(dir)
        .with_context(|| format!("reading directory {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("collecting directory {}", dir.display()))?;
    children.sort_by_key(|entry| entry.path());
    for child in children {
        if entries.len() >= context.max_entries {
            *truncated = true;
            return Ok(());
        }
        let name = child.file_name();
        let name = name.to_string_lossy();
        if !context.include_hidden && name.starts_with('.') {
            continue;
        }
        let path = child.path();
        let metadata = child
            .metadata()
            .with_context(|| format!("reading metadata {}", path.display()))?;
        if context
            .gitignore
            .matched(&path, metadata.is_dir())
            .is_ignore()
        {
            continue;
        }
        if !context.scope.is_read_visible(&path) {
            continue;
        }
        let kind = if metadata.is_dir() {
            "dir"
        } else if metadata.is_file() {
            "file"
        } else {
            "other"
        };
        entries.push(json!({
            "path": context.scope.relative_display(&path),
            "kind": kind,
            "depth": depth,
            "size_bytes": if metadata.is_file() { Some(metadata.len()) } else { None },
        }));
        if metadata.is_dir() && depth < context.max_depth {
            collect_tree(context, &path, depth + 1, entries, truncated)?;
        }
    }
    Ok(())
}

fn path_requires_validation_after_write(path: &str) -> bool {
    let path = Path::new(path);
    let file_name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if matches!(
        file_name.as_str(),
        ".gitignore"
            | ".ignore"
            | "readme"
            | "license"
            | "licence"
            | "changelog"
            | "contributors"
            | "authors"
    ) {
        return false;
    }
    if matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("md" | "markdown" | "txt" | "rst" | "adoc")
    ) {
        return false;
    }
    true
}

fn load_gitignore(root: &Path) -> Result<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);
    let gitignore_path = root.join(".gitignore");
    if gitignore_path.is_file()
        && let Some(error) = builder.add(&gitignore_path)
    {
        bail!("loading {}: {error}", gitignore_path.display());
    }
    builder
        .build()
        .with_context(|| format!("loading gitignore rules from {}", root.display()))
}

fn collect_shell_mutation_snapshot(
    scope: &ToolScope,
    dir: &Path,
    gitignore: &Gitignore,
    snapshot: &mut BTreeMap<String, FileFingerprint>,
) -> Result<()> {
    let mut children = fs::read_dir(dir)
        .with_context(|| format!("reading directory {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()
        .with_context(|| format!("collecting directory {}", dir.display()))?;
    children.sort_by_key(|entry| entry.path());
    for child in children {
        let path = child.path();
        let metadata = child
            .metadata()
            .with_context(|| format!("reading metadata {}", path.display()))?;
        let relative = scope.relative_path(&path)?;
        if is_shell_mutation_snapshot_excluded(&relative) {
            continue;
        }
        if gitignore.matched(&path, metadata.is_dir()).is_ignore() {
            continue;
        }
        if !scope.is_read_visible(&path) {
            continue;
        }
        if metadata.is_dir() {
            collect_shell_mutation_snapshot(scope, &path, gitignore, snapshot)?;
        } else if metadata.is_file() {
            let relative = scope.relative_display(&path);
            if path_requires_validation_after_write(&relative) {
                snapshot.insert(relative, file_fingerprint(&path, &metadata)?);
            }
        }
    }
    Ok(())
}

fn is_shell_mutation_snapshot_excluded(relative: &Path) -> bool {
    relative.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        matches!(
            name.to_string_lossy().as_ref(),
            ".git"
                | ".hg"
                | ".jj"
                | ".svn"
                | ".venv"
                | "venv"
                | "target"
                | "node_modules"
                | ".next"
                | ".pytest_cache"
                | ".mypy_cache"
                | "coverage"
                | "dist"
        )
    })
}

fn file_fingerprint(path: &Path, metadata: &fs::Metadata) -> Result<FileFingerprint> {
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    let content_hash = if metadata.len() <= MAX_SHELL_MUTATION_HASH_BYTES {
        let bytes = fs::read(path).with_context(|| format!("hashing {}", path.display()))?;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut hasher);
        Some(hasher.finish())
    } else {
        None
    };
    Ok(FileFingerprint {
        len: metadata.len(),
        modified_nanos,
        content_hash,
    })
}

fn changed_shell_mutation_paths(
    before: &BTreeMap<String, FileFingerprint>,
    after: &BTreeMap<String, FileFingerprint>,
) -> Vec<String> {
    let mut paths = before.keys().chain(after.keys()).collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
        .into_iter()
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect()
}

#[derive(Debug, Clone)]
pub struct WriteFileTool {
    scope: ToolScope,
}

#[async_trait]
impl LlmTool for WriteFileTool {
    async fn run(
        &self,
        args: &HashMap<String, Value>,
        _ctx: &ToolRunCtx,
    ) -> mojentic::Result<Value> {
        self.scope.note_tool_call();
        let result = self.write(args).await.map_err(to_mojentic_error);
        let payload = match &result {
            Ok(value) => value.clone(),
            Err(error) => json!({ "error": error.to_string() }),
        };
        self.scope.trace_tool_event("tool.write_file", payload);
        result
    }

    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            r#type: "function".to_string(),
            function: FunctionDescriptor {
                name: "write_file".to_string(),
                description: "Create a new UTF-8 file or replace an entire existing UTF-8 file under the active experiment root. For existing source repairs, prefer edit_file; write_file is whole-file replacement and must preserve unrelated content."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "content": { "type": "string" }
                    },
                    "required": ["path", "content"]
                }),
            },
        }
    }

    fn clone_box(&self) -> Box<dyn LlmTool> {
        Box::new(self.clone())
    }
}

impl WriteFileTool {
    async fn write(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let path_arg = required_str(args, "path")?;
        let content = required_str(args, "content")?;
        let path = self.scope.resolve_existing_or_new(path_arg)?;
        self.scope.check_write(&path)?;
        self.scope.note_write_intent(std::slice::from_ref(&path))?;
        self.scope
            .note_patch_fallback_choice(std::slice::from_ref(&path), "write_file", None)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        tokio::fs::write(&path, content)
            .await
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(json!({
            "path": self.scope.relative_display(&path),
            "bytes_written": content.len()
        }))
    }
}

#[derive(Debug, Clone)]
pub struct EditFileTool {
    scope: ToolScope,
}

#[async_trait]
impl LlmTool for EditFileTool {
    async fn run(
        &self,
        args: &HashMap<String, Value>,
        _ctx: &ToolRunCtx,
    ) -> mojentic::Result<Value> {
        self.scope.note_tool_call();
        let result = self.edit(args).await.map_err(to_mojentic_error);
        let payload = match &result {
            Ok(value) => value.clone(),
            Err(error) => json!({ "error": error.to_string() }),
        };
        self.scope.trace_tool_event("tool.edit_file", payload);
        result
    }

    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            r#type: "function".to_string(),
            function: FunctionDescriptor {
                name: "edit_file".to_string(),
                description: "Apply one or more structured text edits to an existing UTF-8 file under the active experiment root. Use replace_exact for a unique snippet, replace_lines/delete_lines for known line ranges, and insert_before/insert_after for a unique anchor."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "path": { "type": "string" },
                        "edits": {
                            "type": "array",
                            "minItems": 1,
                            "items": {
                                "type": "object",
                                "properties": {
                                    "kind": {
                                        "type": "string",
                                        "enum": [
                                            "replace_exact",
                                            "replace_lines",
                                            "insert_before",
                                            "insert_after",
                                            "delete_lines"
                                        ]
                                    },
                                    "old": {
                                        "type": "string",
                                        "description": "Existing unique snippet for replace_exact."
                                    },
                                    "new": {
                                        "type": "string",
                                        "description": "Replacement text for replace_exact or replace_lines."
                                    },
                                    "anchor": {
                                        "type": "string",
                                        "description": "Existing unique snippet for insert_before or insert_after."
                                    },
                                    "text": {
                                        "type": "string",
                                        "description": "Inserted text for insert_before or insert_after."
                                    },
                                    "start_line": { "type": "integer", "minimum": 1 },
                                    "end_line": { "type": "integer", "minimum": 1 },
                                    "expected": {
                                        "type": "string",
                                        "description": "Optional context that must appear in the selected line range."
                                    }
                                },
                                "required": ["kind"]
                            }
                        }
                    },
                    "required": ["path", "edits"]
                }),
            },
        }
    }

    fn clone_box(&self) -> Box<dyn LlmTool> {
        Box::new(self.clone())
    }
}

impl EditFileTool {
    async fn edit(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let path_arg = required_str(args, "path")?;
        let edits = required_array(args, "edits")?;
        let path = self.scope.resolve_existing_or_new(path_arg)?;
        self.scope.check_write(&path)?;
        if !path.is_file() {
            bail!(
                "edit_file can only edit existing files; use write_file to create {}",
                self.scope.relative_display(&path)
            );
        }

        let mut content = tokio::fs::read_to_string(&path)
            .await
            .with_context(|| format!("reading {}", path.display()))?;
        let bytes_before = content.len();
        let mut applied = Vec::new();

        for (index, edit) in edits.iter().enumerate() {
            let object = edit
                .as_object()
                .with_context(|| format!("edits[{index}] must be an object"))?;
            let kind = required_edit_str(object, index, "kind")?;
            let before_edit_len = content.len();
            let summary = match kind {
                "replace_exact" => {
                    let old = required_edit_str(object, index, "old")?;
                    if old.is_empty() {
                        bail!("edits[{index}].old must not be empty");
                    }
                    let new = required_edit_str(object, index, "new")?;
                    let matches = content.match_indices(old).count();
                    match matches {
                        0 => bail!("edits[{index}] replace_exact found no match"),
                        1 => {
                            content = content.replacen(old, new, 1);
                        }
                        count => bail!(
                            "edits[{index}] replace_exact is ambiguous: matched {count} times"
                        ),
                    }
                    json!({
                        "index": index,
                        "kind": kind,
                        "match_count": matches,
                        "old_bytes": old.len(),
                        "new_bytes": new.len(),
                        "bytes_delta": content.len() as isize - before_edit_len as isize,
                    })
                }
                "insert_before" | "insert_after" => {
                    let anchor = required_edit_str(object, index, "anchor")?;
                    if anchor.is_empty() {
                        bail!("edits[{index}].anchor must not be empty");
                    }
                    let text = required_edit_str(object, index, "text")?;
                    let matches = content.match_indices(anchor).count();
                    match matches {
                        0 => bail!("edits[{index}] {kind} found no anchor match"),
                        1 => {
                            let offset = content
                                .find(anchor)
                                .expect("exactly one match implies find succeeds");
                            let insert_at = if kind == "insert_before" {
                                offset
                            } else {
                                offset + anchor.len()
                            };
                            content.insert_str(insert_at, text);
                        }
                        count => bail!("edits[{index}] {kind} is ambiguous: matched {count} times"),
                    }
                    json!({
                        "index": index,
                        "kind": kind,
                        "match_count": matches,
                        "anchor_bytes": anchor.len(),
                        "inserted_bytes": text.len(),
                        "bytes_delta": content.len() as isize - before_edit_len as isize,
                    })
                }
                "replace_lines" => {
                    let new = required_edit_str(object, index, "new")?;
                    let start_line = required_edit_u64(object, index, "start_line")?;
                    let end_line = required_edit_u64(object, index, "end_line")?;
                    let (start_index, end_index, selected) =
                        selected_line_range(&content, start_line, end_line, index)?;
                    verify_expected_context(object, index, &selected)?;
                    let mut lines = split_lines_preserving_newlines(&content);
                    lines.splice(start_index..end_index, [new.to_string()]);
                    content = lines.concat();
                    json!({
                        "index": index,
                        "kind": kind,
                        "start_line": start_line,
                        "end_line": end_line,
                        "old_bytes": selected.len(),
                        "new_bytes": new.len(),
                        "bytes_delta": content.len() as isize - before_edit_len as isize,
                    })
                }
                "delete_lines" => {
                    let start_line = required_edit_u64(object, index, "start_line")?;
                    let end_line = required_edit_u64(object, index, "end_line")?;
                    let (start_index, end_index, selected) =
                        selected_line_range(&content, start_line, end_line, index)?;
                    verify_expected_context(object, index, &selected)?;
                    let mut lines = split_lines_preserving_newlines(&content);
                    lines.drain(start_index..end_index);
                    content = lines.concat();
                    json!({
                        "index": index,
                        "kind": kind,
                        "start_line": start_line,
                        "end_line": end_line,
                        "deleted_bytes": selected.len(),
                        "bytes_delta": content.len() as isize - before_edit_len as isize,
                    })
                }
                other => bail!("edits[{index}].kind is unsupported: {other}"),
            };
            applied.push(summary);
        }

        self.scope.note_write_intent(std::slice::from_ref(&path))?;
        tokio::fs::write(&path, content.as_bytes())
            .await
            .with_context(|| format!("writing {}", path.display()))?;
        Ok(json!({
            "path": self.scope.relative_display(&path),
            "edit_count": applied.len(),
            "bytes_before": bytes_before,
            "bytes_after": content.len(),
            "edits_applied": applied,
        }))
    }
}

#[derive(Debug, Clone)]
pub struct PatchFileTool {
    scope: ToolScope,
}

#[async_trait]
impl LlmTool for PatchFileTool {
    async fn run(
        &self,
        args: &HashMap<String, Value>,
        _ctx: &ToolRunCtx,
    ) -> mojentic::Result<Value> {
        self.scope.note_tool_call();
        let result = self.patch(args).await.map_err(to_mojentic_error);
        let payload = match &result {
            Ok(value) => value.clone(),
            Err(error) => json!({ "error": error.to_string() }),
        };
        self.scope.trace_tool_event("tool.patch_file", payload);
        result
    }

    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            r#type: "function".to_string(),
            function: FunctionDescriptor {
                name: "patch_file".to_string(),
                description: "Apply a unified diff under the active experiment root using patch."
                    .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "patch": { "type": "string" }
                    },
                    "required": ["patch"]
                }),
            },
        }
    }

    fn clone_box(&self) -> Box<dyn LlmTool> {
        Box::new(self.clone())
    }
}

impl PatchFileTool {
    async fn patch(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let patch = required_str(args, "patch")?;
        let touched_paths = patch_paths(&self.scope, patch)?;
        validate_patch_paths(&self.scope, patch)?;
        self.scope.note_write_intent(&touched_paths)?;
        self.scope.note_patch_fallback_choice(
            &touched_paths,
            "patch_file_retry",
            Some(patch.len()),
        )?;
        let strip_level = patch_strip_level(patch);
        let artifacts = patch_artifact_paths(&self.scope, patch)?;
        let pre_existing_artifacts = existing_paths(&artifacts);

        let dry_run = match run_patch_command(&self.scope.root, patch, strip_level, true).await {
            Ok(output) => output,
            Err(error) => {
                let reason = error.to_string();
                let fallbacks = self.scope.note_patch_failure(&touched_paths, &reason)?;
                bail!(
                    "{reason}; patch fallback recommended: {}",
                    fallback_text(&fallbacks)
                );
            }
        };
        if !dry_run.status.success() {
            let reason = format!(
                "patch dry-run failed with status {}: {}{}",
                dry_run.status,
                String::from_utf8_lossy(&dry_run.stderr),
                String::from_utf8_lossy(&dry_run.stdout)
            );
            let fallbacks = self.scope.note_patch_failure(&touched_paths, &reason)?;
            bail!(
                "{reason}; patch fallback recommended: {}",
                fallback_text(&fallbacks)
            );
        }

        let output = match run_patch_command(&self.scope.root, patch, strip_level, false).await {
            Ok(output) => output,
            Err(error) => {
                cleanup_new_patch_artifacts(&artifacts, &pre_existing_artifacts)?;
                let reason = error.to_string();
                let fallbacks = self.scope.note_patch_failure(&touched_paths, &reason)?;
                bail!(
                    "{reason}; patch fallback recommended: {}",
                    fallback_text(&fallbacks)
                );
            }
        };
        if !output.status.success() {
            cleanup_new_patch_artifacts(&artifacts, &pre_existing_artifacts)?;
            let reason = format!(
                "patch failed with status {}: {}{}",
                output.status,
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            );
            let fallbacks = self.scope.note_patch_failure(&touched_paths, &reason)?;
            bail!(
                "{reason}; patch fallback recommended: {}",
                fallback_text(&fallbacks)
            );
        }
        cleanup_new_patch_artifacts(&artifacts, &pre_existing_artifacts)?;
        Ok(json!({
            "strip_level": strip_level,
            "status": output.status.code(),
            "stdout": String::from_utf8_lossy(&output.stdout),
            "stderr": String::from_utf8_lossy(&output.stderr)
        }))
    }
}

async fn run_patch_command(
    root: &Path,
    patch: &str,
    strip_level: u8,
    dry_run: bool,
) -> Result<std::process::Output> {
    let mut command = Command::new("patch");
    command
        .arg(format!("-p{strip_level}"))
        .arg("-N")
        .current_dir(root)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    if dry_run {
        command.arg("--dry-run");
    }
    let mut child = command.spawn().context("spawning patch")?;
    let mut stdin = child.stdin.take().context("opening patch stdin")?;
    stdin.write_all(patch.as_bytes()).await?;
    drop(stdin);
    timeout(
        Duration::from_secs(PATCH_TIMEOUT_SECS),
        child.wait_with_output(),
    )
    .await
    .with_context(|| format!("patch timed out after {PATCH_TIMEOUT_SECS}s"))?
    .context("running patch")
}

#[derive(Debug, Clone)]
pub struct ShellCommandTool {
    scope: ToolScope,
}

#[async_trait]
impl LlmTool for ShellCommandTool {
    async fn run(
        &self,
        args: &HashMap<String, Value>,
        _ctx: &ToolRunCtx,
    ) -> mojentic::Result<Value> {
        self.scope.note_tool_call();
        let result = self.shell(args).await.map_err(to_mojentic_error);
        let payload = match &result {
            Ok(value) => value.clone(),
            Err(error) => json!({ "error": error.to_string() }),
        };
        self.scope.trace_tool_event("tool.shell_command", payload);
        result
    }

    fn descriptor(&self) -> ToolDescriptor {
        ToolDescriptor {
            r#type: "function".to_string(),
            function: FunctionDescriptor {
                name: "shell_command".to_string(),
                description:
                    "Run a shell command from the writable workspace by default, or another scoped child directory."
                        .to_string(),
                parameters: json!({
                    "type": "object",
                    "properties": {
                        "command": { "type": "string" },
                        "cwd": { "type": "string" },
                        "timeout_secs": { "type": "integer", "minimum": 1, "maximum": 1800 }
                    },
                    "required": ["command"]
                }),
            },
        }
    }

    fn clone_box(&self) -> Box<dyn LlmTool> {
        Box::new(self.clone())
    }
}

impl ShellCommandTool {
    async fn shell(&self, args: &HashMap<String, Value>) -> Result<Value> {
        let command = required_str(args, "command")?;
        validate_shell_command(command, &self.scope.root)?;
        let cwd = self
            .scope
            .resolve_shell_cwd(args.get("cwd").and_then(Value::as_str))?;
        self.scope.check_write(&cwd)?;
        let timeout_secs = args
            .get("timeout_secs")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_SHELL_TIMEOUT_SECS)
            .min(MAX_SHELL_TIMEOUT_SECS);
        let validation_probe = is_validation_probe(command);
        let before_mutation_snapshot = match self.scope.shell_mutation_snapshot() {
            Ok(snapshot) => Some(snapshot),
            Err(error) => {
                let _ = self.scope.trace.event(
                    "agent.shell.mutation_snapshot_failed",
                    json!({
                        "command": command,
                        "cwd": self.scope.relative_display(&cwd),
                        "phase": "before",
                        "error": error.to_string(),
                    }),
                );
                None
            }
        };
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let shell_command = format!("set -o pipefail; {command}");
        let output = timeout(
            Duration::from_secs(timeout_secs),
            Command::new(shell)
                .arg("-lc")
                .arg(&shell_command)
                .current_dir(&cwd)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output(),
        )
        .await
        .with_context(|| format!("command timed out after {timeout_secs}s"))?
        .with_context(|| format!("running command {command:?}"))?;
        let stdout = capture_output(&output.stdout);
        let stderr = capture_output(&output.stderr);
        let (shell_mutation_paths, shell_mutation_snapshot_error) =
            if let Some(before_snapshot) = before_mutation_snapshot {
                match self.scope.shell_mutation_snapshot() {
                    Ok(after_snapshot) => {
                        let paths = changed_shell_mutation_paths(&before_snapshot, &after_snapshot);
                        (paths, None)
                    }
                    Err(error) => {
                        let error = error.to_string();
                        let _ = self.scope.trace.event(
                            "agent.shell.mutation_snapshot_failed",
                            json!({
                                "command": command,
                                "cwd": self.scope.relative_display(&cwd),
                                "phase": "after",
                                "error": error.clone(),
                            }),
                        );
                        (Vec::new(), Some(error))
                    }
                }
            } else {
                (Vec::new(), None)
            };
        let shell_mutation_sensed = !shell_mutation_paths.is_empty();
        let repair_required = if validation_probe {
            self.scope
                .note_validation_probe_result(command, &output, &stdout, &stderr)?
        } else {
            None
        };
        self.scope.note_sensed_shell_mutation(&shell_mutation_paths);
        if shell_mutation_sensed {
            let _ = self.scope.trace.event(
                "agent.shell.mutation_sensed",
                json!({
                    "command": command,
                    "cwd": self.scope.relative_display(&cwd),
                    "paths": shell_mutation_paths.clone(),
                    "validation_required_after_write": true,
                }),
            );
        }
        Ok(json!({
            "cwd": self.scope.relative_display(&cwd),
            "command": command,
            "validation_probe": validation_probe,
            "status": output.status.code(),
            "success": output.status.success(),
            "stdout": stdout.content,
            "stdout_truncated": stdout.truncated,
            "stderr": stderr.content,
            "stderr_truncated": stderr.truncated,
            "shell_mutation_sensed": shell_mutation_sensed,
            "shell_mutation_paths": shell_mutation_paths,
            "shell_mutation_requires_validation": shell_mutation_sensed,
            "shell_mutation_snapshot_error": shell_mutation_snapshot_error,
            "repair_required": repair_required
        }))
    }
}

fn required_str<'a>(args: &'a HashMap<String, Value>, key: &str) -> Result<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .with_context(|| format!("missing required string argument {key:?}"))
}

fn required_array<'a>(args: &'a HashMap<String, Value>, key: &str) -> Result<&'a Vec<Value>> {
    args.get(key)
        .and_then(Value::as_array)
        .filter(|value| !value.is_empty())
        .with_context(|| format!("missing required non-empty array argument {key:?}"))
}

fn required_edit_str<'a>(
    edit: &'a serde_json::Map<String, Value>,
    index: usize,
    key: &str,
) -> Result<&'a str> {
    edit.get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("missing required string argument edits[{index}].{key}"))
}

fn required_edit_u64(
    edit: &serde_json::Map<String, Value>,
    index: usize,
    key: &str,
) -> Result<u64> {
    edit.get(key)
        .and_then(Value::as_u64)
        .filter(|value| *value >= 1)
        .with_context(|| format!("missing required positive integer argument edits[{index}].{key}"))
}

fn split_lines_preserving_newlines(content: &str) -> Vec<String> {
    if content.is_empty() {
        Vec::new()
    } else {
        content.split_inclusive('\n').map(str::to_string).collect()
    }
}

fn selected_line_range(
    content: &str,
    start_line: u64,
    end_line: u64,
    edit_index: usize,
) -> Result<(usize, usize, String)> {
    if end_line < start_line {
        bail!("edits[{edit_index}].end_line must be greater than or equal to start_line");
    }
    let lines = split_lines_preserving_newlines(content);
    let total_lines = lines.len() as u64;
    if start_line > total_lines || end_line > total_lines {
        bail!(
            "edits[{edit_index}] line range {start_line}-{end_line} exceeds file length {total_lines}"
        );
    }
    let start_index = (start_line - 1) as usize;
    let end_index = end_line as usize;
    let selected = lines[start_index..end_index].concat();
    Ok((start_index, end_index, selected))
}

fn verify_expected_context(
    edit: &serde_json::Map<String, Value>,
    index: usize,
    selected: &str,
) -> Result<()> {
    let Some(expected) = edit.get("expected").and_then(Value::as_str) else {
        return Ok(());
    };
    if expected.is_empty() {
        bail!("edits[{index}].expected must not be empty when provided");
    }
    if !selected.contains(expected) {
        bail!("edits[{index}].expected was not found in the selected line range");
    }
    Ok(())
}

fn validate_line_range(line_start: Option<u64>, line_end: Option<u64>) -> Result<()> {
    if let (Some(start), Some(end)) = (line_start, line_end)
        && end < start
    {
        bail!("line_end must be greater than or equal to line_start");
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct SelectedLines {
    content: String,
    selected_start: usize,
    selected_end: usize,
    total_lines: usize,
    truncated: bool,
}

fn select_lines(content: &str, line_start: Option<u64>, line_end: Option<u64>) -> SelectedLines {
    let lines = content.lines().collect::<Vec<_>>();
    let total_lines = lines.len();
    if total_lines == 0 {
        return SelectedLines {
            content: String::new(),
            selected_start: 0,
            selected_end: 0,
            total_lines: 0,
            truncated: false,
        };
    }
    let start = line_start.unwrap_or(1).max(1) as usize;
    let end = line_end.unwrap_or(total_lines as u64).max(1) as usize;
    let clamped_start = start.min(total_lines);
    let clamped_end = end.min(total_lines);
    let selected = if clamped_start <= clamped_end {
        lines[(clamped_start - 1)..clamped_end].join("\n")
    } else {
        String::new()
    };
    SelectedLines {
        content: selected,
        selected_start: clamped_start,
        selected_end: clamped_end,
        total_lines,
        truncated: start > 1 || end < total_lines,
    }
}

#[derive(Debug, PartialEq, Eq)]
struct LimitedText {
    content: String,
    truncated: bool,
}

fn limit_text(content: &str, max_bytes: usize) -> LimitedText {
    if content.len() <= max_bytes {
        return LimitedText {
            content: content.to_string(),
            truncated: false,
        };
    }
    LimitedText {
        content: content.chars().take(max_bytes).collect(),
        truncated: true,
    }
}

fn validate_patch_paths(scope: &ToolScope, patch: &str) -> Result<()> {
    for path in patch_paths(scope, patch)? {
        scope.check_write(&path)?;
    }
    Ok(())
}

fn patch_paths(scope: &ToolScope, patch: &str) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for line in patch.lines() {
        let Some(raw) = line
            .strip_prefix("--- ")
            .or_else(|| line.strip_prefix("+++ "))
        else {
            continue;
        };
        let token = raw.split_whitespace().next().unwrap_or_default();
        if token == "/dev/null" {
            continue;
        }
        let relative = token
            .strip_prefix("a/")
            .or_else(|| token.strip_prefix("b/"))
            .unwrap_or(token);
        let path = scope.resolve_existing_or_new(relative)?;
        if !paths.iter().any(|existing| existing == &path) {
            paths.push(path);
        }
    }
    Ok(paths)
}

fn patch_artifact_paths(scope: &ToolScope, patch: &str) -> Result<Vec<PathBuf>> {
    let mut artifacts = Vec::new();
    for path in patch_paths(scope, patch)? {
        for suffix in ["orig", "rej"] {
            let artifact = sibling_with_suffix(&path, suffix);
            if !artifacts.iter().any(|existing| existing == &artifact) {
                artifacts.push(artifact);
            }
        }
    }
    Ok(artifacts)
}

fn sibling_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{file_name}.{suffix}"))
}

fn existing_paths(paths: &[PathBuf]) -> HashSet<PathBuf> {
    paths.iter().filter(|path| path.exists()).cloned().collect()
}

fn cleanup_new_patch_artifacts(
    artifacts: &[PathBuf],
    pre_existing_artifacts: &HashSet<PathBuf>,
) -> Result<()> {
    for artifact in artifacts {
        if artifact.exists() && !pre_existing_artifacts.contains(artifact) {
            fs::remove_file(artifact)
                .with_context(|| format!("removing patch artifact {}", artifact.display()))?;
        }
    }
    Ok(())
}

fn patch_strip_level(patch: &str) -> u8 {
    for line in patch.lines() {
        if let Some(raw) = line
            .strip_prefix("--- ")
            .or_else(|| line.strip_prefix("+++ "))
        {
            let token = raw.split_whitespace().next().unwrap_or_default();
            if token.starts_with("a/") || token.starts_with("b/") {
                return 1;
            }
        }
        if let Some(raw) = line.strip_prefix("diff --git ")
            && raw.split_whitespace().any(|token| {
                token.starts_with("a/")
                    || token.starts_with("b/")
                    || token.starts_with("\"a/")
                    || token.starts_with("\"b/")
            })
        {
            return 1;
        }
    }
    0
}

fn validate_shell_command(command: &str, root: &Path) -> Result<()> {
    for token in command.split_whitespace() {
        let cleaned = token.trim_matches(|ch| {
            matches!(
                ch,
                '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}' | ';' | ','
            )
        });
        if cleaned.starts_with('/') {
            let path = Path::new(cleaned);
            let Ok(canonical) = path.canonicalize() else {
                bail!("absolute path does not exist inside the tool scope: {cleaned}");
            };
            if !canonical.starts_with(root) {
                bail!("shell commands must not reference paths outside the tool scope: {cleaned}");
            }
        }
        if cleaned == ".." || cleaned.starts_with("../") || cleaned.contains("/../") {
            bail!("shell commands must not reference parent paths: {cleaned}");
        }
    }
    Ok(())
}

fn is_validation_probe(command: &str) -> bool {
    let lowered = command.to_ascii_lowercase();
    let trimmed = lowered.trim();
    if trimmed.starts_with("echo ")
        || trimmed == "pwd"
        || trimmed.starts_with("pwd ")
        || trimmed.starts_with("ls ")
        || trimmed == "ls"
        || trimmed.starts_with("cat ")
        || trimmed.starts_with("sed ")
        || trimmed.starts_with("rg ")
        || trimmed.starts_with("grep ")
        || trimmed.starts_with("find ")
        || trimmed.starts_with("head ")
        || trimmed.starts_with("tail ")
    {
        return false;
    }

    [
        "cargo build",
        "cargo check",
        "cargo test",
        "cargo run",
        "cargo clippy",
        "cargo fmt",
        "npm test",
        "npm run",
        "pnpm test",
        "pnpm run",
        "yarn test",
        "yarn run",
        "pytest",
        "python -m pytest",
        "go test",
        "make test",
        "make check",
        "make build",
        "mvn test",
        "gradle test",
        "./gradlew test",
    ]
    .iter()
    .any(|needle| trimmed.contains(needle))
}

fn command_family(command: &str) -> String {
    let lowered = command.to_ascii_lowercase();
    let trimmed = lowered.trim();
    for family in [
        "cargo fmt",
        "cargo clippy",
        "cargo test",
        "cargo build",
        "cargo check",
        "cargo run",
        "npm test",
        "npm run",
        "pnpm test",
        "pnpm run",
        "yarn test",
        "yarn run",
        "python -m pytest",
        "pytest",
        "go test",
        "make test",
        "make check",
        "make build",
        "mvn test",
        "gradle test",
        "./gradlew test",
    ] {
        if trimmed.contains(family) {
            return family.to_string();
        }
    }
    trimmed
        .split_whitespace()
        .next()
        .unwrap_or("unknown")
        .to_string()
}

fn failure_summary(stderr: &str, stdout: &str) -> String {
    let source = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    source
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| limit_text(line, 240).content)
        .unwrap_or_else(|| "validation command failed without output".to_string())
}

fn failure_details(stderr: &str, stdout: &str) -> Vec<String> {
    let combined = [stdout, stderr]
        .into_iter()
        .filter(|source| !source.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    let lines = combined.lines().map(str::trim).collect::<Vec<_>>();
    let mut details = Vec::new();

    for line in &lines {
        if line.starts_with("---- ") && line.ends_with(" stdout ----") {
            push_unique_detail(
                &mut details,
                line.trim_start_matches("---- ")
                    .trim_end_matches(" stdout ----")
                    .trim(),
            );
            continue;
        }
        if line.starts_with("thread '") && line.contains(" panicked at ") {
            push_unique_detail(&mut details, line);
            continue;
        }
        if line.starts_with("error[") || line.starts_with("error:") || line.starts_with("FAILED") {
            push_unique_detail(&mut details, line);
        }
    }

    if let Some(failures_index) = lines.iter().position(|line| *line == "failures:") {
        for line in lines.iter().skip(failures_index + 1) {
            if line.is_empty() {
                continue;
            }
            if line.starts_with("test result:") {
                break;
            }
            if !line.contains(':') && !line.starts_with('-') {
                push_unique_detail(&mut details, line);
            }
        }
    }

    details
        .into_iter()
        .take(8)
        .map(|detail| limit_text(&detail, 240).content)
        .collect()
}

fn push_unique_detail(details: &mut Vec<String>, detail: &str) {
    let trimmed = detail.trim();
    if !trimmed.is_empty() && !details.iter().any(|existing| existing == trimmed) {
        details.push(trimmed.to_string());
    }
}

fn fallback_text(fallbacks: &[PatchFallbackSnapshot]) -> String {
    fallbacks
        .iter()
        .map(|fallback| {
            format!(
                "{} attempt {}: {}",
                fallback.path, fallback.attempts, fallback.guidance
            )
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn parse_path_rules(values: Vec<String>) -> Result<Vec<PathRule>> {
    values
        .into_iter()
        .map(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                bail!("packet scope path must not be empty");
            }
            let (raw_path, recursive) = trimmed
                .strip_suffix("/**")
                .map(|path| (path, true))
                .unwrap_or((trimmed, false));
            let path = Path::new(raw_path);
            if path.is_absolute() {
                bail!("packet scope path must be relative: {trimmed}");
            }
            for component in path.components() {
                match component {
                    Component::Normal(_) | Component::CurDir => {}
                    _ => bail!("packet scope path escapes root: {trimmed}"),
                }
            }
            Ok(PathRule {
                path: if raw_path == "." {
                    PathBuf::from(".")
                } else {
                    PathBuf::from(raw_path)
                },
                recursive,
            })
        })
        .collect()
}

impl PathRule {
    fn matches(&self, path: &Path) -> bool {
        if self.recursive {
            path == self.path || path.starts_with(&self.path)
        } else {
            path == self.path
        }
    }

    fn is_descendant_of(&self, path: &Path) -> bool {
        self.path.starts_with(path)
    }
}

#[derive(Debug, PartialEq, Eq)]
struct CapturedOutput {
    content: String,
    truncated: bool,
}

fn capture_output(bytes: &[u8]) -> CapturedOutput {
    let content = String::from_utf8_lossy(bytes);
    if content.len() <= MAX_CAPTURED_OUTPUT_BYTES {
        return CapturedOutput {
            content: content.to_string(),
            truncated: false,
        };
    }
    CapturedOutput {
        content: content.chars().take(MAX_CAPTURED_OUTPUT_BYTES).collect(),
        truncated: true,
    }
}

fn estimate_tokens(chars: usize) -> usize {
    chars.div_ceil(APPROX_CHARS_PER_TOKEN)
}

fn to_mojentic_error(error: anyhow::Error) -> mojentic::MojenticError {
    mojentic::MojenticError::ToolError(error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(temp: &tempfile::TempDir) -> ToolScope {
        let trace = Arc::new(TraceRecorder::create(&temp.path().join("traces")).unwrap());
        ToolScope::new(temp.path().to_path_buf(), trace).unwrap()
    }

    fn restricted_scope(temp: &tempfile::TempDir) -> ToolScope {
        let trace = Arc::new(TraceRecorder::create(&temp.path().join("traces")).unwrap());
        ToolScope::new_restricted(
            temp.path().to_path_buf(),
            trace,
            vec![
                "task-model-first.md".to_string(),
                "workspace/**".to_string(),
            ],
            vec!["workspace/**".to_string()],
        )
        .unwrap()
    }

    #[test]
    fn rejects_parent_paths() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        assert!(scope.resolve_existing_or_new("../outside").is_err());
    }

    #[test]
    fn rejects_absolute_paths() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        assert!(scope.resolve_existing_or_new("/tmp/outside").is_err());
    }

    #[test]
    fn validates_patch_paths() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        let patch =
            "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n+++ b/src/main.rs\n";
        assert!(validate_patch_paths(&scope, patch).is_ok());
    }

    #[test]
    fn chooses_patch_strip_level_from_headers() {
        assert_eq!(
            patch_strip_level(
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n"
            ),
            1
        );
        assert_eq!(
            patch_strip_level("--- workspace/src/lib.rs\n+++ workspace/src/lib.rs\n"),
            0
        );
    }

    #[tokio::test]
    async fn patch_tool_uses_system_patch_without_git_repo() {
        let temp = tempfile::tempdir().unwrap();
        let scope = restricted_scope(&temp);
        std::fs::create_dir_all(temp.path().join("workspace/src")).unwrap();
        std::fs::write(temp.path().join("workspace/src/lib.rs"), "old\n").unwrap();

        let tool = PatchFileTool { scope };
        let mut args = HashMap::new();
        args.insert(
            "patch".to_string(),
            json!("--- workspace/src/lib.rs\n+++ workspace/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n"),
        );
        let result = tool.patch(&args).await.unwrap();

        assert_eq!(result["strip_level"], 0);
        assert_eq!(
            std::fs::read_to_string(temp.path().join("workspace/src/lib.rs")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn patch_cleanup_removes_only_new_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let new_orig = temp.path().join("workspace/src/main.rs.orig");
        let existing_rej = temp.path().join("workspace/src/main.rs.rej");
        std::fs::create_dir_all(existing_rej.parent().unwrap()).unwrap();
        std::fs::write(&new_orig, "temporary backup\n").unwrap();
        std::fs::write(&existing_rej, "pre-existing reject\n").unwrap();

        let artifacts = vec![new_orig.clone(), existing_rej.clone()];
        let pre_existing_artifacts = HashSet::from([existing_rej.clone()]);
        cleanup_new_patch_artifacts(&artifacts, &pre_existing_artifacts).unwrap();

        assert!(!new_orig.exists());
        assert!(existing_rej.exists());
    }

    #[tokio::test]
    async fn successful_patch_does_not_leave_patch_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let scope = restricted_scope(&temp);
        std::fs::create_dir_all(temp.path().join("workspace/src")).unwrap();
        let file = temp.path().join("workspace/src/main.rs");
        std::fs::write(&file, "old\n").unwrap();

        let tool = PatchFileTool { scope };
        let mut args = HashMap::new();
        args.insert(
            "patch".to_string(),
            json!(
                "--- workspace/src/main.rs\n+++ workspace/src/main.rs\n@@ -1 +1 @@\n-old\n+new\n"
            ),
        );

        tool.patch(&args).await.unwrap();

        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new\n");
        assert!(!temp.path().join("workspace/src/main.rs.orig").exists());
        assert!(!temp.path().join("workspace/src/main.rs.rej").exists());
    }

    #[tokio::test]
    async fn patch_tool_dry_run_prevents_reject_artifacts() {
        let temp = tempfile::tempdir().unwrap();
        let scope = restricted_scope(&temp);
        std::fs::create_dir_all(temp.path().join("workspace/src")).unwrap();
        let file = temp.path().join("workspace/src/main.rs");
        std::fs::write(&file, "actual\n").unwrap();

        let tool = PatchFileTool { scope };
        let mut args = HashMap::new();
        args.insert(
            "patch".to_string(),
            json!(
                "--- workspace/src/main.rs\n+++ workspace/src/main.rs\n@@ -1 +1 @@\n-expected\n+changed\n"
            ),
        );

        assert!(tool.patch(&args).await.is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "actual\n");
        assert!(!temp.path().join("workspace/src/main.rs.orig").exists());
        assert!(!temp.path().join("workspace/src/main.rs.rej").exists());
    }

    #[tokio::test]
    async fn patch_dry_run_failure_records_fallback_guidance() {
        let temp = tempfile::tempdir().unwrap();
        let scope = restricted_scope(&temp);
        std::fs::create_dir_all(temp.path().join("workspace/src")).unwrap();
        let file = temp.path().join("workspace/src/main.rs");
        std::fs::write(&file, "actual\n").unwrap();

        let tool = PatchFileTool {
            scope: scope.clone(),
        };
        let mut args = HashMap::new();
        args.insert(
            "patch".to_string(),
            json!(
                "--- workspace/src/main.rs\n+++ workspace/src/main.rs\n@@ -1 +1 @@\n-expected\n+changed\n"
            ),
        );

        let error = tool.patch(&args).await.unwrap_err().to_string();
        let snapshot = scope.policy_snapshot();

        assert!(error.contains("patch fallback recommended"));
        assert_eq!(snapshot.patch_fallbacks.len(), 1);
        assert_eq!(snapshot.patch_fallbacks[0].path, "workspace/src/main.rs");
        assert_eq!(snapshot.patch_fallbacks[0].attempts, 1);
        assert!(snapshot.patch_fallbacks[0].reason.contains("dry-run"));
        assert!(
            snapshot.patch_fallbacks[0]
                .guidance
                .contains("write_file replaces the entire file")
        );
        assert!(
            !snapshot.patch_fallbacks[0]
                .guidance
                .contains("bounded write_file")
        );
    }

    #[tokio::test]
    async fn edit_file_replace_exact_updates_unique_snippet() {
        let temp = tempfile::tempdir().unwrap();
        let scope = restricted_scope(&temp);
        std::fs::create_dir_all(temp.path().join("workspace/src")).unwrap();
        let file = temp.path().join("workspace/src/lib.rs");
        std::fs::write(&file, "pub fn value() -> i32 {\n    1\n}\n").unwrap();

        let tool = EditFileTool {
            scope: scope.clone(),
        };
        let result = tool
            .edit(&HashMap::from([
                ("path".to_string(), json!("workspace/src/lib.rs")),
                (
                    "edits".to_string(),
                    json!([
                        {
                            "kind": "replace_exact",
                            "old": "    1\n",
                            "new": "    2\n"
                        }
                    ]),
                ),
            ]))
            .await
            .unwrap();

        assert_eq!(result["edit_count"], 1);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "pub fn value() -> i32 {\n    2\n}\n"
        );
        assert_eq!(scope.policy_snapshot().writes_since_shell_probe, 1);
    }

    #[tokio::test]
    async fn edit_file_rejects_ambiguous_exact_match_without_writing() {
        let temp = tempfile::tempdir().unwrap();
        let scope = restricted_scope(&temp);
        std::fs::create_dir_all(temp.path().join("workspace/src")).unwrap();
        let file = temp.path().join("workspace/src/lib.rs");
        std::fs::write(&file, "same\nsame\n").unwrap();

        let tool = EditFileTool {
            scope: scope.clone(),
        };
        let error = tool
            .edit(&HashMap::from([
                ("path".to_string(), json!("workspace/src/lib.rs")),
                (
                    "edits".to_string(),
                    json!([
                        {
                            "kind": "replace_exact",
                            "old": "same",
                            "new": "changed"
                        }
                    ]),
                ),
            ]))
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("ambiguous"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "same\nsame\n");
        assert_eq!(scope.policy_snapshot().writes_since_shell_probe, 0);
    }

    #[tokio::test]
    async fn edit_file_replace_lines_checks_expected_context() {
        let temp = tempfile::tempdir().unwrap();
        let scope = restricted_scope(&temp);
        std::fs::create_dir_all(temp.path().join("workspace/src")).unwrap();
        let file = temp.path().join("workspace/src/lib.rs");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

        let tool = EditFileTool { scope };
        let error = tool
            .edit(&HashMap::from([
                ("path".to_string(), json!("workspace/src/lib.rs")),
                (
                    "edits".to_string(),
                    json!([
                        {
                            "kind": "replace_lines",
                            "start_line": 2,
                            "end_line": 2,
                            "expected": "missing",
                            "new": "TWO\n"
                        }
                    ]),
                ),
            ]))
            .await
            .unwrap_err()
            .to_string();

        assert!(error.contains("expected was not found"));
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "one\ntwo\nthree\n");
    }

    #[tokio::test]
    async fn edit_file_supports_line_and_anchor_operations() {
        let temp = tempfile::tempdir().unwrap();
        let scope = restricted_scope(&temp);
        std::fs::create_dir_all(temp.path().join("workspace/src")).unwrap();
        let file = temp.path().join("workspace/src/lib.rs");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();

        let tool = EditFileTool { scope };
        tool.edit(&HashMap::from([
            ("path".to_string(), json!("workspace/src/lib.rs")),
            (
                "edits".to_string(),
                json!([
                    {
                        "kind": "replace_lines",
                        "start_line": 2,
                        "end_line": 2,
                        "expected": "two",
                        "new": "TWO\n"
                    },
                    {
                        "kind": "insert_after",
                        "anchor": "three\n",
                        "text": "four\n"
                    },
                    {
                        "kind": "delete_lines",
                        "start_line": 1,
                        "end_line": 1,
                        "expected": "one"
                    }
                ]),
            ),
        ]))
        .await
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "TWO\nthree\nfour\n"
        );
    }

    #[test]
    fn coding_tools_exposes_edit_file_instead_of_patch_file() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        let tools = coding_tools(&scope);
        let names = tools
            .iter()
            .map(|tool| tool.descriptor().function.name)
            .collect::<Vec<_>>();

        assert!(names.contains(&"edit_file".to_string()));
        assert!(!names.contains(&"patch_file".to_string()));
    }

    #[test]
    fn rejects_absolute_shell_tokens() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        assert!(validate_shell_command("ls /home/user", &scope.root).is_err());
        assert!(validate_shell_command("cat ../task.md", &scope.root).is_err());
        assert!(validate_shell_command("cd workspace && cargo test", &scope.root).is_ok());
    }

    #[test]
    fn permits_absolute_shell_tokens_inside_tool_root() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        let file = temp.path().join("src-main.rs");
        std::fs::write(&file, "fn main() {}\n").unwrap();

        assert!(validate_shell_command(&format!("cat {}", file.display()), &scope.root).is_ok());
    }

    #[test]
    fn truncates_long_output() {
        let bytes = vec![b'a'; MAX_CAPTURED_OUTPUT_BYTES + 10];
        let output = capture_output(&bytes);
        assert_eq!(output.content.len(), MAX_CAPTURED_OUTPUT_BYTES);
        assert!(output.truncated);
    }

    #[test]
    fn selects_bounded_line_ranges() {
        let selected = select_lines("one\ntwo\nthree\nfour", Some(2), Some(3));
        assert_eq!(selected.content, "two\nthree");
        assert_eq!(selected.selected_start, 2);
        assert_eq!(selected.selected_end, 3);
        assert_eq!(selected.total_lines, 4);
        assert!(selected.truncated);
    }

    #[test]
    fn rejects_inverted_line_ranges() {
        assert!(validate_line_range(Some(10), Some(2)).is_err());
    }

    #[test]
    fn limits_text_by_bytes() {
        let limited = limit_text("abcdef", 3);
        assert_eq!(limited.content, "abc");
        assert!(limited.truncated);
    }

    #[test]
    fn list_tree_respects_depth_hidden_defaults_and_gitignore() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        std::fs::create_dir_all(temp.path().join("workspace/src")).unwrap();
        std::fs::create_dir_all(temp.path().join("target/debug")).unwrap();
        std::fs::create_dir_all(temp.path().join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(temp.path().join(".venv/lib")).unwrap();
        std::fs::write(temp.path().join("task.md"), "task").unwrap();
        std::fs::write(
            temp.path().join(".gitignore"),
            "target/\nnode_modules/\n.venv/\n",
        )
        .unwrap();
        std::fs::write(temp.path().join(".secret"), "secret").unwrap();
        std::fs::write(temp.path().join("workspace/src/lib.rs"), "lib").unwrap();
        std::fs::write(temp.path().join("target/debug/build-output"), "noise").unwrap();
        std::fs::write(temp.path().join("node_modules/pkg/index.js"), "noise").unwrap();
        std::fs::write(temp.path().join(".venv/lib/site.py"), "noise").unwrap();

        let tool = ListTreeTool { scope };
        let mut args = HashMap::new();
        args.insert("max_depth".to_string(), json!(1));
        let result = tool.list(&args).unwrap();
        let entries = result["entries"].as_array().unwrap();
        let paths = entries
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert!(paths.contains(&"task.md"));
        assert!(paths.contains(&"workspace"));
        assert!(paths.contains(&"workspace/src"));
        assert!(!paths.contains(&".secret"));
        assert!(!paths.contains(&"target"));
        assert!(!paths.contains(&"target/debug"));
        assert!(!paths.contains(&"node_modules"));
        assert!(!paths.contains(&".venv"));
        assert!(!paths.contains(&"workspace/src/lib.rs"));
    }

    #[test]
    fn list_tree_does_not_special_case_target_without_gitignore() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        std::fs::create_dir_all(temp.path().join("target/debug")).unwrap();
        std::fs::write(temp.path().join("target/debug/build-output"), "noise").unwrap();

        let tool = ListTreeTool { scope };
        let mut args = HashMap::new();
        args.insert("max_depth".to_string(), json!(1));
        let result = tool.list(&args).unwrap();
        let entries = result["entries"].as_array().unwrap();
        let paths = entries
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert!(paths.contains(&"target"));
        assert!(paths.contains(&"target/debug"));
    }

    #[test]
    fn restricted_scope_hides_sibling_tasks_from_tree() {
        let temp = tempfile::tempdir().unwrap();
        let scope = restricted_scope(&temp);
        std::fs::create_dir_all(temp.path().join("workspace/src")).unwrap();
        std::fs::write(temp.path().join("task.md"), "broad task").unwrap();
        std::fs::write(temp.path().join("task-model-first.md"), "narrow task").unwrap();
        std::fs::write(temp.path().join("notes.md"), "notes").unwrap();
        std::fs::write(temp.path().join("workspace/src/lib.rs"), "lib").unwrap();

        let tool = ListTreeTool { scope };
        let result = tool.list(&HashMap::new()).unwrap();
        let entries = result["entries"].as_array().unwrap();
        let paths = entries
            .iter()
            .map(|entry| entry["path"].as_str().unwrap())
            .collect::<Vec<_>>();

        assert!(paths.contains(&"task-model-first.md"));
        assert!(paths.contains(&"workspace"));
        assert!(paths.contains(&"workspace/src/lib.rs"));
        assert!(!paths.contains(&"task.md"));
        assert!(!paths.contains(&"notes.md"));
    }

    #[test]
    fn restricted_scope_denies_direct_sibling_reads_and_writes() {
        let temp = tempfile::tempdir().unwrap();
        let scope = restricted_scope(&temp);
        std::fs::create_dir_all(temp.path().join("workspace")).unwrap();
        std::fs::write(temp.path().join("task.md"), "broad task").unwrap();
        let sibling = scope.resolve_existing_or_new("task.md").unwrap();
        let workspace = scope.resolve_existing_or_new("workspace/lib.rs").unwrap();

        assert!(scope.check_read(&sibling).is_err());
        assert!(scope.check_write(&sibling).is_err());
        assert!(scope.check_write(&workspace).is_ok());
    }

    #[test]
    fn restricted_shell_defaults_to_writable_workspace() {
        let temp = tempfile::tempdir().unwrap();
        let scope = restricted_scope(&temp);
        std::fs::create_dir_all(temp.path().join("workspace")).unwrap();

        let cwd = scope.resolve_shell_cwd(None).unwrap();

        assert_eq!(scope.relative_display(&cwd), "workspace");
    }

    #[test]
    fn resolves_shell_cwd_root_aliases_inside_tool_root() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        std::fs::create_dir_all(temp.path().join("src")).unwrap();

        let slash = scope.resolve_shell_cwd(Some("/")).unwrap();
        let absolute_root = scope
            .resolve_shell_cwd(Some(temp.path().to_str().unwrap()))
            .unwrap();
        let absolute_child = scope
            .resolve_shell_cwd(Some(temp.path().join("src").to_str().unwrap()))
            .unwrap();

        assert_eq!(slash, *scope.root);
        assert_eq!(absolute_root, *scope.root);
        assert_eq!(scope.relative_display(&absolute_child), "src");
    }

    #[test]
    fn shell_timeout_policy_supports_long_local_builds() {
        assert_eq!(DEFAULT_SHELL_TIMEOUT_SECS, 300);
        assert_eq!(MAX_SHELL_TIMEOUT_SECS, 1800);
        assert_eq!(PATCH_TIMEOUT_SECS, 300);
    }

    #[test]
    fn requires_shell_probe_after_write_budget() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        let source_path = scope.root.join("src/main.rs");
        for _ in 0..MAX_CONSECUTIVE_WRITES_WITHOUT_SHELL {
            assert!(
                scope
                    .note_write_intent(std::slice::from_ref(&source_path))
                    .is_ok()
            );
        }
        assert!(
            scope
                .note_write_intent(std::slice::from_ref(&source_path))
                .is_err()
        );
        scope.note_validation_probe();
        assert!(
            scope
                .note_write_intent(std::slice::from_ref(&source_path))
                .is_ok()
        );
    }

    #[test]
    fn tracks_writes_since_last_shell_probe() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        let source_path = scope.root.join("src/main.rs");

        assert_eq!(scope.policy_snapshot().writes_since_shell_probe, 0);
        assert_eq!(scope.policy_snapshot().total_tool_calls, 0);
        scope
            .note_write_intent(std::slice::from_ref(&source_path))
            .unwrap();
        scope.note_tool_call();
        let dirty = scope.policy_snapshot();
        assert_eq!(dirty.total_tool_calls, 1);
        assert_eq!(dirty.writes_since_shell_probe, 1);
        assert_eq!(dirty.writes_since_shell_probe_paths["src/main.rs"], 1);
        assert!(dirty.validation_required_after_write);
        assert_eq!(dirty.total_write_operations, 1);
        assert_eq!(dirty.total_shell_probes, 0);

        scope.note_validation_probe();
        let clean = scope.policy_snapshot();
        assert_eq!(clean.writes_since_shell_probe, 0);
        assert!(clean.writes_since_shell_probe_paths.is_empty());
        assert!(!clean.validation_required_after_write);
        assert_eq!(clean.total_write_operations, 1);
        assert_eq!(clean.total_shell_probes, 1);
    }

    #[test]
    fn docs_only_writes_do_not_require_validation_after_probe() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        let readme_path = scope.root.join("README.md");

        scope
            .note_write_intent(std::slice::from_ref(&readme_path))
            .unwrap();
        let snapshot = scope.policy_snapshot();

        assert_eq!(snapshot.writes_since_shell_probe, 1);
        assert_eq!(snapshot.writes_since_shell_probe_paths["README.md"], 1);
        assert!(!snapshot.validation_required_after_write);
    }

    #[test]
    fn classifies_validation_probe_commands() {
        assert!(is_validation_probe("cargo check 2>&1 | tail -30"));
        assert!(is_validation_probe("cargo run -- --simulate 600"));
        assert!(is_validation_probe("pytest tests"));
        assert!(!is_validation_probe("echo \"test\""));
        assert!(!is_validation_probe("pwd && ls -la"));
    }

    #[test]
    fn summarizes_validation_command_families_and_failures() {
        assert_eq!(
            command_family("cargo clippy --all-targets -- -D warnings"),
            "cargo clippy"
        );
        assert_eq!(command_family("python -m pytest tests"), "python -m pytest");
        assert_eq!(
            failure_summary("", " \nerror[E0425]: cannot find value `x`\nnext"),
            "error[E0425]: cannot find value `x`"
        );
    }

    #[test]
    fn extracts_targeted_cargo_test_failure_details() {
        let stdout = r#"
running 21 tests
test tests::test_invader_shot_removed_when_leaving_bottom_edge ... FAILED

failures:

---- tests::test_invader_shot_removed_when_leaving_bottom_edge stdout ----

thread 'tests::test_invader_shot_removed_when_leaving_bottom_edge' (31479173) panicked at src/lib.rs:739:9:
Invader shot should be removed when leaving bottom edge

failures:
    tests::test_invader_shot_removed_when_leaving_bottom_edge

test result: FAILED. 20 passed; 1 failed; finished in 0.00s
"#;

        let details = failure_details("", stdout);

        assert!(details
            .iter()
            .any(|detail| detail == "tests::test_invader_shot_removed_when_leaving_bottom_edge"));
        assert!(
            details
                .iter()
                .any(|detail| detail.contains("src/lib.rs:739:9"))
        );
    }

    #[tokio::test]
    async fn failed_validation_probe_activates_repair_state() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        let tool = ShellCommandTool {
            scope: scope.clone(),
        };
        let mut args = HashMap::new();
        args.insert(
            "command".to_string(),
            json!("cargo test --manifest-path missing/Cargo.toml"),
        );

        let result = tool.shell(&args).await.unwrap();
        let snapshot = scope.policy_snapshot();
        let repair = snapshot.validation_repair.unwrap();

        assert_eq!(result["validation_probe"], true);
        assert_eq!(result["success"], false);
        assert_eq!(
            repair.command,
            "cargo test --manifest-path missing/Cargo.toml"
        );
        assert_eq!(repair.command_family, "cargo test");
        assert_eq!(repair.repeated_command_family_count, 1);
        assert!(!repair.failure_text.is_empty());
    }

    #[tokio::test]
    async fn validation_repair_tracks_read_targets() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let shell = ShellCommandTool {
            scope: scope.clone(),
        };
        shell
            .shell(&HashMap::from([(
                "command".to_string(),
                json!("cargo clippy --manifest-path missing/Cargo.toml"),
            )]))
            .await
            .unwrap();

        let reader = ReadFileTool {
            scope: scope.clone(),
        };
        reader
            .read(&HashMap::from([("path".to_string(), json!("src/main.rs"))]))
            .await
            .unwrap();

        let snapshot = scope.policy_snapshot();
        assert_eq!(snapshot.validation_repair_read_paths["src/main.rs"], 1);
    }

    #[tokio::test]
    async fn observation_shell_command_does_not_reset_write_budget() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        scope
            .note_write_intent(std::slice::from_ref(&scope.root.join("src/main.rs")))
            .unwrap();
        let tool = ShellCommandTool {
            scope: scope.clone(),
        };
        let mut args = HashMap::new();
        args.insert("command".to_string(), json!("echo test"));
        let result = tool.shell(&args).await.unwrap();

        assert_eq!(result["validation_probe"], false);
        assert_eq!(scope.policy_snapshot().writes_since_shell_probe, 1);
    }

    #[tokio::test]
    async fn source_mutating_shell_command_counts_as_write() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "pub fn value() -> i32 { 1 }\n",
        )
        .unwrap();
        let tool = ShellCommandTool {
            scope: scope.clone(),
        };
        let mut args = HashMap::new();
        args.insert(
            "command".to_string(),
            json!("printf 'pub fn value() -> i32 { 2 }\\n' > src/lib.rs"),
        );

        let result = tool.shell(&args).await.unwrap();
        let snapshot = scope.policy_snapshot();

        assert_eq!(result["validation_probe"], false);
        assert_eq!(result["success"], true);
        assert_eq!(result["shell_mutation_sensed"], true);
        assert_eq!(result["shell_mutation_paths"], json!(["src/lib.rs"]));
        assert_eq!(snapshot.total_write_operations, 1);
        assert_eq!(snapshot.writes_since_shell_probe, 1);
        assert_eq!(snapshot.writes_since_shell_probe_paths["src/lib.rs"], 1);
        assert!(snapshot.validation_required_after_write);
    }

    #[tokio::test]
    async fn mutating_validation_probe_counts_as_write_after_probe_reset() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"formatter-probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::write(temp.path().join("src/lib.rs"), "pub fn value()->i32{1}\n").unwrap();
        let tool = ShellCommandTool {
            scope: scope.clone(),
        };
        let mut args = HashMap::new();
        args.insert("command".to_string(), json!("cargo fmt"));

        let result = tool.shell(&args).await.unwrap();
        let snapshot = scope.policy_snapshot();

        assert_eq!(result["validation_probe"], true);
        assert_eq!(result["success"], true);
        assert_eq!(result["shell_mutation_sensed"], true);
        assert_eq!(result["shell_mutation_paths"], json!(["src/lib.rs"]));
        assert_eq!(snapshot.total_shell_probes, 1);
        assert_eq!(snapshot.total_write_operations, 1);
        assert_eq!(snapshot.writes_since_shell_probe, 1);
        assert_eq!(snapshot.writes_since_shell_probe_paths["src/lib.rs"], 1);
        assert!(snapshot.validation_required_after_write);
    }

    #[tokio::test]
    async fn tool_payload_measurement_tracks_cumulative_bytes() {
        let temp = tempfile::tempdir().unwrap();
        let scope = scope(&temp);
        let tool = ShellCommandTool {
            scope: scope.clone(),
        };
        let mut args = HashMap::new();
        args.insert("command".to_string(), json!("printf measured"));

        tool.run(&args, &ToolRunCtx::default()).await.unwrap();
        let snapshot = scope.policy_snapshot();
        let trace = std::fs::read_to_string(scope.trace.path()).unwrap();

        assert!(snapshot.total_tool_result_chars > 0);
        assert!(snapshot.total_tool_result_estimated_tokens > 0);
        assert_eq!(
            snapshot.max_tool_result_kind,
            Some("tool.shell_command".to_string())
        );
        assert!(snapshot.tool_result_chars_by_kind["tool.shell_command"] > 0);
        assert!(trace.contains("\"kind\":\"tool.payload.measured\""));
        assert!(trace.contains("\"kind\":\"tool.shell_command\""));
    }

    #[tokio::test]
    async fn shell_command_uses_pipefail() {
        let temp = tempfile::tempdir().unwrap();
        let tool = ShellCommandTool {
            scope: scope(&temp),
        };
        let mut args = HashMap::new();
        args.insert("command".to_string(), json!("false | true"));
        let result = tool.shell(&args).await.unwrap();

        assert_eq!(result["success"], false);
    }
}
