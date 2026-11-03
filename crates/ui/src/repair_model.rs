//! The repair screen's pure logic: which repair to show when it opens, when to fetch a plan again, what to say
//! about one left unfinished, what you changed since the plan shown was made, and setting one choice for many items.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use title_select_api::additions::NewAddition;
use title_select_api::repair::{RepairChoice, RepairItem, RepairPlan, RepairStep};

use crate::parts::plan_model::{self, BulkOutcome};
use crate::parts::status::PlanKind;
use crate::screens::additions::new_addition;

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

/// The additions to ask for when planning again from `plan` with `choices`: the fields of each item chosen
/// to be added as one. An item without a choice takes its default. Fields you haven't opened are what the plan adds
/// it as, or else its suggested name in `system`, the source set's DAT.
pub fn to_add(
    plan: &RepairPlan,
    choices: &BTreeMap<String, RepairChoice>,
    adding: &BTreeMap<String, NewAddition>,
    system: &str,
) -> BTreeMap<String, NewAddition> {
    plan.content
        .items
        .iter()
        .filter(|item| {
            choices.get(&item.key).copied().unwrap_or(item.default) == RepairChoice::AddAddition
        })
        .map(|item| {
            let fields = adding
                .get(&item.key)
                .or(item.addition.as_ref())
                .cloned()
                .unwrap_or_else(|| suggested_addition(item, system));
            (item.key.clone(), fields)
        })
        .collect()
}

/// What `item` is added as until you say otherwise: an addition named from its file, in `system`.
pub fn suggested_addition(item: &RepairItem, system: &str) -> NewAddition {
    let path = item.path.as_deref().unwrap_or(&item.key);
    new_addition(path, Some(system))
}

/// The items you changed since `plan` was shown, by key: one you picked otherwise than the plan does, or an
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

/// The choice `item` takes now: your pick, or else what the plan shown does.
pub fn current(item: &RepairItem, picked: &BTreeMap<String, RepairChoice>) -> RepairChoice {
    picked.get(&item.key).copied().unwrap_or(item.chosen)
}

/// The choices of `item` you can set for many items at once: adding as an addition needs each item's own name, so
/// it's set one item at a time.
pub fn bulk_offers(item: &RepairItem) -> Vec<RepairChoice> {
    item.choices
        .iter()
        .copied()
        .filter(|c| *c != RepairChoice::AddAddition)
        .collect()
}

fn of_kind(plan: &RepairPlan, kind: PlanKind) -> impl Iterator<Item = &RepairItem> {
    plan.content
        .items
        .iter()
        .filter(move |i| PlanKind::from(i.kind) == kind)
}

/// The choices "apply to all" offers for `kind`: those any item of that kind offers. One that only some offer sets
/// only those.
pub fn kind_choices(plan: &RepairPlan, kind: PlanKind) -> Vec<RepairChoice> {
    let offers: Vec<Vec<RepairChoice>> = of_kind(plan, kind).map(bulk_offers).collect();
    plan_model::offered_by_any(offers.iter().map(Vec::as_slice))
}

/// The choices you can set for the items `keys` names: those any of them offers.
pub fn choices_for(plan: &RepairPlan, keys: &[String]) -> Vec<RepairChoice> {
    let keys: BTreeSet<&str> = keys.iter().map(String::as_str).collect();
    let offers: Vec<Vec<RepairChoice>> = plan
        .content
        .items
        .iter()
        .filter(|i| keys.contains(i.key.as_str()))
        .map(bulk_offers)
        .collect();
    plan_model::offered_by_any(offers.iter().map(Vec::as_slice))
}

fn apply<'a>(
    items: impl Iterator<Item = &'a RepairItem>,
    choice: RepairChoice,
    picked: &mut BTreeMap<String, RepairChoice>,
) -> BulkOutcome {
    let offers: Vec<(&RepairItem, Vec<RepairChoice>)> =
        items.map(|i| (i, bulk_offers(i))).collect();
    plan_model::apply_choice(
        offers
            .iter()
            .map(|(i, o)| (i.key.as_str(), i.chosen, o.as_slice())),
        choice,
        picked,
    )
}

/// Sets `choice` for every item of `kind` that offers it; no choice ("no change") changes nothing.
pub fn apply_to_kind(
    plan: &RepairPlan,
    kind: PlanKind,
    choice: Option<RepairChoice>,
    picked: &mut BTreeMap<String, RepairChoice>,
) -> Option<BulkOutcome> {
    Some(apply(of_kind(plan, kind), choice?, picked))
}

/// Sets `choice` for every item `keys` names that offers it: the selected rows, or every row matching a filter.
pub fn apply_to_keys(
    plan: &RepairPlan,
    keys: &[String],
    choice: RepairChoice,
    picked: &mut BTreeMap<String, RepairChoice>,
) -> BulkOutcome {
    let keys: BTreeSet<&str> = keys.iter().map(String::as_str).collect();
    apply(
        plan.content
            .items
            .iter()
            .filter(|i| keys.contains(i.key.as_str())),
        choice,
        picked,
    )
}

/// Where `item` is now: its file, and the member in it; or, for a game it lacks, the game.
pub fn place(item: &RepairItem) -> String {
    match (&item.path, &item.member, &item.game) {
        (Some(path), Some(member), _) => format!("{path} ({member})"),
        (Some(path), None, _) => path.clone(),
        (None, _, Some(game)) => format!("{game} (missing)"),
        (None, _, None) => item.key.clone(),
    }
}

/// The steps of `plan`, in order, by the key of the item each is for: so a row finds its own without reading every
/// step of the plan.
pub fn steps_by_item(plan: &RepairPlan) -> HashMap<&str, Vec<&RepairStep>> {
    let mut by_item: HashMap<&str, Vec<&RepairStep>> = HashMap::new();
    for s in &plan.content.steps {
        by_item.entry(s.item.as_str()).or_default().push(&s.step);
    }
    by_item
}

/// What the plan makes of `item`, given its own `steps`: the file it ends up as, the addition it is added as, or
/// that it is kept aside or left as it is.
pub fn proposed(item: &RepairItem, steps: &[&RepairStep]) -> String {
    if item.chosen == RepairChoice::Leave {
        return "left as it is".to_string();
    }
    if item.chosen == RepairChoice::AddAddition
        && let Some(addition) = &item.addition
    {
        return format!("addition {}", addition.name);
    }
    let steps = || steps.iter().copied();
    let written = steps().find_map(|step| match step {
        RepairStep::Rename { to, .. } => Some(to.clone()),
        RepairStep::Publish { target, .. } | RepairStep::Fill { target, .. } => {
            Some(target.clone())
        }
        _ => None,
    });
    let written = written.or_else(|| {
        steps().find_map(|step| match step {
            RepairStep::Build { target, .. } => Some(target.clone()),
            _ => None,
        })
    });
    match written {
        Some(target) => target,
        None if steps().any(|s| matches!(s, RepairStep::Keep { .. })) => "kept files".to_string(),
        None => "left as it is".to_string(),
    }
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
            to_add(
                &p,
                &requested(&p, &asked, &BTreeMap::new()),
                &adding,
                "Atari - 2600"
            ),
            adding
        );
        let back = BTreeMap::from([("entry:h.zip".to_string(), RepairChoice::KeepAside)]);
        assert!(to_add(&p, &requested(&p, &asked, &back), &adding, "Atari - 2600").is_empty());
    }

    #[test]
    fn planning_again_adds_what_the_plan_did_or_a_suggestion_for_an_addition_never_opened() {
        let added = NewAddition {
            system: "Atari - 2600".into(),
            name: "Hack".into(),
            parent: Some("Pong (USA)".into()),
        };
        let mut p = plan(1, false, false, None);
        let mut push = |key: &str, path: &str, chosen: &str, addition: Option<&NewAddition>| {
            p.content.items.push(
                serde_json::from_value(serde_json::json!({
                    "key": key, "kind": "keep_aside", "summary": "s", "path": path,
                    "choices": ["keep_aside", "add_addition"], "default": "keep_aside", "chosen": chosen,
                    "addition": addition,
                }))
                .unwrap(),
            );
        };
        push("entry:h.zip", "h.zip", "add_addition", Some(&added));
        push("entry:My Demo.zip", "My Demo.zip", "keep_aside", None);
        let picked = BTreeMap::from([("entry:My Demo.zip".to_string(), RepairChoice::AddAddition)]);
        let choices = requested(&p, &chosen(&p), &picked);
        let asked = to_add(&p, &choices, &BTreeMap::new(), "Atari - 2600");
        assert_eq!(asked["entry:h.zip"], added, "what the plan adds it as");
        let suggested = &asked["entry:My Demo.zip"];
        assert_eq!(
            (suggested.system.as_str(), suggested.name.as_str()),
            ("Atari - 2600", "My Demo"),
            "a suggestion in the source set's DAT"
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

    fn bulk_plan() -> RepairPlan {
        let mut p = plan(1, false, false, None);
        let kept = |key: &str, choices: &[&str]| {
            let mut i = item(key, choices, "keep_aside", "keep_aside");
            i.kind = title_select_api::repair::RepairKind::KeepAside;
            i
        };
        p.content.items = vec![
            kept("entry:a.zip", &["leave", "keep_aside", "add_addition"]),
            kept("entry:b.zip", &["leave", "keep_aside", "add_addition"]),
            kept("entry:c.zip", &["keep_aside"]),
            item("game:g", &["apply", "leave"], "apply", "apply"),
        ];
        p
    }

    #[test]
    fn apply_to_all_offers_what_any_item_of_the_kind_offers_but_adding() {
        let p = bulk_plan();
        assert_eq!(
            kind_choices(&p, PlanKind::SetAside),
            [RepairChoice::Leave, RepairChoice::KeepAside]
        );
        let ab = ["entry:a.zip".to_string(), "entry:b.zip".to_string()];
        assert_eq!(
            choices_for(&p, &ab),
            [RepairChoice::Leave, RepairChoice::KeepAside]
        );
        assert_eq!(
            kind_choices(&p, PlanKind::Rename),
            [RepairChoice::Apply, RepairChoice::Leave],
            "a conflict is shown as a rename"
        );
    }

    #[test]
    fn no_change_leaves_every_pick_as_it_was() {
        let p = bulk_plan();
        let mut picked = BTreeMap::from([("entry:a.zip".to_string(), RepairChoice::Leave)]);
        let before = picked.clone();
        assert_eq!(
            apply_to_kind(&p, PlanKind::SetAside, None, &mut picked),
            None
        );
        assert_eq!(picked, before);
    }

    #[test]
    fn apply_to_all_sets_every_item_of_the_kind_and_only_that_kind() {
        let mut p = bulk_plan();
        p.content.items.truncate(2);
        p.content
            .items
            .push(item("game:g", &["apply", "leave"], "apply", "apply"));
        let mut picked = BTreeMap::new();
        let outcome = apply_to_kind(
            &p,
            PlanKind::SetAside,
            Some(RepairChoice::Leave),
            &mut picked,
        )
        .unwrap();
        assert_eq!(outcome.changed, 2);
        assert_eq!(
            picked.keys().collect::<Vec<_>>(),
            ["entry:a.zip", "entry:b.zip"]
        );
        assert_eq!(
            unplanned(&p, &picked, &BTreeMap::new()).len(),
            2,
            "approval waits"
        );
        picked.insert("entry:a.zip".into(), RepairChoice::KeepAside);
        assert_eq!(
            unplanned(&p, &picked, &BTreeMap::new()),
            ["entry:b.zip"],
            "one item can still be changed afterwards"
        );
    }

    #[test]
    fn a_choice_only_some_items_offer_sets_only_those_and_says_so() {
        let p = bulk_plan();
        let mut picked = BTreeMap::new();
        let keys: Vec<String> = p.content.items.iter().map(|i| i.key.clone()).collect();
        let outcome = apply_to_keys(&p, &keys, RepairChoice::Leave, &mut picked);
        assert_eq!(
            outcome,
            BulkOutcome {
                asked: 4,
                offered: 3,
                changed: 3
            }
        );
        assert!(!picked.contains_key("entry:c.zip"));
        assert_eq!(
            current(&p.content.items[2], &picked),
            RepairChoice::KeepAside
        );
        assert!(outcome.message("leave").contains("1 item doesn't offer it"));
    }

    #[test]
    fn adding_as_an_addition_is_never_set_in_bulk() {
        let p = bulk_plan();
        let mut picked = BTreeMap::new();
        let keys = ["entry:a.zip".to_string()];
        let outcome = apply_to_keys(&p, &keys, RepairChoice::AddAddition, &mut picked);
        assert_eq!(outcome.offered, 0);
        assert!(picked.is_empty());
    }

    fn seen() -> title_select_api::repair::SeenFile {
        use title_select_api::build::{FileIdentity, FileStat};
        title_select_api::repair::SeenFile {
            identity: FileIdentity { dev: 1, ino: 2 },
            file: FileStat {
                size: 3,
                mtime_ns: 4,
            },
            ctime_ns: 5,
        }
    }

    #[test]
    fn a_row_reads_where_the_item_is_and_what_the_plan_makes_of_it() {
        use title_select_api::repair::PlannedStep;
        let mut p = plan(1, false, false, None);
        let mut renamed = item("entry:a.zip", &["apply", "leave"], "apply", "apply");
        renamed.path = Some("a.zip".into());
        let mut repacked = item("entry:b.zip#b.bin", &["apply", "leave"], "apply", "apply");
        repacked.path = Some("b.zip".into());
        repacked.member = Some("b.bin".into());
        let mut kept = item(
            "entry:c.txt",
            &["keep_aside", "leave"],
            "keep_aside",
            "keep_aside",
        );
        kept.path = Some("c.txt".into());
        let mut left = item("entry:d.zip", &["apply", "leave"], "apply", "leave");
        left.path = Some("d.zip".into());
        let mut filled = item("game:Echo", &["apply", "leave"], "apply", "apply");
        filled.game = Some("Echo".into());
        p.content.steps = vec![
            PlannedStep {
                item: "entry:a.zip".into(),
                step: RepairStep::Rename {
                    from: "a.zip".into(),
                    to: "Alpha.zip".into(),
                    seen: seen(),
                    roms: vec![],
                    loose: false,
                },
            },
            PlannedStep {
                item: "entry:b.zip#b.bin".into(),
                step: RepairStep::Build {
                    target: "Bravo.zip".into(),
                    roms: vec![],
                    sources: BTreeMap::new(),
                    loose: false,
                },
            },
            PlannedStep {
                item: "entry:c.txt".into(),
                step: RepairStep::Keep {
                    path: "c.txt".into(),
                    kept_name: "c.txt".into(),
                    reason: "unmatched".into(),
                    seen: seen(),
                    needs: vec![],
                    affects: vec![],
                },
            },
        ];
        let by_item = steps_by_item(&p);
        let rows: Vec<(String, String)> = [&renamed, &repacked, &kept, &left, &filled]
            .into_iter()
            .map(|i| {
                let own = by_item.get(i.key.as_str()).map_or(&[][..], Vec::as_slice);
                (place(i), proposed(i, own))
            })
            .collect();
        assert_eq!(
            rows,
            [
                ("a.zip".into(), "Alpha.zip".into()),
                ("b.zip (b.bin)".into(), "Bravo.zip".into()),
                ("c.txt".into(), "kept files".into()),
                ("d.zip".into(), "left as it is".into()),
                ("Echo (missing)".into(), "left as it is".into()),
            ]
        );
    }
}
