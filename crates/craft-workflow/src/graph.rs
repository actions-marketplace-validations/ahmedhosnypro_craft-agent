//! Graph finalization: dependants, the skip rule, the Kahn acyclicity
//! check with cycle naming, and arg reference checks.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::error::WorkflowError;
use crate::plan::{ArgSpec, OnFailure, StepSpec};

pub fn finalize_graph(file: &str, steps: &mut [StepSpec]) -> Result<(), WorkflowError> {
    check_needs(file, steps)?;
    link_dependants(steps);
    check_skip_rule(file, steps)?;
    kahn(file, steps)
}

/// Every `needs` entry must name a step in the plan.
fn check_needs(file: &str, steps: &[StepSpec]) -> Result<(), WorkflowError> {
    let ids: BTreeSet<&str> = steps.iter().map(|spec| spec.id.as_str()).collect();
    for spec in steps.iter() {
        for need in &spec.needs {
            if !ids.contains(need.as_str()) {
                return Err(WorkflowError::UnknownNeed {
                    file: file.to_string(),
                    step: spec.id.clone(),
                    need: need.clone(),
                });
            }
        }
    }
    Ok(())
}

/// Reverse edges: each step gains the ids of its dependants.
fn link_dependants(steps: &mut [StepSpec]) {
    let index: BTreeMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, spec)| (spec.id.as_str(), i))
        .collect();
    let mut edges: Vec<(usize, String)> = Vec::new();
    for spec in steps.iter() {
        for need in &spec.needs {
            if let Some(&dep) = index.get(need.as_str()) {
                edges.push((dep, spec.id.clone()));
            }
        }
    }
    for (dep, dependent) in edges {
        steps[dep].dependants.push(dependent);
    }
}

/// A skipped step must not have dependants, or the run would stall.
fn check_skip_rule(file: &str, steps: &[StepSpec]) -> Result<(), WorkflowError> {
    for spec in steps.iter() {
        if spec.on_failure == OnFailure::Skip && !spec.dependants.is_empty() {
            return Err(WorkflowError::SkipHasDependants {
                file: file.to_string(),
                step: spec.id.clone(),
                dependent: spec.dependants[0].clone(),
            });
        }
    }
    Ok(())
}

fn kahn(file: &str, steps: &[StepSpec]) -> Result<(), WorkflowError> {
    let index: BTreeMap<&str, usize> = steps
        .iter()
        .enumerate()
        .map(|(i, spec)| (spec.id.as_str(), i))
        .collect();
    let mut unmet: Vec<u32> = steps.iter().map(|spec| spec.needs.len() as u32).collect();
    let mut ready: VecDeque<usize> = unmet
        .iter()
        .enumerate()
        .filter(|(_, count)| **count == 0)
        .map(|(i, _)| i)
        .collect();
    let mut peeled = vec![false; steps.len()];
    while let Some(i) = ready.pop_front() {
        peeled[i] = true;
        for target in steps[i]
            .dependants
            .iter()
            .filter_map(|id| index.get(id.as_str()).copied())
        {
            unmet[target] = unmet[target].saturating_sub(1);
            let fresh = unmet[target] == 0 && !peeled[target];
            if fresh {
                ready.push_back(target);
            }
        }
    }
    let leftover: BTreeSet<usize> = (0..steps.len()).filter(|i| !peeled[*i]).collect();
    if leftover.is_empty() {
        return Ok(());
    }
    let path = cycle_path(steps, &index, &leftover);
    Err(WorkflowError::Cycle {
        file: file.to_string(),
        path,
    })
}

/// Walks needs inside the leftover set until a node repeats; the walk is
/// bounded by the leftover size.
fn cycle_path(
    steps: &[StepSpec],
    index: &BTreeMap<&str, usize>,
    leftover: &BTreeSet<usize>,
) -> String {
    let start = match leftover.iter().next() {
        Some(&i) => i,
        None => return String::new(),
    };
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut walked: Vec<String> = Vec::new();
    let mut cur = start;
    for _ in 0..=leftover.len() {
        let id = steps[cur].id.clone();
        if let Some(&pos) = seen.get(&id) {
            let mut cycle: Vec<&str> = walked[pos..].iter().map(|s| s.as_str()).collect();
            cycle.push(walked[pos].as_str());
            return cycle.join(" -> ");
        }
        seen.insert(id.clone(), walked.len());
        walked.push(id);
        let next = steps[cur]
            .needs
            .iter()
            .find(|need| match index.get(need.as_str()) {
                Some(&i) => leftover.contains(&i),
                None => false,
            })
            .and_then(|need| index.get(need.as_str()).copied());
        match next {
            Some(i) => cur = i,
            None => break,
        }
    }
    walked.join(" -> ")
}

/// Collects every `{{arg.NAME}}` reference in a text block.
pub fn scan_arg_refs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("{{arg.") {
        let after = &rest[start + 6..];
        if let Some(end) = after.find("}}") {
            let name = &after[..end];
            if !name.is_empty() && !out.iter().any(|known: &String| known == name) {
                out.push(name.to_string());
            }
            rest = &after[end + 2..];
        } else {
            rest = after;
        }
    }
    out
}

/// Every arg reference in the prelude and the step bodies must be declared.
pub fn check_arg_refs(
    file: &str,
    steps: &[StepSpec],
    shared_body: &str,
    args: &BTreeMap<String, ArgSpec>,
) -> Result<(), WorkflowError> {
    for name in scan_arg_refs(shared_body) {
        if !args.contains_key(&name) {
            return Err(WorkflowError::UnknownArgRef {
                file: file.to_string(),
                step: "prelude".to_string(),
                name,
            });
        }
    }
    for spec in steps {
        for name in scan_arg_refs(&spec.body) {
            if !args.contains_key(&name) {
                return Err(WorkflowError::UnknownArgRef {
                    file: file.to_string(),
                    step: spec.id.clone(),
                    name,
                });
            }
        }
    }
    Ok(())
}
