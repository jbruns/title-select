//! The repair screen's pure logic: which repair to show when it opens, when to fetch a plan again, and what to say
//! about one left unfinished.

use title_select_api::repair::RepairPlan;

/// The repair to show when the repair screen opens, from the source set's plans: the one `wanted` names, if any;
/// otherwise the latest approved repair that hasn't finished, which is shown instead of proposing a new one.
pub fn repair_to_show(plans: &[RepairPlan], wanted: Option<i64>) -> Option<&RepairPlan> {
    match wanted {
        Some(id) => plans.iter().find(|p| p.id == id),
        None => plans
            .iter()
            .filter(|p| p.approval.is_some() && p.finished_at.is_none())
            .max_by_key(|p| p.id),
    }
}

/// Whether to fetch the plan again once its job `job` is seen to end: only while the plan shown is unfinished, and
/// only once for each job, so a repair left unfinished isn't fetched over and over. `fetched` is the job the plan
/// was last fetched for.
pub fn fetch_after(job: i64, ended: bool, unfinished: bool, fetched: Option<i64>) -> bool {
    ended && unfinished && fetched != Some(job)
}

/// What to tell you about an approved repair left unfinished: what stopped it, and that it's tried again when the
/// service starts.
pub fn unfinished_note(plan: &RepairPlan) -> Option<String> {
    if plan.approval.is_none() || plan.finished_at.is_some() {
        return None;
    }
    let why = plan.recovery_error.as_deref()?.trim_end_matches('.');
    Some(format!(
        "This repair isn't finished. What stopped it: {why}. Title Select tries it again when the service \
         starts, and until it finishes, the source set's audits and builds wait for it. Fix what's blocking it, \
         then restart the service."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(id: i64, approved: bool, finished: bool, error: Option<&str>) -> RepairPlan {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "digest": "d",
            "created_at": "2026-01-01T00:00:00Z",
            "content": {
                "source_set": "Alpha", "set_id": 1, "root": "sets", "path": "Alpha", "audit": 1,
                "dat_revision": "r", "generation": 1, "creates_marker": false,
                "folder": {"dev": 1, "ino": 2}, "items": [], "steps": [], "fingerprint": []
            },
            "approval": approved.then(|| serde_json::json!({"digest": "d", "actor": "Operator", "at": "t"})),
            "finished_at": finished.then_some("2026-01-02T00:00:00Z"),
            "recovery_error": error,
        }))
        .unwrap()
    }

    #[test]
    fn the_screen_opens_on_the_unfinished_approved_repair_rather_than_a_new_proposal() {
        let plans = [
            plan(4, false, false, None),
            plan(3, true, false, Some("stuck")),
            plan(2, true, true, None),
        ];
        assert_eq!(repair_to_show(&plans, None).map(|p| p.id), Some(3));
        assert_eq!(repair_to_show(&plans[..1], None).map(|p| p.id), None);
        assert_eq!(repair_to_show(&plans, Some(2)).map(|p| p.id), Some(2));
    }

    #[test]
    fn a_plan_left_unfinished_is_fetched_once_per_job() {
        assert!(fetch_after(7, true, true, None));
        assert!(
            !fetch_after(7, true, true, Some(7)),
            "fetched again in a loop"
        );
        assert!(
            fetch_after(8, true, true, Some(7)),
            "a later job's end is fetched"
        );
        assert!(!fetch_after(7, false, true, None));
        assert!(!fetch_after(7, true, false, None));
    }

    #[test]
    fn an_unfinished_repair_says_what_blocks_it_and_that_it_is_retried_at_start() {
        let note = unfinished_note(&plan(3, true, false, Some("x could not be deleted"))).unwrap();
        assert!(note.contains("x could not be deleted"), "{note}");
        assert!(note.contains("when the service"), "{note}");
        assert_eq!(unfinished_note(&plan(3, true, true, Some("x"))), None);
        assert_eq!(unfinished_note(&plan(3, false, false, None)), None);
    }
}
