use std::collections::HashSet;

use anyhow::Result;
use serde_json::{Map, Value};

/// Verdict for a single candidate offered to delta debugging.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Test {
    /// The configured target failure still happens.
    Preserved,
    /// The candidate is evaluable but no longer reproduces the target failure.
    NotPreserved,
    /// The candidate could not be evaluated, for instance because the adapter
    /// failed to run on it. Never accepted, and worth shrinking into on its own:
    /// that is how a shape the adapter cannot handle gets narrowed down.
    Incompatible,
}

pub struct Reduction {
    pub budget_exhausted: bool,
    pub attempts: usize,
    /// Candidates the adapter could not evaluate. They were skipped, never kept.
    pub untestable: usize,
}

/// A removable child of a container: an object property or an array element.
/// Also used to address a container from the document root.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum Slot {
    Key(String),
    Index(usize),
}

enum Step {
    /// The container shrank; the caller restarts with fresh slots.
    Reduced,
    NoChange,
    Exhausted,
}

struct State<'t, F> {
    test: &'t mut F,
    attempts: usize,
    untestable: usize,
    budget: usize,
}

impl<F> State<'_, F> {
    fn out_of_budget(&self) -> bool {
        self.attempts >= self.budget
    }
}

/// Shrinks `document` structurally while preserving the target failure.
///
/// Reduction is delta debugging over each container in turn: children are cut in
/// groups whose granularity doubles until a group can go, so dropping N of M
/// elements costs O(log N) oracle runs instead of one run per element. The test
/// callback always receives a complete document, never a fragment.
pub fn minimize<F>(document: &mut Value, budget: usize, mut test: F) -> Result<Reduction>
where
    F: FnMut(&Value) -> Result<Test>,
{
    let mut state = State {
        test: &mut test,
        attempts: 0,
        untestable: 0,
        budget,
    };
    let budget_exhausted = reduce_container(document, &[], &mut state)?;
    Ok(Reduction {
        budget_exhausted,
        attempts: state.attempts,
        untestable: state.untestable,
    })
}

/// Reduces the container at `path`, then recurses into whatever it still holds.
fn reduce_container<F>(
    document: &mut Value,
    path: &[Slot],
    state: &mut State<'_, F>,
) -> Result<bool>
where
    F: FnMut(&Value) -> Result<Test>,
{
    loop {
        let slots = child_slots(container(document, path)?);
        if slots.is_empty() {
            break;
        }
        match ddmin(document, path, &slots, 2, state)? {
            Step::Reduced => continue,
            Step::NoChange => break,
            Step::Exhausted => return Ok(true),
        }
    }
    // The container is stable again, so its children can be addressed directly.
    for slot in child_slots(container(document, path)?) {
        let mut child_path = path.to_vec();
        child_path.push(slot);
        if reduce_container(document, &child_path, state)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// One delta debugging level: cut `parts` groups out of `slots`, then halve the
/// surviving group until nothing more can go.
fn ddmin<F>(
    document: &mut Value,
    path: &[Slot],
    slots: &[Slot],
    parts: usize,
    state: &mut State<'_, F>,
) -> Result<Step>
where
    F: FnMut(&Value) -> Result<Test>,
{
    let total = slots.len();
    if total == 0 {
        return Ok(Step::NoChange);
    }
    // A single slot still gets tested: removing the last child is the empty-set
    // step that classic delta debugging would otherwise never reach.
    let parts = parts.min(total);
    let mut untestable: Vec<Vec<Slot>> = Vec::new();

    for group in partition(total, parts) {
        let keep = &slots[group.clone()];
        // Dropping the group, then keeping only the group, covers both halves of
        // the classic subset/complement pair within a single level.
        let complement = complement_slots(slots, group.clone());
        for remove in [keep.to_vec(), complement] {
            if remove.is_empty() {
                continue;
            }
            if state.out_of_budget() {
                return Ok(Step::Exhausted);
            }
            let candidate = candidate_with(document, path, &remove)?;
            // Every accepted step must actually shed bytes, so reduction can
            // never be blamed for growing the artifact.
            if serialized_len(&candidate) >= serialized_len(document) {
                continue;
            }
            state.attempts += 1;
            match (state.test)(&candidate)? {
                Test::Preserved => {
                    replace_container(document, path, &remove)?;
                    return Ok(Step::Reduced);
                }
                Test::NotPreserved => {}
                Test::Incompatible => {
                    state.untestable += 1;
                    // A group no finer than the one just tried cannot make
                    // progress, so recording it would recurse forever.
                    if keep.len() < total {
                        untestable.push(keep.to_vec());
                    }
                }
            }
        }
    }

    // A group the adapter could not evaluate may still hold removable data, so
    // it gets its own pass before the granularity widens.
    for keep in untestable {
        match ddmin(document, path, &keep, 2, state)? {
            Step::Reduced => return Ok(Step::Reduced),
            Step::Exhausted => return Ok(Step::Exhausted),
            Step::NoChange => {}
        }
    }

    if parts >= total {
        return Ok(Step::NoChange);
    }
    ddmin(document, path, slots, parts.saturating_mul(2), state)
}

fn container<'a>(document: &'a Value, path: &[Slot]) -> Result<&'a Value> {
    let mut node = document;
    for slot in path {
        node = match (node, slot) {
            (Value::Object(map), Slot::Key(key)) => map
                .get(key)
                .ok_or_else(|| anyhow::anyhow!("candidate path became invalid"))?,
            (Value::Array(items), Slot::Index(index)) => items
                .get(*index)
                .ok_or_else(|| anyhow::anyhow!("candidate path became invalid"))?,
            _ => return Err(anyhow::anyhow!("candidate path became invalid")),
        };
    }
    Ok(node)
}

fn container_mut<'a>(document: &'a mut Value, path: &[Slot]) -> Result<&'a mut Value> {
    let mut node = document;
    for slot in path {
        node = match (node, slot) {
            (Value::Object(map), Slot::Key(key)) => map
                .get_mut(key)
                .ok_or_else(|| anyhow::anyhow!("candidate path became invalid"))?,
            (Value::Array(items), Slot::Index(index)) => items
                .get_mut(*index)
                .ok_or_else(|| anyhow::anyhow!("candidate path became invalid"))?,
            _ => return Err(anyhow::anyhow!("candidate path became invalid")),
        };
    }
    Ok(node)
}

/// Builds the full document with `remove` taken out of the container at `path`.
fn candidate_with(document: &Value, path: &[Slot], remove: &[Slot]) -> Result<Value> {
    let mut candidate = document.clone();
    let node = container_mut(&mut candidate, path)?;
    let pruned = prune(node, remove);
    *node = pruned;
    Ok(candidate)
}

fn replace_container(document: &mut Value, path: &[Slot], remove: &[Slot]) -> Result<()> {
    let node = container_mut(document, path)?;
    let pruned = prune(node, remove);
    *node = pruned;
    Ok(())
}

fn serialized_len(value: &Value) -> usize {
    serde_json::to_vec(value).map_or(0, |bytes| bytes.len())
}

fn child_slots(node: &Value) -> Vec<Slot> {
    match node {
        Value::Object(map) => map.keys().map(|key| Slot::Key(key.clone())).collect(),
        Value::Array(items) => (0..items.len()).map(Slot::Index).collect(),
        _ => Vec::new(),
    }
}

/// Returns the container without the listed direct children.
fn prune(node: &Value, remove: &[Slot]) -> Value {
    if remove.is_empty() {
        return node.clone();
    }
    let removed: HashSet<&Slot> = remove.iter().collect();
    match node {
        Value::Object(map) => {
            let mut pruned = Map::new();
            for (key, value) in map {
                if !removed.contains(&Slot::Key(key.clone())) {
                    pruned.insert(key.clone(), value.clone());
                }
            }
            Value::Object(pruned)
        }
        Value::Array(items) => Value::Array(
            items
                .iter()
                .enumerate()
                .filter(|(index, _)| !removed.contains(&Slot::Index(*index)))
                .map(|(_, value)| value.clone())
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Splits `total` slots into `parts` contiguous groups of near-equal size.
fn partition(total: usize, parts: usize) -> Vec<std::ops::Range<usize>> {
    let size = total / parts;
    let extra = total % parts;
    let mut groups = Vec::with_capacity(parts);
    let mut start = 0;
    for part in 0..parts {
        let len = size + usize::from(part < extra);
        groups.push(start..start + len);
        start += len;
    }
    groups
}

fn complement_slots(slots: &[Slot], group: std::ops::Range<usize>) -> Vec<Slot> {
    let mut complement = Vec::with_capacity(slots.len() - group.len());
    complement.extend_from_slice(&slots[..group.start]);
    complement.extend_from_slice(&slots[group.end..]);
    complement
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Keeps a document only while it holds `records` with at least two entries
    /// whose `id` values collide, mirroring the importer fixture. A record
    /// without an `id` is reported as untestable, the way a real adapter behaves.
    fn importer(value: &Value) -> Test {
        let Some(records) = value.get("records").and_then(Value::as_array) else {
            return Test::Incompatible;
        };
        let ids: Vec<_> = records
            .iter()
            .filter_map(|record| record.get("id").and_then(Value::as_str))
            .collect();
        if ids.len() != records.len() {
            return Test::Incompatible;
        }
        let mut unique = ids.clone();
        unique.sort_unstable();
        unique.dedup();
        if unique.len() == ids.len() {
            Test::NotPreserved
        } else {
            Test::Preserved
        }
    }

    fn run(document: &mut Value, budget: usize) -> Reduction {
        minimize(document, budget, |candidate| Ok(importer(candidate))).expect("reduction runs")
    }

    /// Builds `count` distinct records whose first two share an identifier.
    /// Putting the collision first is what forces reduction to probe individual
    /// fields, which is where an untestable candidate comes from.
    fn records_with_a_collision(count: usize) -> Vec<Value> {
        let mut records: Vec<_> = (0..count)
            .map(|index| json!({ "id": format!("user-{index}") }))
            .collect();
        let repeated = records[1]["id"].clone();
        records[0]["id"] = repeated;
        records
    }

    #[test]
    fn removes_many_array_elements_with_few_runs() {
        let mut document = json!({ "batch": "nightly", "records": records_with_a_collision(500) });
        let reduction = run(&mut document, 500);

        assert_eq!(document["records"].as_array().unwrap().len(), 2);
        // Group removal is the point: 500 elements cannot be cut in 500 runs.
        assert!(
            reduction.attempts < 60,
            "expected log-shaped reduction, used {} runs",
            reduction.attempts
        );
        assert!(!reduction.budget_exhausted);
    }

    #[test]
    fn removes_noise_properties_and_top_level_keys() {
        let mut document = json!({
            "version": 1,
            "batch": "nightly",
            "records": [
                { "id": "user-0", "name": "zero", "email": "z@example.com" },
                { "id": "user-0", "name": "zero", "email": "z@example.com" }
            ]
        });
        run(&mut document, 500);
        assert_eq!(
            document,
            json!({ "records": [{ "id": "user-0" }, { "id": "user-0" }] })
        );
    }

    #[test]
    fn preserves_declaration_order_of_surviving_keys() {
        // The oracle needs both `kind` and `id`, so two keys survive and their
        // order is observable. Alphabetical order would be `id`, `kind`.
        let mut document = json!({
            "zebra": 1,
            "batch": "nightly",
            "records": [
                { "kind": "user", "zid": "a", "id": "a", "note": "x" },
                { "kind": "user", "zid": "a", "id": "a", "note": "x" }
            ]
        });
        let reduction = minimize(&mut document, 500, |candidate| {
            let Some(records) = candidate.get("records").and_then(Value::as_array) else {
                return Ok(Test::Incompatible);
            };
            if candidate.get("batch").is_none()
                || candidate.get("zebra").is_none()
                || records
                    .iter()
                    .any(|record| record.get("id").is_none() || record.get("kind").is_none())
            {
                return Ok(Test::Incompatible);
            }
            Ok(importer(candidate))
        })
        .expect("reduction runs");

        assert!(!reduction.budget_exhausted);
        // Alphabetical order would be batch, records, zebra.
        let top: Vec<_> = document.as_object().unwrap().keys().cloned().collect();
        assert_eq!(top, ["zebra", "batch", "records"]);
        // Alphabetical order would be id, kind.
        let record: Vec<_> = document["records"][0]
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect();
        assert_eq!(record, ["kind", "id"]);
    }

    #[test]
    fn shrinks_nested_containers() {
        let mut document = json!({
            "meta": { "keep": 1, "drop": 2, "deep": { "x": 1, "y": 2 } },
            "records": [
                { "id": "a", "tags": ["t1", "t2", "t3"] },
                { "id": "a", "tags": ["t1", "t2", "t3"] }
            ]
        });
        run(&mut document, 500);

        // The `meta` subtree is not needed by the failure at all.
        assert!(document.get("meta").is_none());
        let record = document["records"][0].as_object().unwrap();
        assert!(record.contains_key("id"));
        assert!(
            record
                .get("tags")
                .is_none_or(|tags| tags.as_array().unwrap().len() < 3)
        );
    }

    #[test]
    fn shrinks_into_a_group_the_adapter_cannot_evaluate() {
        // A record without an `id` is untestable, so reduction must narrow the
        // shape down instead of throwing the run away.
        let mut document = json!({ "records": records_with_a_collision(40) });
        let reduction = run(&mut document, 500);

        assert!(
            reduction.untestable > 0,
            "the fixture must exercise the untestable path"
        );
        let survivors = document["records"].as_array().unwrap();
        assert_eq!(survivors.len(), 2);
        // Whatever survived still carries the field the adapter reads.
        assert!(survivors.iter().all(|record| record.get("id").is_some()));
    }

    #[test]
    fn stops_at_the_budget_without_claiming_progress() {
        let mut document = json!({ "records": records_with_a_collision(200) });
        let before = document.clone();
        let reduction = run(&mut document, 0);

        assert!(reduction.budget_exhausted);
        assert_eq!(reduction.attempts, 0);
        assert_eq!(document, before);
    }

    #[test]
    fn reaches_the_smallest_document_the_oracle_accepts() {
        let mut document = json!({ "records": records_with_a_collision(50) });
        let before = serialized_len(&document);
        let reduction = minimize(&mut document, 200, |_| Ok(Test::Preserved)).expect("runs");

        // Nothing is required, so reduction must reach the empty document rather
        // than stopping at the last element it cannot halve any further.
        assert_eq!(document, json!({}));
        assert!(serialized_len(&document) < before);
        assert!(reduction.attempts > 0);
        assert!(!reduction.budget_exhausted);
    }

    #[test]
    fn an_unreachable_failure_leaves_the_document_alone() {
        let mut document = json!({ "records": records_with_a_collision(20) });
        let before = document.clone();
        let reduction = minimize(&mut document, 200, |_| Ok(Test::NotPreserved)).expect("runs");

        assert_eq!(document, before);
        assert!(!reduction.budget_exhausted);
    }

    #[test]
    fn partition_covers_every_slot_exactly_once() {
        for total in 1..=17_usize {
            for parts in 2..=total {
                let groups = partition(total, parts);
                let covered: usize = groups.iter().map(std::iter::ExactSizeIterator::len).sum();
                assert_eq!(covered, total, "total={total} parts={parts}");
                let mut flattened: Vec<usize> =
                    groups.iter().flat_map(|range| range.clone()).collect();
                flattened.sort_unstable();
                assert_eq!(flattened, (0..total).collect::<Vec<_>>());
            }
        }
    }

    #[test]
    fn prune_keeps_order_and_touches_only_the_listed_children() {
        let node = json!({ "b": 1, "a": 2, "c": 3 });
        let pruned = prune(&node, &[Slot::Key("a".to_owned())]);
        assert_eq!(pruned, json!({ "b": 1, "c": 3 }));

        let list = json!([10, 20, 30, 40]);
        assert_eq!(
            prune(&list, &[Slot::Index(0), Slot::Index(2)]),
            json!([20, 40])
        );
        assert_eq!(prune(&list, &[]), list);
    }
}
