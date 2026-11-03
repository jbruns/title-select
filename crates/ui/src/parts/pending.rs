//! Pending work and problems. `ActionPending` shows a slow request made through an `Action`: while it runs, a spinner,
//! a short status such as "Making plan…" and the time elapsed, over what it will replace, kept dimmed; once it fails,
//! the problem. `Pending` is the same for any pending flag.

use leptos::prelude::*;
use leptos_use::use_interval_fn;

use crate::app::Problem;

/// Time elapsed, for a pending status: "4 s", "1 min 05 s", "1 h 02 min".
pub fn elapsed(ms: f64) -> String {
    let s = (ms.max(0.0) / 1000.0).floor() as u64;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min {:02} s", s / 60, s % 60),
        _ => format!("{} h {:02} min", s / 3600, s % 3600 / 60),
    }
}

/// The problem a finished action left, if its last run failed.
pub fn action_problem<I, O>(action: Action<I, Result<O, String>>) -> Signal<Option<String>>
where
    I: 'static,
    O: 'static,
{
    Signal::derive(move || {
        action
            .value()
            .with(|v| v.as_ref().and_then(|r| r.as_ref().err().cloned()))
    })
}

/// Shows `children` and, while `action` runs, `status` over them with them dimmed; once it fails, its problem.
#[component]
pub fn ActionPending<I, O>(
    action: Action<I, Result<O, String>>,
    #[prop(into)] status: Signal<String>,
    children: ChildrenFn,
) -> impl IntoView
where
    I: 'static,
    O: 'static,
{
    view! {
        <Pending pending=action.pending() status problem=action_problem(action)>
            {children()}
        </Pending>
    }
}

/// Shows `children`, dimmed and inert with `status` above them while `pending` or while a resource they read
/// reloads (through `<Transition>`), and `problem` once it isn't pending. An `Err` that `children` render shows as a
/// problem in their place (through `<ErrorBoundary>`).
#[component]
pub fn Pending(
    #[prop(into)] pending: Signal<bool>,
    #[prop(into)] status: Signal<String>,
    #[prop(optional, into)] problem: MaybeProp<String>,
    children: ChildrenFn,
) -> impl IntoView {
    let loading = RwSignal::new(false);
    let busy = Signal::derive(move || pending.get() || loading.get());
    view! {
        <div class="pending" aria-busy=move || busy.get().to_string()>
            <Show when=move || busy.get()>
                <PendingStatus status />
            </Show>
            {move || {
                problem.get().filter(|_| !busy.get()).map(|message| view! { <Problem message /> })
            }}
            <div class="pending-content" class:dimmed=move || busy.get() inert=move || busy.get()>
                <ErrorBoundary fallback=|errors| {
                    view! {
                        {move || {
                            errors
                                .get()
                                .into_iter()
                                .map(|(_, e)| view! { <Problem message=e.to_string() /> })
                                .collect_view()
                        }}
                    }
                }>
                    <Transition set_pending=loading.write_only()>{children()}</Transition>
                </ErrorBoundary>
            </div>
        </div>
    }
}

/// A spinner, `status` and the time since it appeared. Only the status is announced; the ticking time is not.
#[component]
pub fn PendingStatus(#[prop(into)] status: Signal<String>) -> impl IntoView {
    let started = js_sys::Date::now();
    let since = RwSignal::new(0.0);
    use_interval_fn(move || since.set(js_sys::Date::now() - started), 1000);
    view! {
        <p class="pending-status" role="status">
            <span class="spinner" aria-hidden="true"></span>
            <span>{move || status.get()}</span>
            <span class="elapsed" aria-hidden="true">
                {move || elapsed(since.get())}
            </span>
        </p>
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elapsed_reads_in_the_largest_two_units() {
        assert_eq!(elapsed(0.0), "0 s");
        assert_eq!(elapsed(-5.0), "0 s");
        assert_eq!(elapsed(4_999.0), "4 s");
        assert_eq!(elapsed(59_999.0), "59 s");
        assert_eq!(elapsed(65_000.0), "1 min 05 s");
        assert_eq!(elapsed(3_599_000.0), "59 min 59 s");
        assert_eq!(elapsed(3_720_000.0), "1 h 02 min");
    }
}
