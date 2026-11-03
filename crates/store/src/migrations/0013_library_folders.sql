-- Managed Folders are now Library Folders, and Operator Additions are now Additions (#73). Renaming the table rewrites
-- the references other tables, indexes and triggers make to it, and renaming the column those to it.
ALTER TABLE managed_folders RENAME TO library_folders;
ALTER TABLE jobs RENAME COLUMN managed_folder_id TO library_folder_id;

-- The trigger's message names the folder, so it is made again.
DROP TRIGGER jobs_kind_profile;
CREATE TRIGGER jobs_kind_profile BEFORE INSERT ON jobs
WHEN (NEW.kind = 'selection') <> (NEW.profile_version_id IS NOT NULL)
  OR (NEW.kind = 'build') <> (NEW.plan_id IS NOT NULL)
  OR (NEW.kind IN ('build', 'check')) <> (NEW.library_folder_id IS NOT NULL)
BEGIN
    SELECT RAISE(ABORT, 'a selection job runs a profile version, a build job an approved plan, a check job a library folder, an audit job none');
END;

-- What a Library Folder's current Build cites.
UPDATE audit_citations SET cited_by = 'library-folder:' || substr(cited_by, length('managed-folder:') + 1)
WHERE cited_by LIKE 'managed-folder:%';
UPDATE selection_citations SET cited_by = 'library-folder:' || substr(cited_by, length('managed-folder:') + 1)
WHERE cited_by LIKE 'managed-folder:%';

-- An ingest item assessed as a candidate Addition.
UPDATE ingest_items SET assessment = json_set(assessment, '$.proposal.kind', 'addition')
WHERE json_extract(assessment, '$.proposal.kind') = 'operator_addition';
