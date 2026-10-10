//! Read-only stash inventories. Object IDs survive reflog index renumbering.
use serde::Serialize;
use std::{collections::BTreeMap, path::Path, time::Duration};
use tokio::process::Command;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Inventory {
    NotRepository,
    Captured {
        git_directory: String,
        object_ids: Vec<String>,
    },
    Unknown,
}

async fn git(root: &Path, args: &[&str]) -> Option<std::process::Output> {
    tokio::time::timeout(
        Duration::from_secs(10),
        Command::new("git")
            .args(args)
            .current_dir(root)
            .env("LC_ALL", "C")
            .env("GIT_OPTIONAL_LOCKS", "0")
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()
}

pub async fn capture(root: &Path) -> Inventory {
    let Some(repo) = git(root, &["rev-parse", "--absolute-git-dir"]).await else {
        return Inventory::Unknown;
    };
    if !repo.status.success() {
        return if String::from_utf8_lossy(&repo.stderr).contains("not a git repository") {
            Inventory::NotRepository
        } else {
            Inventory::Unknown
        };
    }
    let Some(stashes) = git(root, &["stash", "list", "--format=%H"]).await else {
        return Inventory::Unknown;
    };
    let (Ok(directory), Ok(ids)) = (
        String::from_utf8(repo.stdout),
        String::from_utf8(stashes.stdout),
    ) else {
        return Inventory::Unknown;
    };
    if !stashes.status.success() || directory.trim().is_empty() {
        return Inventory::Unknown;
    }
    let object_ids = ids.lines().map(str::to_owned).collect::<Vec<_>>();
    if object_ids
        .iter()
        .any(|id| !matches!(id.len(), 40 | 64) || !id.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Inventory::Unknown;
    }
    Inventory::Captured {
        git_directory: directory.trim().to_owned(),
        object_ids,
    }
}

#[derive(Debug, Serialize)]
pub struct Comparison {
    pub added_object_ids: Vec<String>,
    pub removed_baseline_object_ids: Vec<String>,
    pub attribution: &'static str,
    pub inventory_unknown_or_repository_changed: bool,
}

impl Comparison {
    pub fn blocks_done(&self) -> bool {
        self.inventory_unknown_or_repository_changed
            || !self.added_object_ids.is_empty()
            || !self.removed_baseline_object_ids.is_empty()
    }

    pub fn prompt(&self) -> String {
        format!(
            "DONE withheld: Git stash state differs from entry or cannot be verified. New unresolved stash object IDs: {:?}. Missing baseline stash object IDs: {:?}. Inventory unknown/repository changed: {}. Compare the current workspace with the entry inventory. Resolve new stashes explicitly and validate the actual working tree before DONE. Do not automatically pop/drop pre-existing stashes or discard work. If ownership or restoration is unclear, report FAIL. Stashes appeared during this run; shared-repository attribution is uncertain.",
            self.added_object_ids,
            self.removed_baseline_object_ids,
            self.inventory_unknown_or_repository_changed
        )
    }
}

pub fn compare(baseline: &Inventory, current: &Inventory) -> Comparison {
    let mut result = Comparison {
        added_object_ids: vec![],
        removed_baseline_object_ids: vec![],
        attribution: "appeared_during_run_shared_repository_risk_accepted",
        inventory_unknown_or_repository_changed: false,
    };
    let (before, after) = match (baseline, current) {
        (Inventory::NotRepository, Inventory::NotRepository) => return result,
        (Inventory::NotRepository, Inventory::Captured { object_ids, .. }) => {
            (&[][..], object_ids.as_slice())
        }
        (
            Inventory::Captured {
                git_directory: a,
                object_ids: before,
            },
            Inventory::Captured {
                git_directory: b,
                object_ids: after,
            },
        ) if a == b => (before.as_slice(), after.as_slice()),
        _ => {
            result.inventory_unknown_or_repository_changed = true;
            return result;
        }
    };
    // Counts also detect a second entry pointing at an identical stash object.
    let counts = |ids: &[String]| {
        let mut counts = BTreeMap::<String, usize>::new();
        for id in ids {
            *counts.entry(id.clone()).or_default() += 1;
        }
        counts
    };
    let before = counts(before);
    let after = counts(after);
    for (id, count) in &after {
        result.added_object_ids.extend(std::iter::repeat_n(
            id.clone(),
            count.saturating_sub(*before.get(id).unwrap_or(&0)),
        ));
    }
    for (id, count) in &before {
        result
            .removed_baseline_object_ids
            .extend(std::iter::repeat_n(
                id.clone(),
                count.saturating_sub(*after.get(id).unwrap_or(&0)),
            ));
    }
    result
}

pub fn entry_prompt(inventory: &Inventory) -> Option<String> {
    match inventory {
        Inventory::NotRepository => None,
        _ => Some(format!(
            "Git stash inventory at entry (captured before agent tools): {}. Preserve pre-existing stashes. Any new unresolved stash or removed baseline stash withholds DONE; restoration/disposal is an explicit agent decision, never automatic. Object IDs, not shifting stash indices, identify entries. Shared-repository attribution is uncertain.",
            serde_json::to_string(inventory).expect("stash inventory serialization")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn known(ids: &[&str]) -> Inventory {
        Inventory::Captured {
            git_directory: "/repo/.git".into(),
            object_ids: ids.iter().map(|s| s.to_string()).collect(),
        }
    }
    #[test]
    fn existing_stashes_and_index_reordering_do_not_block() {
        assert!(!compare(&known(&["a", "b"]), &known(&["b", "a"])).blocks_done());
    }
    #[test]
    fn identifies_added_removed_and_duplicate_entries() {
        let delta = compare(&known(&["a", "b"]), &known(&["a", "a", "c"]));
        assert_eq!(delta.added_object_ids, ["a", "c"]);
        assert_eq!(delta.removed_baseline_object_ids, ["b"]);
        assert!(delta.blocks_done());
    }
    #[test]
    fn unknown_or_replaced_repository_fails_closed() {
        assert!(compare(&Inventory::Unknown, &known(&[])).blocks_done());
        assert!(compare(&known(&[]), &Inventory::NotRepository).blocks_done());
        assert!(!compare(&Inventory::NotRepository, &Inventory::NotRepository).blocks_done());
        assert!(compare(&Inventory::NotRepository, &known(&["a"])).blocks_done());
    }
    #[tokio::test]
    async fn real_inventory_preserves_stashes_and_recognizes_resolution() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let run = |args: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .args(args)
                    .current_dir(root)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        run(&["init"]);
        run(&["config", "user.email", "fixture@example.invalid"]);
        run(&["config", "user.name", "Fixture"]);
        std::fs::write(root.join("file"), "seed").unwrap();
        run(&["add", "file"]);
        run(&["commit", "-m", "seed"]);
        std::fs::write(root.join("file"), "prior").unwrap();
        run(&["stash", "push"]);
        let entry = capture(root).await;
        assert!(!compare(&entry, &capture(root).await).blocks_done());
        std::fs::write(root.join("file"), "new").unwrap();
        run(&["stash", "push"]);
        assert_eq!(
            compare(&entry, &capture(root).await).added_object_ids.len(),
            1
        );
        run(&["stash", "pop"]);
        assert!(!compare(&entry, &capture(root).await).blocks_done());
        run(&["stash", "drop"]);
        assert_eq!(
            compare(&entry, &capture(root).await)
                .removed_baseline_object_ids
                .len(),
            1
        );
        // Inventory operations never change the restored working file.
        assert_eq!(std::fs::read_to_string(root.join("file")).unwrap(), "new");
    }
}
