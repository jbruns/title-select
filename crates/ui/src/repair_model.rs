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

/// The choices to plan again with: those `plan` was asked for with (`asked`), with what you picked on it since. A
/// pick of an item's default needs no asking, as an item without a choice takes its default.
pub fn requested(
    plan: &RepairPlan,
    asked: &BTreeMap<String, RepairChoice>,
    picked: &BTreeMap<String, RepairChoice>,
) -> BTreeMap<String, RepairChoice> {
    let mut choices = asked.clone();
    for item in &plan.content.items {
        match picked.get(&item.key) {
            Some(c) if *c == item.default => {
                choices.remove(&item.key);
            }
            Some(c) => {
                choices.insert(item.key.clone(), *c);
            }
            None => {}
        }
    }
    choices
}

/// The operator additions to ask for when planning again from `plan` with `choices`: the fields of each item chosen
/// to be added as one. An item without a choice takes its default.
pub fn to_add(
    plan: &RepairPlan,
    choices: &BTreeMap<String, RepairChoice>,
    adding: &BTreeMap<String, NewAddition>,
) -> BTreeMap<String, NewAddition> {
    plan.content
        .items
        .iter()
        .filter(|item| {
            choices.get(&item.key).copied().unwrap_or(item.default) == RepairChoice::AddAddition
        })
        .filter_map(|item| Some((item.key.clone(), adding.get(&item.key)?.clone())))
        .collect()
}

/// The items you changed since `plan` was shown, by key: one you picked otherwise than the plan does, or an operator
/// addition it adds, edited. Approving approves the plan as it was made, so it waits until you plan again with them.
pub fn unplanned(
    plan: &RepairPlan,
    picked: &BTreeMap<String, RepairChoice>,
    adding: &BTreeMap<String, NewAddition>,
) -> Vec<String> {
    plan.content
        .items
        .iter()
        .filter(|item| {
            picked.get(&item.key).is_some_and(|c| *c != item.chosen)
                || (item.chosen == RepairChoice::AddAddition
                    && item
                        .addition
                        .as_ref()
                        .is_some_and(|added| adding.get(&item.key).is_some_and(|a| a != added)))
        })
        .map(|item| item.key.clone())
        .collect()
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

    fn item(
        key: &str,
        choices: &[&str],
        default: &str,
        chosen: &str,
    ) -> title_select_api::repair::RepairItem {
        serde_json::from_value(serde_json::json!({
            "key": key, "kind": "conflict", "summary": "s",
            "choices": choices, "default": default, "chosen": chosen,
        }))
        .unwrap()
    }

    #[test]
    fn a_pick_other_than_the_plans_holds_its_approval_until_it_is_planned_again() {
        let mut p = plan(1, false, false, None);
        p.content.items.push(item(
            "entry:a.zip",
            &["apply", "leave", "keep_aside"],
            "leave",
            "leave",
        ));
        let none = BTreeMap::new();
        assert!(unplanned(&p, &none, &BTreeMap::new()).is_empty());

        let apply = BTreeMap::from([("entry:a.zip".to_string(), RepairChoice::Apply)]);
        assert_eq!(unplanned(&p, &apply, &BTreeMap::new()), ["entry:a.zip"]);
        assert_eq!(requested(&p, &none, &apply), apply, "planned again with it");
        let back = BTreeMap::from([("entry:a.zip".to_string(), RepairChoice::Leave)]);
        assert!(
            unplanned(&p, &back, &BTreeMap::new()).is_empty(),
            "picked back to what the plan does"
        );
    }

    #[test]
    fn a_shown_plans_own_choice_set_back_to_its_default_holds_its_approval() {
        let mut p = plan(1, false, false, None);
        p.content.items.push(item(
            "entry:c.zip",
            &["leave", "keep_aside"],
            "leave",
            "keep_aside",
        ));
        let asked = chosen(&p);
        assert_eq!(
            asked,
            BTreeMap::from([("entry:c.zip".to_string(), RepairChoice::KeepAside)])
        );
        let leave = BTreeMap::from([("entry:c.zip".to_string(), RepairChoice::Leave)]);
        assert_eq!(unplanned(&p, &leave, &BTreeMap::new()), ["entry:c.zip"]);
        assert!(
            requested(&p, &asked, &leave).is_empty(),
            "its default needs no asking"
        );
    }

    #[test]
    fn picking_a_default_the_plan_overrode_holds_its_approval() {
        // The plan left a game whose default is to apply, as applying it would keep aside a ROM's last copy.
        let mut p = plan(1, false, false, None);
        p.content
            .items
            .push(item("game:g", &["apply", "leave"], "apply", "leave"));
        let apply = BTreeMap::from([("game:g".to_string(), RepairChoice::Apply)]);
        assert_eq!(unplanned(&p, &apply, &BTreeMap::new()), ["game:g"]);
        assert!(requested(&p, &BTreeMap::new(), &apply).is_empty());
    }

    #[test]
    fn an_addition_set_back_to_its_default_isnt_asked_for_when_planning_again() {
        let added = NewAddition {
            system: "Atari - 2600".into(),
            name: "Hack".into(),
            parent: None,
        };
        let mut p = plan(1, false, false, None);
        p.content.items.push(
            serde_json::from_value(serde_json::json!({
                "key": "entry:h.zip", "kind": "keep_aside", "summary": "s",
                "choices": ["keep_aside", "add_addition"], "default": "keep_aside", "chosen": "add_addition",
                "addition": added,
            }))
            .unwrap(),
        );
        let adding = BTreeMap::from([("entry:h.zip".to_string(), added.clone())]);
        let asked = chosen(&p);
        assert_eq!(
            to_add(&p, &requested(&p, &asked, &BTreeMap::new()), &adding),
            adding
        );
        let back = BTreeMap::from([("entry:h.zip".to_string(), RepairChoice::KeepAside)]);
        assert!(to_add(&p, &requested(&p, &asked, &back), &adding).is_empty());
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
        assert!(unplanned(&p, &choices, &same).is_empty());
        let renamed = BTreeMap::from([(
            "entry:h.zip".to_string(),
            NewAddition {
                name: "Hack 2".into(),
                ..added
            },
        )]);
        assert_eq!(unplanned(&p, &choices, &renamed), ["entry:h.zip"]);
    }
}
