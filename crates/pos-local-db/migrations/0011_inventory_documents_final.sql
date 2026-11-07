-- 0011: a posted inventory document is a fact (E11-T03): never edited, never deleted. A
-- mistake is put right by a new compensating document. Spec: docs/backlog/04-local-database.md
-- Append-only: once merged to main this file is never edited.
CREATE TRIGGER inventory_documents_no_update BEFORE UPDATE ON inventory_documents
BEGIN SELECT RAISE(ABORT, 'inventory documents are immutable'); END;
CREATE TRIGGER inventory_documents_no_delete BEFORE DELETE ON inventory_documents
BEGIN SELECT RAISE(ABORT, 'inventory documents are immutable'); END;
