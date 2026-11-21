//! The Profile editor's tab for an Arcade DAT's system (ADR 0027): the arcade selector's settings in place of the Retool
//! settings, which an Arcade DAT doesn't read.

use std::collections::BTreeSet;

use leptos::prelude::*;
use title_select_api::arcade::{
    ArcadeSettings, CONTROL_TYPES, Category, DriverStatus, ElectionStep, LANGUAGES, Orientation,
    REGIONS, Range, Variant,
};
use title_select_api::selection::SelectionProfile;

use crate::arcade_model::{
    ArcadeSource, category_label, driver_status_label, reset_arcade, update_arcade,
    use_global_arcade, variant_label,
};
use crate::screens::profiles::{LineEditor, OrderedList};

type Read<T> = fn(&ArcadeSettings) -> T;
type Write<T> = fn(&mut ArcadeSettings, T);

/// A system tab of an Arcade DAT: its arcade settings, from the global ones or set for this system.
#[component]
pub fn ArcadeSettingsForm(profile: RwSignal<SelectionProfile>, system: String) -> impl IntoView {
    let s = StoredValue::new(system.clone());
    let source = Memo::new(move |_| profile.with(|p| ArcadeSource::of(p, &s.get_value())));
    let choices = move || profile.with(|p| p.choices.get(&s.get_value()).map_or(0, |c| c.len()));
    let settings = Memo::new(move |_| profile.with(|p| p.arcade_settings(&s.get_value())));
    let get = move |f: fn(&ArcadeSettings) -> Vec<String>| Signal::derive(move || settings.with(f));
    let change = move |f: Box<dyn FnOnce(&mut ArcadeSettings)>| {
        profile.update(|p| update_arcade(p, &s.get_value(), f))
    };
    view! {
        <div class="settings" role="tabpanel" aria-label=format!("Arcade settings for {system}")>
            <p class="lede">
                "This system's DAT is an arcade DAT, so the arcade selection chooses one machine per family with these settings. "
                {move || match source.get() {
                    ArcadeSource::Defaults => "Every setting is the default. Change one to set it for this system.",
                    ArcadeSource::Global => {
                        "Every setting comes from your global arcade settings. Change one to set them all for this system."
                    }
                    ArcadeSource::System => "These settings are set for this system and replace your global ones.",
                }}
            </p>
            <p class="source">
                <span class="tag">{move || source.get().label()}</span>
            </p>
            <div class="actions">
                <button
                    type="button"
                    disabled=move || source.get() != ArcadeSource::System
                    on:click=move |_| profile.update(|p| use_global_arcade(p, &s.get_value()))
                >
                    "Use global settings for this system"
                </button>
                <button type="button" on:click=move |_| profile.update(|p| reset_arcade(p, &s.get_value()))>
                    "Reset to defaults for this system"
                </button>
                <button
                    type="button"
                    disabled=move || choices() == 0
                    on:click=move |_| profile.update(|p| p.reset_choices(&s.get_value(), None))
                >
                    {move || format!("Reset to default choices for this system ({})", choices())}
                </button>
            </div>
            <fieldset class="setting">
                <legend>"Leave out"</legend>
                <p class="note small">
                    "BIOSes, devices and machines MAME can't run on their own are always left out. A machine whose DAT entry doesn't say is kept."
                </p>
                <ul class="checks columns">
                    <Check settings change label="Mechanical machines" get=|a| a.drop_mechanical set=|a, on| a.drop_mechanical = on />
                    <Check settings change label="Machines with no screen" get=|a| a.drop_screenless set=|a, on| a.drop_screenless = on />
                    <Check
                        settings
                        change
                        label="Machines with software lists: computers and consoles"
                        get=|a| a.drop_software_lists
                        set=|a, on| a.drop_software_lists = on
                    />
                    <Check settings change label="Machines that need disks" get=|a| a.drop_disks set=|a, on| a.drop_disks = on />
                    <Check settings change label="Machines that play samples" get=|a| a.drop_samples set=|a, on| a.drop_samples = on />
                </ul>
                <p class="stacked">"Categories"</p>
                <p class="note small">
                    "Read from what the ListXML says about each machine: its driver's folder, software lists and controls."
                </p>
                <ul class="checks columns">
                    {Category::ALL
                        .into_iter()
                        .map(|c| {
                            view! {
                                <SetCheck
                                    label=category_label(c)
                                    has=Signal::derive(move || settings.with(|a| a.drop_categories.contains(&c)))
                                    toggle=Callback::new(move |on| change(Box::new(move |a| toggle(&mut a.drop_categories, c, on))))
                                />
                            }
                        })
                        .collect_view()}
                </ul>
                <p class="stacked">"Variants"</p>
                <ul class="checks columns">
                    {Variant::ALL
                        .into_iter()
                        .map(|v| {
                            view! {
                                <SetCheck
                                    label=variant_label(v)
                                    has=Signal::derive(move || settings.with(|a| a.drop_variants.contains(&v)))
                                    toggle=Callback::new(move |on| change(Box::new(move |a| toggle(&mut a.drop_variants, v, on))))
                                />
                            }
                        })
                        .collect_view()}
                </ul>
                <label class="stacked" for="a-driver">"Driver status"</label>
                <select
                    id="a-driver"
                    prop:value=move || settings.with(|a| status_value(a.minimum_driver_status))
                    on:change=move |ev| {
                        let status = status_of(&event_target_value(&ev));
                        change(Box::new(move |a| a.minimum_driver_status = status));
                    }
                >
                    {[Some(DriverStatus::Good), Some(DriverStatus::Imperfect), None]
                        .into_iter()
                        .map(|status| {
                            view! { <option value=status_value(status)>{driver_status_label(status)}</option> }
                        })
                        .collect_view()}
                </select>
            </fieldset>
            <fieldset class="setting">
                <legend>"Keep only"</legend>
                <RangeInputs id="a-years" label="Years" settings change get=|a| a.years set=|a, r| a.years = r />
                <RangeInputs id="a-players" label="Players" settings change get=|a| a.players set=|a, r| a.players = r />
                <label class="stacked" for="a-orientation">"Screen"</label>
                <select
                    id="a-orientation"
                    prop:value=move || settings.with(|a| orientation_value(a.orientation))
                    on:change=move |ev| {
                        let orientation = orientation_of(&event_target_value(&ev));
                        change(Box::new(move |a| a.orientation = orientation));
                    }
                >
                    <option value="">"Horizontal or vertical"</option>
                    <option value="horizontal">"Horizontal only"</option>
                    <option value="vertical">"Vertical only"</option>
                </select>
                <Controls
                    label="Keep only machines with one of these controls"
                    settings
                    change
                    get=|a| a.require_controls.clone()
                    set=|a, c| a.require_controls = c
                />
                <Controls
                    label="Leave out machines with any of these controls"
                    settings
                    change
                    get=|a| a.forbid_controls.clone()
                    set=|a, c| a.forbid_controls = c
                />
            </fieldset>
            <OrderedList
                id="a-election"
                label="Election, in order"
                note="Each family keeps one machine, decided by these steps in order. A tie left goes to the shortname that sorts first."
                empty="Only the shortname decides."
                options=ElectionStep::ALL.iter().map(|s| s.label().to_owned()).collect()
                items=Signal::derive(move || settings.with(|a| a.election.iter().map(|s| s.label().to_owned()).collect()))
                on_change=Callback::new(move |labels: Vec<String>| {
                    let steps = labels.iter().filter_map(|l| step_of(l)).collect();
                    change(Box::new(move |a| a.election = steps));
                })
            />
            <OrderedList
                id="a-regions"
                label="Regions, in order of preference"
                note="Read from each machine's description. A machine that names none ranks after every region."
                empty="No preference: region doesn't decide."
                options=REGIONS.iter().map(|r| r.to_string()).collect()
                items=get(|a| a.region_order.clone())
                on_change=Callback::new(move |v: Vec<String>| change(Box::new(move |a| a.region_order = v)))
            />
            <OrderedList
                id="a-languages"
                label="Languages, in order of preference"
                note="Read from each machine's description. A machine that names none ranks with the first."
                empty="No preference: language doesn't decide."
                options=LANGUAGES.iter().map(|l| l.to_string()).collect()
                items=get(|a| a.language_order.clone())
                on_change=Callback::new(move |v: Vec<String>| change(Box::new(move |a| a.language_order = v)))
            />
            <fieldset class="setting">
                <legend>"Your lists"</legend>
                <details class="note small">
                    <summary>"How to write a line"</summary>
                    <p>
                        "One per line, matched against each machine's shortname and description. Plain text matches part of one, "
                        <code>"|"</code>
                        " then the whole of one matches exactly, and "
                        <code>"/"</code>
                        " then a regular expression matches by pattern. Always include wins over always exclude and every filter, but BIOSes, devices and machines MAME can't run stay out."
                    </p>
                </details>
                <LineEditor
                    id="a-include"
                    label="Always include"
                    items=get(|a| a.include.clone())
                    on_change=Callback::new(move |v: Vec<String>| change(Box::new(move |a| a.include = v)))
                />
                <LineEditor
                    id="a-exclude"
                    label="Always exclude"
                    items=get(|a| a.exclude.clone())
                    on_change=Callback::new(move |v: Vec<String>| change(Box::new(move |a| a.exclude = v)))
                />
            </fieldset>
            <fieldset class="setting" disabled=true>
                <legend>"Curated list and ratings"</legend>
                <p class="source">
                    <span class="tag">"Unavailable"</span>
                    " The selection doesn't read the curated list or ratings yet, so these drop nothing."
                </p>
                <ul class="checks">
                    <li>
                        <label class="inline">
                            <input type="checkbox" prop:checked=move || settings.with(|a| a.curated) />
                            "Keep only the families the curated list names"
                        </label>
                    </li>
                    <li>
                        <label class="inline">
                            <input type="checkbox" prop:checked=move || settings.with(|a| a.minimum_rating.is_some()) />
                            "Leave out families rated below a minimum"
                        </label>
                    </li>
                    <li>
                        <label class="inline">
                            <input type="checkbox" prop:checked=move || settings.with(|a| a.drop_unrated) />
                            "Leave out families with no rating"
                        </label>
                    </li>
                </ul>
            </fieldset>
        </div>
    }
}

/// A switch of the arcade settings, as a checkbox in a list.
#[component]
fn Check(
    settings: Memo<ArcadeSettings>,
    change: impl Fn(Box<dyn FnOnce(&mut ArcadeSettings)>) + Copy + 'static,
    label: &'static str,
    get: Read<bool>,
    set: Write<bool>,
) -> impl IntoView {
    view! {
        <li>
            <label class="inline">
                <input
                    type="checkbox"
                    prop:checked=move || settings.with(get)
                    on:change=move |ev| {
                        let on = event_target_checked(&ev);
                        change(Box::new(move |a| set(a, on)));
                    }
                />
                {label}
            </label>
        </li>
    }
}

/// Membership of one value in a set of the arcade settings, as a checkbox in a list.
#[component]
fn SetCheck(label: &'static str, has: Signal<bool>, toggle: Callback<bool>) -> impl IntoView {
    view! {
        <li>
            <label class="inline">
                <input
                    type="checkbox"
                    prop:checked=has
                    on:change=move |ev| toggle.run(event_target_checked(&ev))
                />
                {label}
            </label>
        </li>
    }
}

fn toggle<T: Ord>(set: &mut BTreeSet<T>, value: T, on: bool) {
    if on {
        set.insert(value);
    } else {
        set.remove(&value);
    }
}

/// An inclusive range as two number boxes; an empty box leaves that end open.
#[component]
fn RangeInputs(
    id: &'static str,
    label: &'static str,
    settings: Memo<ArcadeSettings>,
    change: impl Fn(Box<dyn FnOnce(&mut ArcadeSettings)>) + Copy + 'static,
    get: Read<Range>,
    set: Write<Range>,
) -> impl IntoView {
    let end = move |f: fn(Range) -> Option<u32>| {
        move || settings.with(|a| f(get(a)).map(|n| n.to_string()).unwrap_or_default())
    };
    let changed = move |min: bool, text: String| {
        let n = text.trim().parse::<u32>().ok();
        change(Box::new(move |a| {
            let mut range = get(a);
            if min {
                range.min = n;
            } else {
                range.max = n;
            }
            set(a, range);
        }));
    };
    view! {
        <p class="stacked">{label}</p>
        <div class="actions">
            <label class="inline" for=format!("{id}-min")>
                "From"
                <input
                    id=format!("{id}-min")
                    type="number"
                    min="0"
                    inputmode="numeric"
                    placeholder="Any"
                    prop:value=end(|r| r.min)
                    on:change=move |ev| changed(true, event_target_value(&ev))
                />
            </label>
            <label class="inline" for=format!("{id}-max")>
                "to"
                <input
                    id=format!("{id}-max")
                    type="number"
                    min="0"
                    inputmode="numeric"
                    placeholder="Any"
                    prop:value=end(|r| r.max)
                    on:change=move |ev| changed(false, event_target_value(&ev))
                />
            </label>
        </div>
    }
}

/// A set of ListXML control types, as checkboxes under a summary that names those checked.
#[component]
fn Controls(
    label: &'static str,
    settings: Memo<ArcadeSettings>,
    change: impl Fn(Box<dyn FnOnce(&mut ArcadeSettings)>) + Copy + 'static,
    get: Read<BTreeSet<String>>,
    set: Write<BTreeSet<String>>,
) -> impl IntoView {
    let chosen = move || {
        let chosen = settings.with(get);
        match chosen.is_empty() {
            true => "none".to_owned(),
            false => chosen.into_iter().collect::<Vec<_>>().join(", "),
        }
    };
    view! {
        <details class="note small">
            <summary>{move || format!("{label}: {}", chosen())}</summary>
            <ul class="checks columns">
                {CONTROL_TYPES
                    .iter()
                    .map(|control| {
                        let control = control.to_string();
                        let c = control.clone();
                        view! {
                            <li>
                                <label class="inline">
                                    <input
                                        type="checkbox"
                                        prop:checked={
                                            let c = c.clone();
                                            move || settings.with(|a| get(a).contains(&c))
                                        }
                                        on:change=move |ev| {
                                            let on = event_target_checked(&ev);
                                            let c = c.clone();
                                            change(Box::new(move |a| {
                                                let mut set_now = get(a);
                                                toggle(&mut set_now, c, on);
                                                set(a, set_now);
                                            }));
                                        }
                                    />
                                    <code>{control}</code>
                                </label>
                            </li>
                        }
                    })
                    .collect_view()}
            </ul>
        </details>
    }
}

fn status_value(status: Option<DriverStatus>) -> &'static str {
    status.map_or("", DriverStatus::as_str)
}

fn status_of(value: &str) -> Option<DriverStatus> {
    DriverStatus::ALL.into_iter().find(|s| s.as_str() == value)
}

fn orientation_value(orientation: Option<Orientation>) -> &'static str {
    match orientation {
        None => "",
        Some(Orientation::Horizontal) => "horizontal",
        Some(Orientation::Vertical) => "vertical",
    }
}

fn orientation_of(value: &str) -> Option<Orientation> {
    match value {
        "horizontal" => Some(Orientation::Horizontal),
        "vertical" => Some(Orientation::Vertical),
        _ => None,
    }
}

fn step_of(label: &str) -> Option<ElectionStep> {
    ElectionStep::ALL.into_iter().find(|s| s.label() == label)
}
