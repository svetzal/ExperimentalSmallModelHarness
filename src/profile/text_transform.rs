use super::{DomainProfile, ProfileRef, ToolCapability};
use crate::contract::{
    ArtifactClass, Budgets, EvidenceInvalidation, MutableArtifactClasses, ResolvedRunContract,
};

pub const TEXT_TRANSFORM_PROFILE_ID: &str = "text_transform";
pub const TEXT_TRANSFORM_PROFILE_VERSION: &str = "text_transform_profile.v1";

#[derive(Debug, Clone, Copy, Default)]
pub struct TextTransformProfile;

impl DomainProfile for TextTransformProfile {
    fn profile_ref(&self) -> ProfileRef {
        ProfileRef {
            id: TEXT_TRANSFORM_PROFILE_ID.into(),
            version: TEXT_TRANSFORM_PROFILE_VERSION.into(),
        }
    }

    fn tool_capabilities(&self) -> &'static [ToolCapability] {
        &[
            ToolCapability::ListTree,
            ToolCapability::ReadFile,
            ToolCapability::WriteFile,
            ToolCapability::EditFile,
            ToolCapability::ExecuteProbe,
        ]
    }

    fn system_guidance(&self) -> String {
        [
            "You are an adaptive scoped text-transformation worker built on Mojentic.",
            "Use the provided tools to inspect and edit UTF-8 text files under the workspace root.",
            "All tool paths are relative to the workspace root.",
            "Execute every declared assertion by its probe ID after the latest edit.",
            "Do not use or infer shell validation commands.",
            "Reply exactly DONE only after every declared assertion passes for the latest edit.",
            "If the task cannot be completed, reply FAIL with one concise reason.",
        ]
        .join("\n")
    }

    fn run_guidance(&self, goal: &str) -> String {
        format!(
            "Complete this scoped text transformation inside the current workspace.\n\n{goal}\n\nInspect the input, create or edit the requested text artifact, then execute each declared assertion by probe ID."
        )
    }

    fn post_write_validation_nudge(&self, _had_final_text: bool) -> String {
        "You modified a tracked text artifact. Execute the declared assertion by probe ID before editing again or replying DONE.".into()
    }

    fn repair_ladder_suffix(&self) -> &'static str {
        "After a focused edit, execute the failed assertion again by probe ID."
    }

    fn command_family(&self, command: &str) -> String {
        command.to_string()
    }
    fn validation_command_family(&self, command: &str) -> String {
        command.to_string()
    }
    fn path_requires_validation_after_write(&self, _path: &str) -> bool {
        true
    }
    fn is_ignored_dir(&self, dir_name: &str) -> bool {
        dir_name == ".git"
    }
    fn failure_details(&self, _stderr: &str, _stdout: &str) -> Vec<String> {
        Vec::new()
    }
    fn resolve_legacy_contract(&self, _goal_text: &str, _budgets: Budgets) -> ResolvedRunContract {
        unreachable!("the text-transform profile requires an explicit contract")
    }
    fn default_artifact_classes(&self) -> MutableArtifactClasses {
        MutableArtifactClasses {
            classes: vec![ArtifactClass {
                name: "text_artifact".into(),
                exempt_file_names: Vec::new(),
                exempt_extensions: Vec::new(),
            }],
        }
    }
    fn default_evidence_invalidation(&self) -> EvidenceInvalidation {
        EvidenceInvalidation {
            invalidated_by_source_mutation: true,
            generation_gated_freshness: true,
            tracked_artifact_classes: vec!["text_artifact".into()],
        }
    }
    fn action_intent_phrases(&self) -> &'static [&'static str] {
        &["execute assertion"]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_fixtures_are_stable() {
        let profile = TextTransformProfile;
        assert_eq!(
            format!("{}\n", profile.system_guidance()),
            include_str!("../../fixtures/prompts/text_transform_system.txt")
        );
        assert_eq!(
            format!(
                "{}\n",
                profile.run_guidance("Read input.txt and create brief.md.")
            ),
            include_str!("../../fixtures/prompts/text_transform_run.txt")
        );
    }
}
