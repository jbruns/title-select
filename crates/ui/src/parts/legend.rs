//! The legend strip under a list: the statuses the list shows now, each as its icon and label. It collapses, and
//! remembers that in local storage per list.

use codee::string::FromToStringCodec;
use leptos::prelude::*;
use leptos_use::storage::use_local_storage;

use super::icon::{Glyph, Icon, LabelledIcon};
use super::status::Status;

/// The statuses among `shown`, once each, in their vocabulary's order.
pub fn present<S: Status>(shown: impl IntoIterator<Item = S>) -> Vec<S> {
    let mut seen = vec![false; S::ALL.len()];
    for s in shown {
        seen[s.rank()] = true;
    }
    S::ALL
        .iter()
        .zip(seen)
        .filter_map(|(s, seen)| seen.then_some(*s))
        .collect()
}

/// The local storage key remembering whether the legend of `list` is collapsed.
pub fn collapsed_key(list: &str) -> String {
    format!("title-select.legend.{list}.collapsed")
}

/// One legend strip for a list. `statuses` are those its rows show now (see `present`); with none, there's no strip.
/// `list` names the list, such as "source-sets", so each list remembers its own collapsed state.
#[component]
pub fn Legend<S: Status>(
    #[prop(into)] statuses: Signal<Vec<S>>,
    list: &'static str,
) -> impl IntoView {
    let (collapsed, set_collapsed, _) =
        use_local_storage::<bool, FromToStringCodec>(collapsed_key(list));
    let id = format!("legend-{list}");
    let controls = id.clone();
    view! {
        <Show when=move || statuses.with(|s| !s.is_empty())>
            <div class="legend">
                <button
                    type="button"
                    class="legend-toggle"
                    aria-expanded=move || (!collapsed.get()).to_string()
                    aria-controls=controls.clone()
                    on:click=move |_| set_collapsed.set(!collapsed.get_untracked())
                >
                    {move || {
                        let glyph = if collapsed.get() { Glyph::ChevronRight } else { Glyph::ChevronDown };
                        view! { <LabelledIcon glyph label="Legend" /> }
                    }}
                </button>
                <ul class="legend-items" id=id.clone() hidden=move || collapsed.get()>
                    <For each=move || statuses.get() key=|s| s.rank() let(s)>
                        {
                            let look = s.look();
                            view! {
                                <li class=format!("status {}", look.tone.class())>
                                    <Icon glyph=look.glyph label=look.hint />
                                    <span class="status-label">{look.label}</span>
                                </li>
                            }
                        }
                    </For>
                </ul>
            </div>
        </Show>
    }
}

#[cfg(test)]
mod tests {
    use title_select_api::JobState;

    use super::*;

    #[test]
    fn a_legend_shows_each_present_status_once_in_vocabulary_order() {
        assert_eq!(
            present([
                JobState::Failed,
                JobState::Queued,
                JobState::Failed,
                JobState::Running,
            ]),
            vec![JobState::Queued, JobState::Running, JobState::Failed]
        );
        assert!(present::<JobState>([]).is_empty());
    }
}
