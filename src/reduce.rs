use anyhow::Result;
use serde_json::Value;

use crate::json;

pub struct Reduction {
    pub budget_exhausted: bool,
}

pub fn minimize(
    document: &mut Value,
    protected: &[Value],
    budget: usize,
    mut check: impl FnMut(&Value) -> Result<bool>,
) -> Result<Reduction> {
    let mut executions = 0;
    let mut budget_exhausted = false;

    loop {
        let mut reduced = false;
        for pointer in json::removable_pointers(document) {
            let mut candidate = document.clone();
            if json::remove_pointer(&mut candidate, &pointer).is_err()
                || json::contains_any(&candidate, protected)
                || serde_json::to_vec(&candidate)?.len() >= serde_json::to_vec(document)?.len()
            {
                continue;
            }
            if executions >= budget {
                budget_exhausted = true;
                return Ok(Reduction { budget_exhausted });
            }
            executions += 1;
            if check(&candidate)? {
                *document = candidate;
                reduced = true;
                break;
            }
        }
        if !reduced {
            return Ok(Reduction { budget_exhausted });
        }
    }
}
