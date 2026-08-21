use super::{DomainProfile, ProfileRef, ToolCapability};
use crate::contract::{
    ArtifactClass, Budgets, EvidenceInvalidation, MutableArtifactClasses, ResolvedRunContract,
};

pub const TERMINAL_PROFILE_ID: &str = "terminal_work";
pub const TERMINAL_PROFILE_VERSION: &str = "terminal_work_profile.v1";

#[derive(Debug, Clone, Copy, Default)]
pub struct TerminalWorkProfile;

impl DomainProfile for TerminalWorkProfile {
    fn profile_ref(&self) -> ProfileRef {
        ProfileRef {
            id: TERMINAL_PROFILE_ID.into(),
            version: TERMINAL_PROFILE_VERSION.into(),
        }
    }

    fn tool_capabilities(&self) -> &'static [ToolCapability] {
        &[
            ToolCapability::ListTree,
            ToolCapability::ReadFile,
            ToolCapability::WriteFile,
            ToolCapability::EditFile,
            ToolCapability::ShellCommand,
        ]
    }

    fn system_guidance(&self) -> String {
        [
            "You are an adaptive terminal-work agent built on Mojentic.",
            "Use the provided tools to inspect the current workspace, modify artifacts, and run shell commands.",
            "All tool paths and shell commands are rooted in the assigned task workspace.",
            "Infer the relevant technologies and validation methods from the task and workspace; do not assume a language, framework, or build system.",
            "Work in small observable steps and preserve unrelated files.",
            "After a mutation, run a deterministic command that checks the requested outcome before making further speculative changes.",
            "Use edit_file for focused changes to existing text and write_file for new files or complete replacements only after reading the current artifact.",
            "Never end a turn with an empty response. Continue using tools, or reply DONE or FAIL as instructed.",
            "Reply exactly DONE only when the requested end state has been reached and checked.",
            "If the task cannot be completed, reply FAIL with one concise reason.",
        ]
        .join("\n")
    }

    fn run_guidance(&self, goal: &str) -> String {
        format!(
            "Complete this task inside the current isolated terminal workspace.\n\n{goal}\n\nInspect the workspace and environment first. Make only task-relevant changes, check the resulting behavior with deterministic commands available in the environment, and use DONE only after that check succeeds."
        )
    }

    fn post_write_validation_nudge(&self, _had_final_text: bool) -> String {
        "You modified the task workspace after the latest check. Do not edit again yet. Run the most relevant deterministic check for the requested outcome, then repair from its concrete output or reply DONE if it passes.".into()
    }

    fn repair_ladder_suffix(&self) -> &'static str {
        "After a focused repair, rerun the failed check before making another edit."
    }

    fn command_family(&self, command: &str) -> String {
        command
            .split_whitespace()
            .next()
            .unwrap_or("unknown")
            .to_ascii_lowercase()
    }

    fn validation_command_family(&self, command: &str) -> String {
        self.command_family(command)
    }

    fn path_requires_validation_after_write(&self, _path: &str) -> bool {
        true
    }

    fn is_ignored_dir(&self, dir_name: &str) -> bool {
        crate::profile::coding::is_ignored_dir(dir_name)
    }

    fn failure_details(&self, stderr: &str, stdout: &str) -> Vec<String> {
        crate::profile::coding::failure_details(stderr, stdout)
    }

    fn resolve_legacy_contract(&self, _goal_text: &str, _budgets: Budgets) -> ResolvedRunContract {
        unreachable!("the terminal-work profile requires an explicit contract")
    }

    fn default_artifact_classes(&self) -> MutableArtifactClasses {
        MutableArtifactClasses {
            classes: vec![ArtifactClass {
                name: "workspace_artifact".into(),
                exempt_file_names: Vec::new(),
                exempt_extensions: Vec::new(),
            }],
        }
    }

    fn default_evidence_invalidation(&self) -> EvidenceInvalidation {
        EvidenceInvalidation {
            invalidated_by_source_mutation: true,
            generation_gated_freshness: true,
            tracked_artifact_classes: vec!["workspace_artifact".into()],
        }
    }

    fn action_intent_phrases(&self) -> &'static [&'static str] {
        &["run the check", "verify the result"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_is_domain_neutral_and_exposes_terminal_tools() {
        let profile = TerminalWorkProfile;
        let prompt = format!(
            "{}\n{}",
            profile.system_guidance(),
            profile.run_guidance("Produce the requested outcome.")
        )
        .to_ascii_lowercase();
        for forbidden in ["cargo", "rust", "python", "npm", "terminal-bench"] {
            assert!(
                !prompt.contains(forbidden),
                "found {forbidden:?} in {prompt:?}"
            );
        }
        assert!(
            profile
                .tool_capabilities()
                .contains(&ToolCapability::ShellCommand)
        );
        assert!(
            !profile
                .tool_capabilities()
                .contains(&ToolCapability::ExecuteProbe)
        );
    }

    #[test]
    fn every_workspace_artifact_requires_fresh_evidence() {
        let profile = TerminalWorkProfile;
        assert!(profile.path_requires_validation_after_write("README.md"));
        assert!(profile.path_requires_validation_after_write("service.conf"));
        assert_eq!(
            profile
                .default_evidence_invalidation()
                .tracked_artifact_classes,
            vec!["workspace_artifact"]
        );
    }
}
