//! The repair screen's pure logic: which repair to show when it opens, when to fetch a plan again, what to say
//! about one left unfinished, and what you changed since the plan shown was made.

use std::collections::BTreeMap;

use title_select_api::additions::NewAddition;
use title_select_api::repair::{RepairChoice, RepairPlan};

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

/// The choices `plan` was made with that differ from each item's default, by key: what the screen starts from when
/// it shows a plan it didn't make, so that setting one back to its default is a change.
pub fn chosen(plan: &RepairPlan) -> BTreeMap<String, RepairChoice> {
    plan.content
        .items
        .iter()
        .filter(|item| item.chosen != item.default)
        .map(|item| (item.key.clone(), item.chosen))
        .collect()
}

/// The items you changed since `plan` was made, by key: a choice other than the ones it was planned with
/// (`planned`), or an operator addition it adds, edited. Approving approves the plan as it was made, so it waits
/// until you plan again with them.
pub fn unplanned(
    plan: &RepairPlan,
    planned: &BTreeMap<String, RepairChoice>,
    choices: &BTreeMap<String, RepairChoice>,
    adding: &BTreeMap<String, NewAddition>,
) -> Vec<String> {
    let mut keys: Vec<String> = planned
        .keys()
        .chain(choices.keys())
        .filter(|k| planned.get(*k) != choices.get(*k))
        .cloned()
        .collect();
    keys.extend(plan.content.items.iter().filter_map(|item| {
        let added = item.addition.as_ref()?;
        (item.chosen == RepairChoice::AddAddition
            && adding.get(&item.key).is_some_and(|a| a != added))
        .then(|| item.key.clone())
    }));
    keys.sort();
    keys.dedup();
    keys
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

    #[test]
    fn a_choice_changed_since_the_plan_was_made_holds_its_approval_until_it_is_planned_again() {
        let p = plan(1, false, false, None);
        let none = BTreeMap::new();
        assert!(unplanned(&p, &none, &none, &BTreeMap::new()).is_empty());

        let changed = BTreeMap::from([("entry:a.zip".to_string(), RepairChoice::Apply)]);
        assert_eq!(
            unplanned(&p, &none, &changed, &BTreeMap::new()),
            ["entry:a.zip"],
            "a choice made after the plan isn't in it"
        );
        assert!(
            unplanned(&p, &changed, &changed, &BTreeMap::new()).is_empty(),
            "planned again with it"
        );
        assert_eq!(
            unplanned(&p, &changed, &none, &BTreeMap::new()),
            ["entry:a.zip"],
            "set back to its default after planning with it"
        );
    }

    #[test]
    fn a_shown_plans_own_choice_set_back_to_its_default_holds_its_approval() {
        let mut p = plan(1, false, false, None);
        p.content.items.push(
            serde_json::from_value(serde_json::json!({
                "key": "entry:c.zip", "kind": "conflict", "summary": "s",
                "choices": ["leave", "keep_aside"], "default": "leave", "chosen": "keep_aside",
            }))
            .unwrap(),
        );
        let planned = chosen(&p);
        assert_eq!(
            planned,
            BTreeMap::from([("entry:c.zip".to_string(), RepairChoice::KeepAside)])
        );
        assert!(unplanned(&p, &planned, &planned, &BTreeMap::new()).is_empty());
        assert_eq!(
            unplanned(&p, &planned, &BTreeMap::new(), &BTreeMap::new()),
            ["entry:c.zip"]
        );
    }

    #[test]
    fn an_addition_edited_since_the_plan_was_made_holds_its_approval() {
        let added = NewAddition {
            system: "Atari - 2600".into(),
            name: "Hack".into(),
            parent: None,
        };
        let mut p = plan(1, false, false, None);
        p.content.items.push(
            serde_json::from_value(serde_json::json!({
                "key": "entry:h.zip", "kind": "keep_aside", "summary": "s",
                "choices": ["leave", "add_addition"], "default": "leave", "chosen": "add_addition",
                "addition": added,
            }))
            .unwrap(),
        );
        let choices = BTreeMap::from([("entry:h.zip".to_string(), RepairChoice::AddAddition)]);
        let same = BTreeMap::from([("entry:h.zip".to_string(), added.clone())]);
        assert!(unplanned(&p, &choices, &choices, &same).is_empty());
        let renamed = BTreeMap::from([(
            "entry:h.zip".to_string(),
            NewAddition {
                name: "Hack 2".into(),
                ..added
            },
        )]);
        assert_eq!(unplanned(&p, &choices, &choices, &renamed), ["entry:h.zip"]);
    }
}
