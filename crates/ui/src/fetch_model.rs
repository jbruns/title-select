//! Fetching newer DAT Revisions from DAT Providers, as the workbench shows it: pure, and tested on the host.

use std::time::Duration;

use title_select_api::fetch::{
    FetchArtifact, FetchArtifactStatus, FetchOutcome, FetchProviderStatus, FetchStatus, FetchSystem,
};

use crate::parts::status::{FetchState, highest};

/// Where one artifact stands, for a provider that is `enabled` and `running` or not.
pub fn artifact_state(status: &FetchArtifactStatus, enabled: bool, running: bool) -> FetchState {
    let last = status.last_attempt.as_ref();
    if running && last.is_some_and(|a| a.finished_at.is_none()) {
        FetchState::Running
    } else if last.is_some_and(|a| a.outcome == Some(FetchOutcome::Failed)) {
        FetchState::Failed
    } else if !enabled {
        FetchState::Off
    } else if status.last_success.is_some() {
        FetchState::Fetched
    } else {
        FetchState::NotFetched
    }
}

/// Where a system stands: its worst artifact, or off when its provider is.
pub fn system_state(provider: &FetchProviderStatus, system: &FetchSystem) -> FetchState {
    let states = system
        .artifacts
        .iter()
        .map(|a| artifact_state(a, provider.enabled, provider.running));
    match highest(states) {
        Some(state) => state,
        None if !provider.enabled => FetchState::Off,
        None => FetchState::NotFetched,
    }
}

/// Where a provider stands as a whole.
pub fn provider_state(provider: &FetchProviderStatus) -> FetchState {
    if provider.running {
        return FetchState::Running;
    }
    if !provider.enabled {
        return FetchState::Off;
    }
    let release = provider
        .release
        .iter()
        .map(|r| artifact_state(r, true, false));
    let systems = provider.systems.iter().map(|s| system_state(provider, s));
    highest(release.chain(systems)).unwrap_or(FetchState::NotFetched)
}

/// What an artifact is called, in a sentence.
pub fn artifact_name(artifact: FetchArtifact) -> &'static str {
    match artifact {
        FetchArtifact::Release => "release check",
        FetchArtifact::Dat => "DAT",
        FetchArtifact::Cues => "cue pack",
        FetchArtifact::Sbi => "SBI pack",
    }
}

/// The latest time any of `artifacts` was fetched, if one was.
pub fn last_fetched(artifacts: &[FetchArtifactStatus]) -> Option<String> {
    artifacts
        .iter()
        .filter_map(|a| a.last_success.as_ref())
        .map(|a| {
            a.finished_at
                .clone()
                .unwrap_or_else(|| a.started_at.clone())
        })
        .max()
}

/// The earliest time the next request for any of `artifacts` may be sent, if one may not be sent now.
pub fn next_fetch(artifacts: &[FetchArtifactStatus]) -> Option<String> {
    artifacts.iter().filter_map(|a| a.next_at.clone()).min()
}

/// Each failed artifact's latest error, as `(artifact, error)`.
pub fn failures(system: &FetchSystem) -> Vec<(FetchArtifact, String)> {
    system
        .artifacts
        .iter()
        .filter_map(|a| a.last_attempt.as_ref())
        .filter(|a| a.outcome == Some(FetchOutcome::Failed))
        .map(|a| (a.artifact, a.error.clone().unwrap_or_default()))
        .collect()
}

/// How soon the panel refreshes while a provider is fetching, so it shows each request as it finishes.
pub const RUNNING_REFRESH: Duration = Duration::from_secs(3);
/// How soon the panel refreshes while a provider may fetch on its schedule.
pub const IDLE_REFRESH: Duration = Duration::from_secs(60);

/// How soon the panel should load `status` again: quickly while any provider is fetching, slowly while any is on,
/// and never when none is.
pub fn refresh_after(status: &FetchStatus) -> Option<Duration> {
    if status.providers.iter().any(|p| p.running) {
        Some(RUNNING_REFRESH)
    } else if status.providers.iter().any(|p| p.enabled) {
        Some(IDLE_REFRESH)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use title_select_api::fetch::{FetchAttempt, FetchProvider, FetchTrigger};

    use super::*;

    fn attempt(artifact: FetchArtifact, outcome: Option<FetchOutcome>, at: &str) -> FetchAttempt {
        FetchAttempt {
            id: 1,
            provider: FetchProvider::Redump,
            system: "psx".into(),
            artifact,
            trigger: FetchTrigger::Schedule,
            url: String::new(),
            started_at: at.into(),
            finished_at: outcome.map(|_| at.into()),
            outcome,
            error: (outcome == Some(FetchOutcome::Failed)).then(|| "HTTP 503".into()),
            dat_revision: None,
            version: None,
            pack: None,
            release: None,
        }
    }

    fn status(
        artifact: FetchArtifact,
        last: Option<FetchAttempt>,
        success: Option<FetchAttempt>,
    ) -> FetchArtifactStatus {
        FetchArtifactStatus {
            artifact,
            last_attempt: last,
            last_success: success,
            next_at: None,
        }
    }

    fn provider(
        enabled: bool,
        running: bool,
        artifacts: Vec<FetchArtifactStatus>,
    ) -> FetchProviderStatus {
        FetchProviderStatus {
            provider: FetchProvider::Redump,
            enabled,
            running,
            release: None,
            systems: vec![FetchSystem {
                system: Some("psx".into()),
                dat: "Redump/Sony - PlayStation".into(),
                source_sets: vec!["PSX".into()],
                artifacts,
            }],
        }
    }

    #[test]
    fn a_failure_outranks_an_older_success_and_shows_its_error() {
        let ok = attempt(
            FetchArtifact::Dat,
            Some(FetchOutcome::Added),
            "2026-01-01T00:00:00Z",
        );
        let failed = attempt(
            FetchArtifact::Cues,
            Some(FetchOutcome::Failed),
            "2026-01-02T00:00:00Z",
        );
        let p = provider(
            true,
            false,
            vec![
                status(FetchArtifact::Dat, Some(ok.clone()), Some(ok)),
                status(FetchArtifact::Cues, Some(failed), None),
            ],
        );
        let system = &p.systems[0];
        assert_eq!(system_state(&p, system), FetchState::Failed);
        assert_eq!(provider_state(&p), FetchState::Failed);
        assert_eq!(
            failures(system),
            [(FetchArtifact::Cues, "HTTP 503".to_string())]
        );
        assert_eq!(
            last_fetched(&system.artifacts).as_deref(),
            Some("2026-01-01T00:00:00Z")
        );
    }

    #[test]
    fn a_provider_turned_off_shows_off_unless_a_failure_is_recorded() {
        let ok = attempt(
            FetchArtifact::Dat,
            Some(FetchOutcome::Unchanged),
            "2026-01-01T00:00:00Z",
        );
        let p = provider(
            false,
            false,
            vec![status(FetchArtifact::Dat, Some(ok.clone()), Some(ok))],
        );
        assert_eq!(system_state(&p, &p.systems[0]), FetchState::Off);
        assert_eq!(provider_state(&p), FetchState::Off);
        let never = provider(true, false, vec![status(FetchArtifact::Dat, None, None)]);
        assert_eq!(
            system_state(&never, &never.systems[0]),
            FetchState::NotFetched
        );
    }

    #[test]
    fn an_unfinished_request_shows_running_only_while_its_provider_runs() {
        let open = attempt(FetchArtifact::Dat, None, "2026-01-01T00:00:00Z");
        let running = provider(
            true,
            true,
            vec![status(FetchArtifact::Dat, Some(open.clone()), None)],
        );
        assert_eq!(
            system_state(&running, &running.systems[0]),
            FetchState::Running
        );
        let stopped = provider(
            true,
            false,
            vec![status(FetchArtifact::Dat, Some(open), None)],
        );
        assert_eq!(
            system_state(&stopped, &stopped.systems[0]),
            FetchState::NotFetched
        );
    }

    #[test]
    fn the_panel_refreshes_quickly_while_a_fetch_runs_slowly_while_one_may_and_never_when_none_can()
    {
        let at = |enabled, running| FetchStatus {
            providers: vec![
                provider(false, false, vec![]),
                provider(enabled, running, vec![]),
            ],
        };
        assert_eq!(refresh_after(&at(true, true)), Some(RUNNING_REFRESH));
        assert_eq!(refresh_after(&at(true, false)), Some(IDLE_REFRESH));
        assert_eq!(refresh_after(&at(false, false)), None);
        assert!(
            RUNNING_REFRESH >= Duration::from_secs(2) && RUNNING_REFRESH <= Duration::from_secs(5)
        );
    }
}
