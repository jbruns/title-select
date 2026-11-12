//! Exports of what a Source Set lacks, from its latest Audit: the Fixdat and the have/miss CSV. They only read; a
//! Fixdat names what to acquire, and Title Select never acquires it.

use std::collections::HashSet;

use title_select_api::AuditSummary;
use title_select_api::selection::Role;
use title_select_engine::dat::Dat;
use title_select_engine::fixdat::{Lacking, Scope};

use super::{Error, Result, Store};

/// An exported file: the name to save it as, and its content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Export {
    pub file_name: String,
    pub content: String,
}

impl Store {
    /// The Fixdat of Source Set `name` from its latest Audit, of the whole DAT or of the winners of Selection
    /// `selection`.
    pub fn fixdat(&self, name: &str, selection: Option<i64>) -> Result<Export> {
        self.lacking(name, selection, |lacking| Export {
            file_name: lacking.fixdat_file_name(),
            content: lacking.fixdat(),
        })
    }

    /// The have/miss CSV of Source Set `name` from its latest Audit, scoped as [`Store::fixdat`] is.
    pub fn have_miss(&self, name: &str, selection: Option<i64>) -> Result<Export> {
        self.lacking(name, selection, |lacking| Export {
            file_name: lacking.have_miss_file_name(),
            content: lacking.have_miss(),
        })
    }

    fn lacking(
        &self,
        name: &str,
        selection: Option<i64>,
        export: impl FnOnce(&Lacking) -> Export,
    ) -> Result<Export> {
        let audit = self.history(name)?.into_iter().next().ok_or_else(|| {
            Error::Refused(format!(
                "source set {name} has no audit yet; audit it to export what it lacks"
            ))
        })?;
        let winners = selection
            .map(|id| self.winners(name, &audit, id))
            .transpose()?;
        let report = self.report(audit.id)?;
        let dat = Dat::read(&self.blob_path(&report.dat_revision.sha256))
            .map_err(|e| Error::Io(e.to_string()))?;
        let scope = selection.zip(winners.as_ref()).map(|(id, games)| Scope {
            selection: id,
            games,
        });
        Ok(export(&Lacking {
            dat: &dat,
            report: &report,
            audit: audit.id,
            scope,
        }))
    }

    /// The winners of Selection `id`, refused unless it selected from Source Set `name` against the DAT Revision of
    /// its latest Audit, `audit`.
    fn winners(&self, name: &str, audit: &AuditSummary, id: i64) -> Result<HashSet<String>> {
        let record = self.selection_record(id)?;
        if record.source_set != name {
            return Err(Error::Refused(format!(
                "selection {id} is of source set {}, not {name}; choose one of {name}",
                record.source_set
            )));
        }
        if record.dat_revision_sha256 != audit.dat_revision_sha256 {
            return Err(Error::Refused(format!(
                "selection {id} is against DAT revision {}, but the latest audit of {name} is against DAT revision \
                 {}; choose a selection against that revision, or audit against the selection's",
                record.dat_revision_version, audit.dat_revision_version
            )));
        }
        Ok(self
            .selection_rows(id)?
            .into_iter()
            .filter(|row| row.role == Role::Winner)
            .map(|row| row.title)
            .collect())
    }
}
