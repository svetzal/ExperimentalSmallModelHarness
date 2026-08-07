//! Pure runtime observations, state transitions, and policy decisions.
//!
//! This module deliberately has no knowledge of providers, filesystems,
//! subprocesses, clocks, tracing, or domain profiles. Effect adapters classify
//! real outcomes into [`RuntimeEvent`] values and translate decisions back into
//! the legacy trace vocabulary at the orchestration boundary.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_CONSECUTIVE_WRITES_WITHOUT_PROBE: usize = 3;
pub const FAILED_VALIDATION_REPAIR_WRITE_ALLOWANCE: usize = 1;
pub const MAX_REPAIR_NO_ACTION_TURNS: usize = 2;
pub const MAX_ACTION_BOUNDARY_NO_ACTION_TURNS: usize = 2;
pub const EMPTY_RESPONSE_ESCALATION_TURNS: usize = 3;
pub const MAX_PRE_VALIDATION_REPEATED_INSPECTIONS: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RuntimeEvent {
    RunStarted,
    RunFinished,
    TurnStarted {
        turn: usize,
    },
    ModelCallStarted {
        turn: usize,
        depth: usize,
    },
    ModelContent {
        chars: usize,
    },
    ModelThinking {
        chars: usize,
    },
    ModelToolCall {
        name: String,
    },
    ModelNoContent,
    ToolRead {
        path: String,
    },
    ToolMutation {
        paths: Vec<String>,
        evidence_invalidating_paths: Vec<String>,
        source: MutationSource,
    },
    ValidationProbe {
        probe_id: Option<String>,
        command: String,
        command_family: String,
        status: Option<i32>,
        success: bool,
        clears_pending_mutations: bool,
        caused_mutation: bool,
        failure_text: String,
        failure_details: Vec<String>,
    },
    RequestedProbeObserved {
        probe_id: String,
        command: String,
        status: Option<i32>,
        success: bool,
    },
    ActionBoundaryInterrupted {
        turn: usize,
    },
    RepairNoContentInterrupted {
        turn: usize,
    },
    RepairDepthExceeded {
        turn: usize,
        reason: RepairDepthReason,
    },
    Inspection {
        signature: String,
    },
    TurnFinished {
        turn: usize,
        content: bool,
        thinking: bool,
        tool_calls: usize,
        mutated: bool,
        probed: bool,
        repair_was_active_before: bool,
        repair_interrupted: bool,
        action_boundary_interrupted: bool,
    },
    TerminalToken {
        token: TerminalToken,
    },
    ManualStop {
        reason: String,
    },
    EnvironmentStop {
        reason: String,
    },
    EffectFailed {
        effect: String,
        error: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MutationSource {
    Write,
    Patch,
    Shell,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalToken {
    Done,
    Fail,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RepairDepthReason {
    MaxLlmCallDepth,
    RedContextAfterRepairAction,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ValidationRepair {
    pub command: String,
    pub command_family: String,
    pub status: Option<i32>,
    pub failure_text: String,
    pub failure_details: Vec<String>,
    pub repeated_command_family_count: usize,
    pub repeated_failure_summary_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SuccessfulValidation {
    pub probe_id: Option<String>,
    pub command: String,
    pub command_family: String,
    pub status: Option<i32>,
    pub probe_epoch: usize,
    pub mutation_epoch: usize,
    pub total_write_operations: usize,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    Pending,
    Passed,
    Failed,
    Stale,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequestedProbeState {
    pub id: String,
    pub status: ProbeStatus,
    pub observed_command: Option<String>,
    pub status_code: Option<i32>,
    pub mutation_epoch: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EffectFailure {
    pub effect: String,
    pub error: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeState {
    pub run_started: bool,
    pub run_finished: bool,
    pub current_turn: usize,
    pub total_tool_calls: usize,
    pub consecutive_writes_without_probe: usize,
    pub writes_since_probe: usize,
    pub writes_since_probe_paths: BTreeMap<String, usize>,
    pub pending_evidence_paths: BTreeMap<String, usize>,
    pub total_write_operations: usize,
    pub total_validation_probes: usize,
    pub mutation_epoch: usize,
    pub fresh_validation_epoch: Option<usize>,
    pub latest_successful_validation: Option<SuccessfulValidation>,
    pub validation_repair: Option<ValidationRepair>,
    pub validation_repair_write_allowance: usize,
    pub validation_repair_read_paths: BTreeMap<String, usize>,
    pub repeated_command_failures: BTreeMap<String, usize>,
    pub repeated_failure_summaries: BTreeMap<String, usize>,
    pub requested_probes: Vec<RequestedProbeState>,
    pub emitted_first_source_mutation: bool,
    pub emitted_first_validation_probe: bool,
    pub emitted_first_post_repair_action: bool,
    pub consecutive_action_boundary_no_action_turns: usize,
    pub active_repair_failure_key: Option<String>,
    pub consecutive_repair_no_action_turns: usize,
    pub repeated_inspections: BTreeMap<String, usize>,
    pub meaningful_action_seen: bool,
    pub consecutive_empty_responses: usize,
    pub consecutive_hidden_only_no_action_turns: usize,
    pub terminal_readiness: bool,
    pub terminal_token: Option<TerminalToken>,
    pub manual_stop: Option<String>,
    pub environment_stop: Option<String>,
    pub effect_failures: Vec<EffectFailure>,
    last_action_boundary_interrupt_turn: Option<usize>,
    last_repair_interrupt_turn: Option<usize>,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl RuntimeState {
    pub fn new(requested_probe_ids: Vec<String>) -> Self {
        let requested_probes = requested_probe_ids
            .into_iter()
            .map(|id| RequestedProbeState {
                id,
                status: ProbeStatus::Pending,
                observed_command: None,
                status_code: None,
                mutation_epoch: None,
            })
            .collect();
        Self {
            run_started: false,
            run_finished: false,
            current_turn: 0,
            total_tool_calls: 0,
            consecutive_writes_without_probe: 0,
            writes_since_probe: 0,
            writes_since_probe_paths: BTreeMap::new(),
            pending_evidence_paths: BTreeMap::new(),
            total_write_operations: 0,
            total_validation_probes: 0,
            mutation_epoch: 0,
            fresh_validation_epoch: None,
            latest_successful_validation: None,
            validation_repair: None,
            validation_repair_write_allowance: 0,
            validation_repair_read_paths: BTreeMap::new(),
            repeated_command_failures: BTreeMap::new(),
            repeated_failure_summaries: BTreeMap::new(),
            requested_probes,
            emitted_first_source_mutation: false,
            emitted_first_validation_probe: false,
            emitted_first_post_repair_action: false,
            consecutive_action_boundary_no_action_turns: 0,
            active_repair_failure_key: None,
            consecutive_repair_no_action_turns: 0,
            repeated_inspections: BTreeMap::new(),
            meaningful_action_seen: false,
            consecutive_empty_responses: 0,
            consecutive_hidden_only_no_action_turns: 0,
            terminal_readiness: false,
            terminal_token: None,
            manual_stop: None,
            environment_stop: None,
            effect_failures: Vec::new(),
            last_action_boundary_interrupt_turn: None,
            last_repair_interrupt_turn: None,
        }
    }

    pub fn reduce(&mut self, event: &RuntimeEvent) {
        match event {
            RuntimeEvent::RunStarted => self.run_started = true,
            RuntimeEvent::RunFinished => self.run_finished = true,
            RuntimeEvent::TurnStarted { turn } => self.current_turn = *turn,
            RuntimeEvent::ModelToolCall { .. } => self.total_tool_calls += 1,
            RuntimeEvent::ToolRead { path } => {
                if self.validation_repair.is_some() {
                    *self
                        .validation_repair_read_paths
                        .entry(path.clone())
                        .or_insert(0) += 1;
                }
            }
            RuntimeEvent::ToolMutation {
                paths,
                evidence_invalidating_paths,
                ..
            } => {
                if self.consecutive_writes_without_probe >= MAX_CONSECUTIVE_WRITES_WITHOUT_PROBE
                    && !evidence_invalidating_paths.is_empty()
                    && self.validation_repair.is_some()
                    && self.validation_repair_write_allowance > 0
                {
                    self.validation_repair_write_allowance -= 1;
                }
                self.consecutive_writes_without_probe += 1;
                self.writes_since_probe += 1;
                self.total_write_operations += 1;
                for path in paths {
                    *self
                        .writes_since_probe_paths
                        .entry(path.clone())
                        .or_insert(0) += 1;
                }
                for path in evidence_invalidating_paths {
                    *self.pending_evidence_paths.entry(path.clone()).or_insert(0) += 1;
                }
                if !evidence_invalidating_paths.is_empty() {
                    self.mutation_epoch += 1;
                    self.fresh_validation_epoch = None;
                    self.emitted_first_source_mutation = true;
                    if self.validation_repair.is_some() {
                        self.emitted_first_post_repair_action = true;
                    }
                    for probe in &mut self.requested_probes {
                        if probe.status == ProbeStatus::Passed {
                            probe.status = ProbeStatus::Stale;
                        }
                    }
                }
                self.meaningful_action_seen = true;
                self.repeated_inspections.clear();
                self.recompute_terminal_readiness();
            }
            RuntimeEvent::ValidationProbe {
                probe_id,
                command,
                command_family,
                status,
                success,
                clears_pending_mutations,
                caused_mutation,
                failure_text,
                failure_details,
            } => {
                if *caused_mutation {
                    self.mutation_epoch += 1;
                    self.fresh_validation_epoch = None;
                    for probe in &mut self.requested_probes {
                        if probe.status == ProbeStatus::Passed {
                            probe.status = ProbeStatus::Stale;
                        }
                    }
                }
                let had_pending_mutations = !self.pending_evidence_paths.is_empty();
                self.total_validation_probes += 1;
                self.emitted_first_validation_probe = true;
                self.meaningful_action_seen = true;
                self.repeated_inspections.clear();
                if self.validation_repair.is_some() {
                    self.emitted_first_post_repair_action = true;
                }
                if let Some(id) = probe_id
                    && let Some(probe) = self
                        .requested_probes
                        .iter_mut()
                        .find(|probe| probe.id == *id)
                {
                    probe.status = if *success && !*caused_mutation {
                        ProbeStatus::Passed
                    } else {
                        ProbeStatus::Failed
                    };
                    probe.observed_command = Some(command.clone());
                    probe.status_code = *status;
                    probe.mutation_epoch = Some(self.mutation_epoch);
                }
                if *success {
                    if *clears_pending_mutations && !*caused_mutation {
                        self.consecutive_writes_without_probe = 0;
                        self.writes_since_probe = 0;
                        self.writes_since_probe_paths.clear();
                        self.pending_evidence_paths.clear();
                    }
                    self.validation_repair = None;
                    self.validation_repair_write_allowance = 0;
                    self.validation_repair_read_paths.clear();
                    self.active_repair_failure_key = None;
                    self.consecutive_repair_no_action_turns = 0;
                    if self.pending_evidence_paths.is_empty() && !*caused_mutation {
                        self.fresh_validation_epoch = Some(self.mutation_epoch);
                        if had_pending_mutations {
                            self.latest_successful_validation = Some(SuccessfulValidation {
                                probe_id: probe_id.clone(),
                                command: command.clone(),
                                command_family: command_family.clone(),
                                status: *status,
                                probe_epoch: self.total_validation_probes,
                                mutation_epoch: self.mutation_epoch,
                                total_write_operations: self.total_write_operations,
                            });
                        }
                    }
                } else {
                    let command_count = self
                        .repeated_command_failures
                        .entry(command_family.clone())
                        .or_insert(0);
                    *command_count += 1;
                    let summary_count = self
                        .repeated_failure_summaries
                        .entry(failure_text.clone())
                        .or_insert(0);
                    *summary_count += 1;
                    let repair = ValidationRepair {
                        command: command.clone(),
                        command_family: command_family.clone(),
                        status: *status,
                        failure_text: failure_text.clone(),
                        failure_details: failure_details.clone(),
                        repeated_command_family_count: *command_count,
                        repeated_failure_summary_count: *summary_count,
                    };
                    self.active_repair_failure_key = Some(repair.failure_key());
                    self.validation_repair = Some(repair);
                    self.validation_repair_write_allowance = if had_pending_mutations {
                        FAILED_VALIDATION_REPAIR_WRITE_ALLOWANCE
                    } else {
                        0
                    };
                    self.validation_repair_read_paths.clear();
                    self.fresh_validation_epoch = None;
                }
                self.recompute_terminal_readiness();
            }
            RuntimeEvent::RequestedProbeObserved {
                probe_id,
                command,
                status,
                success,
            } => {
                if let Some(probe) = self
                    .requested_probes
                    .iter_mut()
                    .find(|probe| probe.id == *probe_id)
                {
                    probe.status = if *success {
                        ProbeStatus::Passed
                    } else {
                        ProbeStatus::Failed
                    };
                    probe.observed_command = Some(command.clone());
                    probe.status_code = *status;
                    probe.mutation_epoch = Some(self.mutation_epoch);
                }
                self.recompute_terminal_readiness();
            }
            RuntimeEvent::ActionBoundaryInterrupted { turn } => {
                self.last_action_boundary_interrupt_turn = Some(*turn)
            }
            RuntimeEvent::RepairNoContentInterrupted { turn } => {
                self.last_repair_interrupt_turn = Some(*turn)
            }
            RuntimeEvent::Inspection { signature } => {
                if !self.meaningful_action_seen {
                    *self
                        .repeated_inspections
                        .entry(signature.clone())
                        .or_insert(0) += 1;
                }
            }
            RuntimeEvent::TurnFinished {
                turn,
                content,
                thinking,
                tool_calls,
                mutated,
                probed,
                repair_was_active_before,
                repair_interrupted,
                action_boundary_interrupted,
            } => {
                let acted = *mutated || *probed;
                let action_boundary_progress = *probed || (*mutated && !self.validation_required());
                if action_boundary_progress {
                    self.consecutive_action_boundary_no_action_turns = 0;
                } else if *action_boundary_interrupted
                    || self.last_action_boundary_interrupt_turn == Some(*turn)
                {
                    self.consecutive_action_boundary_no_action_turns += 1;
                }
                if self.validation_repair.is_none() {
                    self.active_repair_failure_key = None;
                    self.consecutive_repair_no_action_turns = 0;
                } else if !*repair_was_active_before
                    || (!*repair_interrupted
                        && self.last_repair_interrupt_turn != Some(*turn)
                        && acted)
                {
                    self.consecutive_repair_no_action_turns = 0;
                } else if *repair_interrupted
                    || self.last_repair_interrupt_turn == Some(*turn)
                    || !acted
                {
                    self.consecutive_repair_no_action_turns += 1;
                }
                if *content {
                    self.consecutive_empty_responses = 0;
                    self.consecutive_hidden_only_no_action_turns = 0;
                } else if *thinking && !acted && !*action_boundary_interrupted {
                    self.consecutive_hidden_only_no_action_turns += 1;
                } else if *tool_calls == 0 {
                    self.consecutive_empty_responses += 1;
                    self.consecutive_hidden_only_no_action_turns = 0;
                } else {
                    self.consecutive_empty_responses = 0;
                    self.consecutive_hidden_only_no_action_turns = 0;
                }
                self.last_action_boundary_interrupt_turn = None;
                self.last_repair_interrupt_turn = None;
            }
            RuntimeEvent::TerminalToken { token } => self.terminal_token = Some(*token),
            RuntimeEvent::ManualStop { reason } => self.manual_stop = Some(reason.clone()),
            RuntimeEvent::EnvironmentStop { reason } => {
                self.environment_stop = Some(reason.clone())
            }
            RuntimeEvent::EffectFailed { effect, error } => {
                self.effect_failures.push(EffectFailure {
                    effect: effect.clone(),
                    error: error.clone(),
                })
            }
            RuntimeEvent::ModelCallStarted { .. }
            | RuntimeEvent::ModelContent { .. }
            | RuntimeEvent::ModelThinking { .. }
            | RuntimeEvent::ModelNoContent
            | RuntimeEvent::RepairDepthExceeded { .. } => {}
        }
    }

    pub fn validation_required(&self) -> bool {
        !self.pending_evidence_paths.is_empty()
    }

    pub fn requested_probes_satisfied(&self) -> bool {
        self.requested_probes.iter().all(|probe| {
            probe.status == ProbeStatus::Passed && probe.mutation_epoch == Some(self.mutation_epoch)
        })
    }

    fn recompute_terminal_readiness(&mut self) {
        self.terminal_readiness = !self.validation_required()
            && self.validation_repair.is_none()
            && self.fresh_validation_epoch == Some(self.mutation_epoch)
            && self.requested_probes_satisfied();
    }
}

impl ValidationRepair {
    fn failure_key(&self) -> String {
        format!(
            "{}\n{}",
            self.command_family.trim(),
            self.failure_text.trim()
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum RuntimeDecision {
    Continue,
    RequestValidation,
    AllowMutation { consume_repair_allowance: bool },
    RejectMutation { reason: String },
    PromptRepair,
    EscalateRepair,
    HardStopRepairNoAction,
    PromptActionBoundary,
    HardStopActionBoundary,
    StopRepeatedInspection { signature: String, count: usize },
    PromptEmptyResponse,
    HardStopEmptyResponse,
    AcceptDone,
    RejectDone,
    AcceptFail,
    HardStopRepairDepth { reason: RepairDepthReason },
    StopManual,
    StopEnvironment,
    RecordEffectFailure,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RuntimePolicy;

impl RuntimePolicy {
    pub fn decide(&self, state: &RuntimeState, event: &RuntimeEvent) -> RuntimeDecision {
        if let RuntimeEvent::ToolMutation {
            evidence_invalidating_paths,
            ..
        } = event
        {
            if state.run_finished {
                return RuntimeDecision::RejectMutation {
                    reason: "run is already terminal".to_string(),
                };
            }
            if state.consecutive_writes_without_probe >= MAX_CONSECUTIVE_WRITES_WITHOUT_PROBE {
                let allowance = !evidence_invalidating_paths.is_empty()
                    && state.validation_repair.is_some()
                    && state.validation_repair_write_allowance > 0;
                return if allowance {
                    RuntimeDecision::AllowMutation {
                        consume_repair_allowance: true,
                    }
                } else {
                    RuntimeDecision::RejectMutation {
                        reason: "write budget exhausted: run a shell validation probe before editing again".to_string(),
                    }
                };
            }
            return RuntimeDecision::AllowMutation {
                consume_repair_allowance: false,
            };
        }

        let mut after = state.clone();
        after.reduce(event);
        match event {
            RuntimeEvent::RepairDepthExceeded { reason, .. } => {
                RuntimeDecision::HardStopRepairDepth { reason: *reason }
            }
            RuntimeEvent::Inspection { signature }
                if after
                    .repeated_inspections
                    .get(signature)
                    .copied()
                    .unwrap_or(0)
                    >= MAX_PRE_VALIDATION_REPEATED_INSPECTIONS =>
            {
                RuntimeDecision::StopRepeatedInspection {
                    signature: signature.clone(),
                    count: after.repeated_inspections[signature],
                }
            }
            RuntimeEvent::TurnFinished { .. }
                if after.consecutive_repair_no_action_turns >= MAX_REPAIR_NO_ACTION_TURNS =>
            {
                RuntimeDecision::HardStopRepairNoAction
            }
            RuntimeEvent::TurnFinished { .. } if after.consecutive_repair_no_action_turns > 0 => {
                RuntimeDecision::EscalateRepair
            }
            RuntimeEvent::TurnFinished { .. }
                if after.consecutive_action_boundary_no_action_turns
                    >= MAX_ACTION_BOUNDARY_NO_ACTION_TURNS =>
            {
                RuntimeDecision::HardStopActionBoundary
            }
            RuntimeEvent::TurnFinished { turn, .. }
                if state.last_action_boundary_interrupt_turn == Some(*turn) =>
            {
                RuntimeDecision::PromptActionBoundary
            }
            RuntimeEvent::TurnFinished {
                content: false,
                tool_calls: 0,
                ..
            } if after.consecutive_empty_responses >= EMPTY_RESPONSE_ESCALATION_TURNS => {
                RuntimeDecision::HardStopEmptyResponse
            }
            RuntimeEvent::TurnFinished {
                content: false,
                tool_calls: 0,
                ..
            } => RuntimeDecision::PromptEmptyResponse,
            RuntimeEvent::TerminalToken {
                token: TerminalToken::Done,
            } => {
                if state.terminal_readiness {
                    RuntimeDecision::AcceptDone
                } else {
                    RuntimeDecision::RejectDone
                }
            }
            RuntimeEvent::TerminalToken {
                token: TerminalToken::Fail,
            } => RuntimeDecision::AcceptFail,
            RuntimeEvent::ValidationProbe { success: false, .. } => RuntimeDecision::PromptRepair,
            RuntimeEvent::ValidationProbe { success: true, .. } if after.terminal_readiness => {
                RuntimeDecision::AcceptDone
            }
            RuntimeEvent::ToolMutation { .. } => RuntimeDecision::RequestValidation,
            RuntimeEvent::RequestedProbeObserved { .. } if after.terminal_readiness => {
                RuntimeDecision::AcceptDone
            }
            RuntimeEvent::ManualStop { .. } => RuntimeDecision::StopManual,
            RuntimeEvent::EnvironmentStop { .. } => RuntimeDecision::StopEnvironment,
            RuntimeEvent::EffectFailed { .. } => RuntimeDecision::RecordEffectFailure,
            _ => RuntimeDecision::Continue,
        }
    }
}

/// Token adapter preserving the established terminal grammar.
pub fn terminal_token(response: &str) -> Option<TerminalToken> {
    let first = response
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty());
    if first.is_some_and(|line| {
        line.split_once(char::is_whitespace)
            .map(|(head, _)| head.eq_ignore_ascii_case("FAIL"))
            .unwrap_or_else(|| line.eq_ignore_ascii_case("FAIL"))
    }) {
        return Some(TerminalToken::Fail);
    }
    response
        .lines()
        .map(str::trim)
        .any(|line| line.eq_ignore_ascii_case("DONE"))
        .then_some(TerminalToken::Done)
}

/// Minimal effect boundary used by deterministic orchestration tests and by
/// adapters that must prove policy authorization precedes mutation execution.
pub trait RuntimeEffects {
    type Error;
    fn mutate(&mut self, paths: &[String]) -> Result<(), Self::Error>;
    fn validate(&mut self, probe_id: Option<&str>) -> Result<bool, Self::Error>;
}

pub struct RuntimeOrchestrator<E> {
    pub state: RuntimeState,
    pub policy: RuntimePolicy,
    pub effects: E,
}

impl<E: RuntimeEffects> RuntimeOrchestrator<E> {
    pub fn request_mutation(
        &mut self,
        paths: Vec<String>,
        evidence_invalidating_paths: Vec<String>,
    ) -> Result<RuntimeDecision, E::Error> {
        let event = RuntimeEvent::ToolMutation {
            paths: paths.clone(),
            evidence_invalidating_paths,
            source: MutationSource::Write,
        };
        let decision = self.policy.decide(&self.state, &event);
        if matches!(decision, RuntimeDecision::AllowMutation { .. }) {
            self.effects.mutate(&paths)?;
            self.state.reduce(&event);
        }
        Ok(decision)
    }

    pub fn request_validation(&mut self, probe_id: Option<&str>) -> Result<bool, E::Error> {
        let success = self.effects.validate(probe_id)?;
        self.state.reduce(&RuntimeEvent::ValidationProbe {
            probe_id: probe_id.map(str::to_string),
            command: "declared_probe".to_string(),
            command_family: "declared_probe".to_string(),
            status: Some(if success { 0 } else { 1 }),
            success,
            clears_pending_mutations: success,
            caused_mutation: false,
            failure_text: if success {
                String::new()
            } else {
                "declared probe failed".to_string()
            },
            failure_details: Vec::new(),
        });
        Ok(success)
    }

    pub fn observe_terminal(&mut self, token: TerminalToken) -> RuntimeDecision {
        let event = RuntimeEvent::TerminalToken { token };
        let decision = self.policy.decide(&self.state, &event);
        self.state.reduce(&event);
        if matches!(
            decision,
            RuntimeDecision::AcceptDone | RuntimeDecision::AcceptFail
        ) {
            self.state.reduce(&RuntimeEvent::RunFinished);
        }
        decision
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mutation() -> RuntimeEvent {
        RuntimeEvent::ToolMutation {
            paths: vec!["artifact".into()],
            evidence_invalidating_paths: vec!["artifact".into()],
            source: MutationSource::Write,
        }
    }

    fn probe(id: Option<&str>, success: bool, clears: bool) -> RuntimeEvent {
        RuntimeEvent::ValidationProbe {
            probe_id: id.map(str::to_string),
            command: "probe".into(),
            command_family: "probe-family".into(),
            status: Some(if success { 0 } else { 1 }),
            success,
            clears_pending_mutations: clears,
            caused_mutation: false,
            failure_text: if success { "" } else { "failure" }.into(),
            failure_details: Vec::new(),
        }
    }

    fn apply(state: &mut RuntimeState, events: &[RuntimeEvent]) {
        for event in events {
            state.reduce(event);
        }
    }

    #[test]
    fn mutation_invalidates_evidence_and_fresh_declared_probe_restores_readiness() {
        let mut state = RuntimeState::new(vec!["required".into()]);
        apply(
            &mut state,
            &[mutation(), probe(Some("required"), true, true)],
        );
        assert!(state.terminal_readiness);
        state.reduce(&mutation());
        assert!(!state.terminal_readiness);
        assert_eq!(state.requested_probes[0].status, ProbeStatus::Stale);
        state.reduce(&probe(Some("required"), true, true));
        assert!(state.terminal_readiness);
    }

    #[test]
    fn non_invalidating_mutation_preserves_fresh_terminal_evidence() {
        let mut state = RuntimeState::default();
        state.reduce(&probe(None, true, true));
        assert!(state.terminal_readiness);
        state.reduce(&RuntimeEvent::ToolMutation {
            paths: vec!["notes".into()],
            evidence_invalidating_paths: Vec::new(),
            source: MutationSource::Write,
        });
        assert!(!state.validation_required());
        assert!(state.terminal_readiness);
    }

    #[test]
    fn stale_or_unrequested_probes_do_not_restore_readiness() {
        let mut state = RuntimeState::new(vec!["required".into()]);
        state.reduce(&mutation());
        state.reduce(&probe(None, true, true));
        assert!(!state.terminal_readiness);
        assert_eq!(state.requested_probes[0].status, ProbeStatus::Pending);
        let mut mutating_probe = probe(Some("required"), true, true);
        if let RuntimeEvent::ValidationProbe {
            caused_mutation, ..
        } = &mut mutating_probe
        {
            *caused_mutation = true;
        }
        state.reduce(&mutating_probe);
        assert!(!state.terminal_readiness);
    }

    #[test]
    fn all_declared_probes_must_pass_in_the_current_mutation_epoch() {
        let mut state = RuntimeState::new(vec!["first".into(), "second".into()]);
        state.reduce(&mutation());
        state.reduce(&probe(None, true, true));
        state.reduce(&RuntimeEvent::RequestedProbeObserved {
            probe_id: "second".into(),
            command: "second command".into(),
            status: Some(0),
            success: true,
        });
        assert!(!state.terminal_readiness);
        state.reduce(&RuntimeEvent::RequestedProbeObserved {
            probe_id: "first".into(),
            command: "first command".into(),
            status: Some(0),
            success: true,
        });
        assert!(state.terminal_readiness);
        assert_eq!(state.requested_probes[0].id, "first");
        assert_eq!(state.requested_probes[1].id, "second");
    }

    #[test]
    fn typed_events_are_serializable_without_effect_context() {
        let events = [
            RuntimeEvent::RunStarted,
            RuntimeEvent::TurnStarted { turn: 1 },
            RuntimeEvent::ModelCallStarted { turn: 1, depth: 0 },
            RuntimeEvent::ModelContent { chars: 4 },
            RuntimeEvent::ModelThinking { chars: 8 },
            RuntimeEvent::ModelToolCall {
                name: "effect".into(),
            },
            RuntimeEvent::ModelNoContent,
            RuntimeEvent::ManualStop {
                reason: "operator".into(),
            },
        ];
        for event in events {
            assert!(serde_json::to_string(&event).is_ok());
        }
    }

    #[test]
    fn failed_validation_activates_repair_and_grants_exactly_one_allowance() {
        let policy = RuntimePolicy;
        let mut state = RuntimeState::default();
        for _ in 0..3 {
            state.reduce(&mutation());
        }
        state.reduce(&probe(None, false, false));
        assert!(state.validation_repair.is_some());
        assert_eq!(state.validation_repair_write_allowance, 1);
        assert_eq!(
            policy.decide(&state, &mutation()),
            RuntimeDecision::AllowMutation {
                consume_repair_allowance: true
            }
        );
        state.reduce(&mutation());
        assert_eq!(state.validation_repair_write_allowance, 0);
        assert!(matches!(
            policy.decide(&state, &mutation()),
            RuntimeDecision::RejectMutation { .. }
        ));
    }

    #[test]
    fn repair_edit_probe_resolution_and_no_action_escalation() {
        let policy = RuntimePolicy;
        let mut state = RuntimeState::default();
        state.reduce(&mutation());
        state.reduce(&probe(None, false, false));
        let turn = |turn| RuntimeEvent::TurnFinished {
            turn,
            content: true,
            thinking: false,
            tool_calls: 0,
            mutated: false,
            probed: false,
            repair_was_active_before: true,
            repair_interrupted: false,
            action_boundary_interrupted: false,
        };
        assert_eq!(
            policy.decide(&state, &turn(1)),
            RuntimeDecision::EscalateRepair
        );
        state.reduce(&turn(1));
        assert_eq!(
            policy.decide(&state, &turn(2)),
            RuntimeDecision::HardStopRepairNoAction
        );
        state.reduce(&mutation());
        state.reduce(&probe(None, true, true));
        assert!(state.validation_repair.is_none());
    }

    #[test]
    fn repair_depth_and_red_pressure_are_hard_stops() {
        let policy = RuntimePolicy;
        for reason in [
            RepairDepthReason::MaxLlmCallDepth,
            RepairDepthReason::RedContextAfterRepairAction,
        ] {
            assert_eq!(
                policy.decide(
                    &RuntimeState::default(),
                    &RuntimeEvent::RepairDepthExceeded { turn: 1, reason }
                ),
                RuntimeDecision::HardStopRepairDepth { reason }
            );
        }
    }

    #[test]
    fn action_boundary_stops_after_two_interrupts_and_only_probe_resets_dirty_state() {
        let policy = RuntimePolicy;
        let mut state = RuntimeState::default();
        for turn in 1..=2 {
            state.reduce(&RuntimeEvent::ActionBoundaryInterrupted { turn });
            let finished = RuntimeEvent::TurnFinished {
                turn,
                content: false,
                thinking: true,
                tool_calls: 0,
                mutated: false,
                probed: false,
                repair_was_active_before: false,
                repair_interrupted: false,
                action_boundary_interrupted: true,
            };
            let expected = if turn == 1 {
                RuntimeDecision::PromptActionBoundary
            } else {
                RuntimeDecision::HardStopActionBoundary
            };
            assert_eq!(policy.decide(&state, &finished), expected);
            state.reduce(&finished);
        }

        let mut state = RuntimeState::default();
        state.reduce(&RuntimeEvent::ActionBoundaryInterrupted { turn: 1 });
        state.reduce(&RuntimeEvent::TurnFinished {
            turn: 1,
            content: false,
            thinking: true,
            tool_calls: 0,
            mutated: false,
            probed: false,
            repair_was_active_before: false,
            repair_interrupted: false,
            action_boundary_interrupted: true,
        });
        state.reduce(&mutation());
        state.reduce(&RuntimeEvent::TurnFinished {
            turn: 2,
            content: false,
            thinking: false,
            tool_calls: 1,
            mutated: true,
            probed: false,
            repair_was_active_before: false,
            repair_interrupted: false,
            action_boundary_interrupted: false,
        });
        assert_eq!(state.consecutive_action_boundary_no_action_turns, 1);

        state.reduce(&probe(None, true, true));
        state.reduce(&RuntimeEvent::TurnFinished {
            turn: 3,
            content: false,
            thinking: false,
            tool_calls: 1,
            mutated: false,
            probed: true,
            repair_was_active_before: false,
            repair_interrupted: false,
            action_boundary_interrupted: false,
        });
        assert_eq!(state.consecutive_action_boundary_no_action_turns, 0);
    }

    #[test]
    fn dirty_action_boundary_requires_a_probe_instead_of_another_write() {
        let policy = RuntimePolicy;
        let mut state = RuntimeState::default();
        state.reduce(&mutation());
        state.reduce(&RuntimeEvent::ActionBoundaryInterrupted { turn: 1 });
        let dirty_write_turn = RuntimeEvent::TurnFinished {
            turn: 1,
            content: false,
            thinking: true,
            tool_calls: 1,
            mutated: true,
            probed: false,
            repair_was_active_before: false,
            repair_interrupted: false,
            action_boundary_interrupted: true,
        };

        assert_eq!(
            policy.decide(&state, &dirty_write_turn),
            RuntimeDecision::PromptActionBoundary
        );
        state.reduce(&dirty_write_turn);
        assert_eq!(state.consecutive_action_boundary_no_action_turns, 1);

        state.reduce(&RuntimeEvent::ActionBoundaryInterrupted { turn: 2 });
        let repeated_dirty_write = RuntimeEvent::TurnFinished {
            turn: 2,
            content: false,
            thinking: true,
            tool_calls: 1,
            mutated: true,
            probed: false,
            repair_was_active_before: false,
            repair_interrupted: false,
            action_boundary_interrupted: true,
        };
        assert_eq!(
            policy.decide(&state, &repeated_dirty_write),
            RuntimeDecision::HardStopActionBoundary
        );

        state.reduce(&probe(None, true, true));
        state.reduce(&RuntimeEvent::TurnFinished {
            turn: 2,
            content: false,
            thinking: false,
            tool_calls: 1,
            mutated: false,
            probed: true,
            repair_was_active_before: false,
            repair_interrupted: false,
            action_boundary_interrupted: true,
        });
        assert_eq!(state.consecutive_action_boundary_no_action_turns, 0);
    }

    #[test]
    fn inspection_threshold_and_empty_vs_tool_only_turns() {
        let policy = RuntimePolicy;
        let mut state = RuntimeState::default();
        let inspection = RuntimeEvent::Inspection {
            signature: "same-read".into(),
        };
        for _ in 0..3 {
            assert_eq!(
                policy.decide(&state, &inspection),
                RuntimeDecision::Continue
            );
            state.reduce(&inspection);
        }
        assert!(matches!(
            policy.decide(&state, &inspection),
            RuntimeDecision::StopRepeatedInspection { count: 4, .. }
        ));
        state.reduce(&RuntimeEvent::TurnFinished {
            turn: 1,
            content: false,
            thinking: false,
            tool_calls: 1,
            mutated: false,
            probed: false,
            repair_was_active_before: false,
            repair_interrupted: false,
            action_boundary_interrupted: false,
        });
        assert_eq!(state.consecutive_empty_responses, 0);
        for turn in 2..=4 {
            state.reduce(&RuntimeEvent::TurnFinished {
                turn,
                content: false,
                thinking: false,
                tool_calls: 0,
                mutated: false,
                probed: false,
                repair_was_active_before: false,
                repair_interrupted: false,
                action_boundary_interrupted: false,
            });
        }
        assert_eq!(state.consecutive_empty_responses, 3);
    }

    #[test]
    fn done_fail_manual_environment_and_effect_failure_decisions() {
        let policy = RuntimePolicy;
        let mut state = RuntimeState::default();
        assert_eq!(terminal_token("failing tests"), None);
        assert_eq!(terminal_token("FAIL blocked"), Some(TerminalToken::Fail));
        assert_eq!(terminal_token("summary\nDONE"), Some(TerminalToken::Done));
        assert_eq!(
            policy.decide(
                &state,
                &RuntimeEvent::TerminalToken {
                    token: TerminalToken::Done
                }
            ),
            RuntimeDecision::RejectDone
        );
        state.reduce(&probe(None, true, true));
        assert_eq!(
            policy.decide(
                &state,
                &RuntimeEvent::TerminalToken {
                    token: TerminalToken::Done
                }
            ),
            RuntimeDecision::AcceptDone
        );
        assert_eq!(
            policy.decide(
                &state,
                &RuntimeEvent::TerminalToken {
                    token: TerminalToken::Fail
                }
            ),
            RuntimeDecision::AcceptFail
        );
        for (event, expected) in [
            (
                RuntimeEvent::ManualStop {
                    reason: "operator".into(),
                },
                RuntimeDecision::StopManual,
            ),
            (
                RuntimeEvent::EnvironmentStop {
                    reason: "invalid".into(),
                },
                RuntimeDecision::StopEnvironment,
            ),
            (
                RuntimeEvent::EffectFailed {
                    effect: "write".into(),
                    error: "denied".into(),
                },
                RuntimeDecision::RecordEffectFailure,
            ),
        ] {
            assert_eq!(policy.decide(&state, &event), expected);
            state.reduce(&event);
        }
        assert_eq!(state.effect_failures.len(), 1);
    }

    #[derive(Default)]
    struct FakeEffects {
        mutations: usize,
        validations: usize,
        validation_result: bool,
    }

    impl RuntimeEffects for FakeEffects {
        type Error = ();
        fn mutate(&mut self, _paths: &[String]) -> Result<(), Self::Error> {
            self.mutations += 1;
            Ok(())
        }
        fn validate(&mut self, _probe_id: Option<&str>) -> Result<bool, Self::Error> {
            self.validations += 1;
            Ok(self.validation_result)
        }
    }

    #[test]
    fn fake_effects_run_only_after_policy_allows_them() {
        let mut state = RuntimeState::default();
        for _ in 0..3 {
            state.reduce(&mutation());
        }
        let mut orchestrator = RuntimeOrchestrator {
            state,
            policy: RuntimePolicy,
            effects: FakeEffects::default(),
        };
        assert!(matches!(
            orchestrator.request_mutation(vec!["artifact".into()], vec!["artifact".into()]),
            Ok(RuntimeDecision::RejectMutation { .. })
        ));
        assert_eq!(orchestrator.effects.mutations, 0);
        orchestrator.state.reduce(&probe(None, true, true));
        assert!(matches!(
            orchestrator.request_mutation(vec!["artifact".into()], vec!["artifact".into()]),
            Ok(RuntimeDecision::AllowMutation { .. })
        ));
        assert_eq!(orchestrator.effects.mutations, 1);
        orchestrator.state.reduce(&probe(None, true, true));
        assert_eq!(
            orchestrator.observe_terminal(TerminalToken::Done),
            RuntimeDecision::AcceptDone
        );
        assert!(matches!(
            orchestrator.request_mutation(vec!["later".into()], vec!["later".into()]),
            Ok(RuntimeDecision::RejectMutation { .. })
        ));
        assert_eq!(orchestrator.effects.mutations, 1);
    }

    #[test]
    fn fake_probe_effect_drives_the_same_freshness_transitions() {
        let mut orchestrator = RuntimeOrchestrator {
            state: RuntimeState::new(vec!["exact".into()]),
            policy: RuntimePolicy,
            effects: FakeEffects::default(),
        };
        orchestrator
            .request_mutation(vec!["artifact".into()], vec!["artifact".into()])
            .unwrap();
        assert!(!orchestrator.request_validation(Some("exact")).unwrap());
        assert!(!orchestrator.state.terminal_readiness);
        orchestrator.effects.validation_result = true;
        assert!(orchestrator.request_validation(Some("exact")).unwrap());
        assert!(orchestrator.state.terminal_readiness);
        orchestrator
            .request_mutation(vec!["artifact".into()], vec!["artifact".into()])
            .unwrap();
        assert!(!orchestrator.state.terminal_readiness);
        assert!(orchestrator.request_validation(Some("exact")).unwrap());
        assert!(orchestrator.state.terminal_readiness);
        assert_eq!(orchestrator.effects.validations, 3);
    }
}
