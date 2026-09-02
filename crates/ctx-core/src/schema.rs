//! Framework-neutral persistent-state model: deterministic schema-change
//! classification over the normalized [`SchemaTableDefinition`]/[`SchemaColumn`]
//! IR that goose migrations, `SQLAlchemy` models, and any future schema source
//! all populate identically. This module knows nothing about goose or
//! `SQLAlchemy` specifically; it only ever reads the shared IR shape.
//!
//! Two complementary, independently useful views are provided:
//!
//! - [`declared_schema_changes`] reads the operations one schema-declaring
//!   file/statement (a migration, or an ORM model version) declares on its
//!   own, with no history needed. A brand-new migration that drops a column
//!   is destructive the moment it exists; no prior version is required to
//!   know that.
//! - [`diff_schema_tables`] structurally compares two versions of the same
//!   schema-declaring symbol (for example an edited `SQLAlchemy` model),
//!   surfacing column-level changes that are only visible as a diff.
//!
//! Renames are never inferred from a diff: a column absent from `after` and
//! a differently named column present in `after` are reported independently
//! as dropped/added unless the source explicitly declares the rename (goose
//! `RENAME COLUMN`, read directly off [`SchemaTableDefinition::renamed_columns`]).
//! Guessing a rename from name/type similarity would be exactly the kind of
//! fuzzy matching this project's provenance rules forbid.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::{
    graph::GraphSnapshot,
    indexing::PlannedNodeAttributes,
    ir::{ColumnAlteration, SchemaColumn, SchemaTableDefinition, SymbolKind},
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SchemaChangeKind {
    TableCreated,
    TableDropped,
    TableRenamed {
        previous_entity: String,
    },
    ColumnAdded {
        column: String,
    },
    /// A `NOT NULL` column with no `DEFAULT`, added to a table that already
    /// existed before this statement (`table_created` was `false`). A
    /// well-known destructive migration pattern: existing rows have no value
    /// for the new column.
    ColumnAddedNotNullWithoutDefault {
        column: String,
    },
    ColumnDropped {
        column: String,
    },
    ColumnRenamed {
        previous_name: String,
        new_name: String,
    },
    ColumnTypeChanged {
        column: String,
        before: String,
        after: String,
    },
    /// An `ALTER TABLE ... ALTER COLUMN ... TYPE` statement declares a new
    /// type for an existing column with no access to its prior type (unlike
    /// `ColumnTypeChanged`, which comes from a diff that has both).
    ColumnTypeAltered {
        column: String,
        new_type: String,
    },
    /// An `ALTER TABLE ... ALTER COLUMN ... SET/DROP NOT NULL` statement.
    /// `nullable: false` means `SET NOT NULL` (tightening); `true` means
    /// `DROP NOT NULL` (relaxing).
    ColumnNullabilityAltered {
        column: String,
        nullable: bool,
    },
    /// An `ALTER TABLE ... ALTER COLUMN ... SET/DROP DEFAULT` statement.
    ColumnDefaultAltered {
        column: String,
    },
    ColumnNullabilityTightened {
        column: String,
    },
    ColumnNullabilityRelaxed {
        column: String,
    },
    ColumnDefaultChanged {
        column: String,
    },
    PrimaryKeyChanged {
        column: String,
    },
    ForeignKeyChanged {
        column: String,
    },
    UniqueConstraintAdded {
        column: String,
    },
    UniqueConstraintRemoved {
        column: String,
    },
    CheckConstraintChanged,
    IndexAdded {
        index: String,
    },
    IndexDropped {
        index: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaChange {
    pub entity: String,
    pub kind: SchemaChangeKind,
    /// Contract-relevant per prompt2.md's destructive-change list: a
    /// reviewer should treat this as needing explicit attention, distinct
    /// from routine/additive schema evolution (a new nullable column, a new
    /// index).
    pub destructive: bool,
}

impl SchemaChange {
    #[must_use]
    pub fn description(&self) -> String {
        match &self.kind {
            SchemaChangeKind::TableCreated => format!("table {} created", self.entity),
            SchemaChangeKind::TableDropped => format!("table {} dropped", self.entity),
            SchemaChangeKind::TableRenamed { previous_entity } => {
                format!("table {previous_entity} renamed to {}", self.entity)
            }
            SchemaChangeKind::ColumnAdded { column } => {
                format!("{}.{column} added", self.entity)
            }
            SchemaChangeKind::ColumnAddedNotNullWithoutDefault { column } => format!(
                "{}.{column} added as NOT NULL with no DEFAULT on an existing table",
                self.entity
            ),
            SchemaChangeKind::ColumnDropped { column } => {
                format!("{}.{column} dropped", self.entity)
            }
            SchemaChangeKind::ColumnRenamed {
                previous_name,
                new_name,
            } => format!(
                "{}.{previous_name} renamed to {}.{new_name}",
                self.entity, self.entity
            ),
            SchemaChangeKind::ColumnTypeChanged {
                column,
                before,
                after,
            } => format!(
                "{}.{column} type changed from {before} to {after}",
                self.entity
            ),
            SchemaChangeKind::ColumnTypeAltered { column, new_type } => {
                format!("{}.{column} type altered to {new_type}", self.entity)
            }
            SchemaChangeKind::ColumnNullabilityAltered { column, nullable } => format!(
                "{}.{column} {}",
                self.entity,
                if *nullable {
                    "became nullable"
                } else {
                    "became NOT NULL"
                }
            ),
            SchemaChangeKind::ColumnDefaultAltered { column } => {
                format!("{}.{column} default altered", self.entity)
            }
            SchemaChangeKind::ColumnNullabilityTightened { column } => {
                format!("{}.{column} became NOT NULL", self.entity)
            }
            SchemaChangeKind::ColumnNullabilityRelaxed { column } => {
                format!("{}.{column} became nullable", self.entity)
            }
            SchemaChangeKind::ColumnDefaultChanged { column } => {
                format!("{}.{column} default changed", self.entity)
            }
            SchemaChangeKind::PrimaryKeyChanged { column } => {
                format!("{}.{column} primary-key membership changed", self.entity)
            }
            SchemaChangeKind::ForeignKeyChanged { column } => {
                format!("{}.{column} foreign-key target changed", self.entity)
            }
            SchemaChangeKind::UniqueConstraintAdded { column } => {
                format!("{}.{column} unique constraint added", self.entity)
            }
            SchemaChangeKind::UniqueConstraintRemoved { column } => {
                format!("{}.{column} unique constraint removed", self.entity)
            }
            SchemaChangeKind::CheckConstraintChanged => {
                format!("{} check constraints changed", self.entity)
            }
            SchemaChangeKind::IndexAdded { index } => {
                format!("index {index} added on {}", self.entity)
            }
            SchemaChangeKind::IndexDropped { index } => {
                format!("index {index} dropped on {}", self.entity)
            }
        }
    }
}

fn change(entity: &str, kind: SchemaChangeKind, destructive: bool) -> SchemaChange {
    SchemaChange {
        entity: entity.to_owned(),
        kind,
        destructive,
    }
}

/// Reads the operations one schema-declaring statement/file declares about
/// its own table, independent of any prior version. Always meaningful even
/// for a brand-new file.
#[must_use]
pub fn declared_schema_changes(table: &SchemaTableDefinition) -> Vec<SchemaChange> {
    let entity = table.entity.as_str();
    let mut changes = Vec::new();
    if table.table_created {
        changes.push(change(entity, SchemaChangeKind::TableCreated, false));
    }
    if table.table_dropped {
        changes.push(change(entity, SchemaChangeKind::TableDropped, true));
    }
    if let Some(previous_entity) = &table.renamed_from {
        changes.push(change(
            entity,
            SchemaChangeKind::TableRenamed {
                previous_entity: previous_entity.clone(),
            },
            true,
        ));
    }
    // A brand-new table's initial column set is not a "change" to review —
    // nothing downstream depends on the table yet, so only the single
    // `TableCreated` summary above is emitted, not one entry per column.
    if !table.table_created {
        for column in &table.columns {
            if column.nullable == Some(false) && column.default.is_none() {
                changes.push(change(
                    entity,
                    SchemaChangeKind::ColumnAddedNotNullWithoutDefault {
                        column: column.name.clone(),
                    },
                    true,
                ));
            } else {
                changes.push(change(
                    entity,
                    SchemaChangeKind::ColumnAdded {
                        column: column.name.clone(),
                    },
                    false,
                ));
            }
        }
    }
    for column in &table.dropped_columns {
        changes.push(change(
            entity,
            SchemaChangeKind::ColumnDropped {
                column: column.clone(),
            },
            true,
        ));
    }
    for rename in &table.renamed_columns {
        changes.push(change(
            entity,
            SchemaChangeKind::ColumnRenamed {
                previous_name: rename.previous_name.clone(),
                new_name: rename.new_name.clone(),
            },
            true,
        ));
    }
    for alteration in &table.column_alterations {
        changes.extend(column_alteration_changes(entity, alteration));
    }
    for index in &table.indexes_added {
        changes.push(change(
            entity,
            SchemaChangeKind::IndexAdded {
                index: index_label(index),
            },
            false,
        ));
    }
    for index in &table.indexes_dropped {
        changes.push(change(
            entity,
            SchemaChangeKind::IndexDropped {
                index: index.clone(),
            },
            false,
        ));
    }
    changes
}

fn column_alteration_changes(entity: &str, alteration: &ColumnAlteration) -> Vec<SchemaChange> {
    let mut changes = Vec::new();
    if let Some(new_type) = &alteration.new_type {
        changes.push(change(
            entity,
            SchemaChangeKind::ColumnTypeAltered {
                column: alteration.column.clone(),
                new_type: new_type.clone(),
            },
            true,
        ));
    }
    if let Some(nullable) = alteration.nullable {
        changes.push(change(
            entity,
            SchemaChangeKind::ColumnNullabilityAltered {
                column: alteration.column.clone(),
                nullable,
            },
            !nullable,
        ));
    }
    if alteration.default_changed {
        changes.push(change(
            entity,
            SchemaChangeKind::ColumnDefaultAltered {
                column: alteration.column.clone(),
            },
            false,
        ));
    }
    changes
}

fn index_label(index: &crate::ir::SchemaIndex) -> String {
    index
        .name
        .clone()
        .unwrap_or_else(|| index.columns.join(","))
}

/// Structurally compares two versions of the same schema-declaring symbol's
/// table declaration (for example an edited `SQLAlchemy` model). Only columns
/// matched by exact name in both versions are compared field-by-field; a
/// column present in one version and absent from the other is reported as
/// dropped/added, never guessed as a rename.
#[must_use]
pub fn diff_schema_tables(
    before: &SchemaTableDefinition,
    after: &SchemaTableDefinition,
) -> Vec<SchemaChange> {
    let entity = after.entity.as_str();
    let mut changes = Vec::new();
    let before_columns = column_map(before);
    let after_columns = column_map(after);
    let renamed_away: BTreeSet<&str> = after
        .renamed_columns
        .iter()
        .map(|rename| rename.previous_name.as_str())
        .collect();

    for (name, before_column) in &before_columns {
        if renamed_away.contains(name) {
            continue;
        }
        match after_columns.get(name) {
            None => changes.push(change(
                entity,
                SchemaChangeKind::ColumnDropped {
                    column: (*name).to_owned(),
                },
                true,
            )),
            Some(after_column) => {
                changes.extend(diff_column(entity, before_column, after_column));
            }
        }
    }
    for name in after_columns.keys() {
        if !before_columns.contains_key(name) {
            changes.push(change(
                entity,
                SchemaChangeKind::ColumnAdded {
                    column: (*name).to_owned(),
                },
                false,
            ));
        }
    }
    let before_checks: BTreeSet<&str> = before.checks.iter().map(String::as_str).collect();
    let after_checks: BTreeSet<&str> = after.checks.iter().map(String::as_str).collect();
    if before_checks != after_checks {
        changes.push(change(
            entity,
            SchemaChangeKind::CheckConstraintChanged,
            true,
        ));
    }
    changes
}

fn column_map(table: &SchemaTableDefinition) -> std::collections::BTreeMap<&str, &SchemaColumn> {
    table
        .columns
        .iter()
        .map(|column| (column.name.as_str(), column))
        .collect()
}

fn diff_column(entity: &str, before: &SchemaColumn, after: &SchemaColumn) -> Vec<SchemaChange> {
    let mut changes = Vec::new();
    let name = &after.name;
    if before.data_type != after.data_type {
        changes.push(change(
            entity,
            SchemaChangeKind::ColumnTypeChanged {
                column: name.clone(),
                before: before.data_type.clone(),
                after: after.data_type.clone(),
            },
            true,
        ));
    }
    match (before.nullable, after.nullable) {
        (Some(true) | None, Some(false)) => changes.push(change(
            entity,
            SchemaChangeKind::ColumnNullabilityTightened {
                column: name.clone(),
            },
            true,
        )),
        (Some(false), Some(true)) => changes.push(change(
            entity,
            SchemaChangeKind::ColumnNullabilityRelaxed {
                column: name.clone(),
            },
            false,
        )),
        _ => {}
    }
    if before.default != after.default {
        changes.push(change(
            entity,
            SchemaChangeKind::ColumnDefaultChanged {
                column: name.clone(),
            },
            false,
        ));
    }
    if before.primary_key != after.primary_key {
        changes.push(change(
            entity,
            SchemaChangeKind::PrimaryKeyChanged {
                column: name.clone(),
            },
            true,
        ));
    }
    if before.foreign_key != after.foreign_key {
        changes.push(change(
            entity,
            SchemaChangeKind::ForeignKeyChanged {
                column: name.clone(),
            },
            true,
        ));
    }
    if !before.unique && after.unique {
        changes.push(change(
            entity,
            SchemaChangeKind::UniqueConstraintAdded {
                column: name.clone(),
            },
            false,
        ));
    } else if before.unique && !after.unique {
        changes.push(change(
            entity,
            SchemaChangeKind::UniqueConstraintRemoved {
                column: name.clone(),
            },
            true,
        ));
    }
    changes
}

/// One column present in an ORM model or migration-derived schema state but
/// absent from the other. Presence-only: this reconciliation does not
/// compare type/nullable/etc. between the two sources (that level of detail
/// is already covered per-source by `declared_schema_changes`/
/// `diff_schema_tables` as each source evolves on its own).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DivergenceKind {
    /// The `SQLAlchemy` model declares this column; no migration-derived
    /// state does.
    ExpectedByOrmOnly,
    /// A migration declares this column; no `SQLAlchemy` model field does.
    DeclaredByMigrationOnly,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchemaDivergence {
    pub entity: String,
    pub column: String,
    pub kind: DivergenceKind,
}

/// Best-effort reconciliation between `SQLAlchemy` declarative models and
/// goose migration history, over the current graph.
///
/// Only entities declared by *both* an ORM model and at least one migration
/// are compared — an entity known from only one source has nothing to
/// reconcile. This project never fuzzy-matches schema identity: a
/// migration's and an ORM model's `entity` name are either the same string
/// (compared exactly) or they are unrelated facts about different tables,
/// so there is no "ambiguous match" state to represent here.
///
/// The migration-derived column set is reconstructed by replaying every
/// migration file's declared add/drop/rename operations for that entity in
/// file-path order (goose's own numeric-timestamp-prefix convention sorts
/// correctly this way). This reconstruction is a diagnostic aid, not a
/// stored fact: it is never persisted as a `DEFINES_SCHEMA` edge or treated
/// as a `DbEntity`'s "current" schema anywhere else in this codebase. A
/// migration statement this codebase cannot recognize already produces zero
/// facts elsewhere (see `ctx-adapters::database::parse_ddl_statement`), so
/// it is silently absent from this replay too — a real mismatch hidden
/// behind unsupported DDL stays invisible here rather than being reported as
/// false consistency.
#[must_use]
pub fn reconcile_orm_and_migrations(graph: &GraphSnapshot) -> Vec<SchemaDivergence> {
    let mut migration_tables: BTreeMap<&str, Vec<(&str, &SchemaTableDefinition)>> = BTreeMap::new();
    let mut orm_tables: BTreeMap<&str, Vec<&SchemaTableDefinition>> = BTreeMap::new();
    for node in graph.nodes.values() {
        let PlannedNodeAttributes::Symbol {
            symbol_kind,
            file_path,
            schema_tables,
            ..
        } = &node.attributes
        else {
            continue;
        };
        for table in schema_tables {
            if *symbol_kind == SymbolKind::SchemaMigration {
                migration_tables
                    .entry(table.entity.as_str())
                    .or_default()
                    .push((file_path.as_str(), table));
            } else {
                orm_tables
                    .entry(table.entity.as_str())
                    .or_default()
                    .push(table);
            }
        }
    }

    let mut divergences = Vec::new();
    for (entity, orm_versions) in &orm_tables {
        let Some(migrations) = migration_tables.get(entity) else {
            continue;
        };
        let orm_columns: BTreeSet<&str> = orm_versions
            .iter()
            .flat_map(|table| table.columns.iter().map(|column| column.name.as_str()))
            .collect();
        let migration_columns = migration_derived_columns(migrations);
        for column in &orm_columns {
            if !migration_columns.contains(*column) {
                divergences.push(SchemaDivergence {
                    entity: (*entity).to_owned(),
                    column: (*column).to_owned(),
                    kind: DivergenceKind::ExpectedByOrmOnly,
                });
            }
        }
        for column in &migration_columns {
            if !orm_columns.contains(column.as_str()) {
                divergences.push(SchemaDivergence {
                    entity: (*entity).to_owned(),
                    column: column.clone(),
                    kind: DivergenceKind::DeclaredByMigrationOnly,
                });
            }
        }
    }
    divergences.sort_by(|left, right| {
        left.entity
            .cmp(&right.entity)
            .then_with(|| left.column.cmp(&right.column))
    });
    divergences
}

fn migration_derived_columns(migrations: &[(&str, &SchemaTableDefinition)]) -> BTreeSet<String> {
    let mut ordered = migrations.to_vec();
    ordered.sort_by(|left, right| left.0.cmp(right.0));
    let mut columns = BTreeSet::new();
    for (_, table) in ordered {
        for column in &table.columns {
            columns.insert(column.name.clone());
        }
        for dropped in &table.dropped_columns {
            columns.remove(dropped);
        }
        for rename in &table.renamed_columns {
            columns.remove(&rename.previous_name);
            columns.insert(rename.new_name.clone());
        }
    }
    columns
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{ColumnRename, ForeignKeyRef, SchemaIndex, SourceRange};

    fn table(entity: &str, columns: Vec<SchemaColumn>) -> SchemaTableDefinition {
        SchemaTableDefinition {
            entity: entity.to_owned(),
            columns,
            range: SourceRange::default(),
            ..SchemaTableDefinition::default()
        }
    }

    fn column(name: &str, data_type: &str) -> SchemaColumn {
        SchemaColumn {
            name: name.to_owned(),
            data_type: data_type.to_owned(),
            ..SchemaColumn::default()
        }
    }

    #[test]
    fn a_freshly_created_table_is_not_destructive() {
        let mut created = table("subscriptions", vec![column("id", "UUID")]);
        created.table_created = true;
        let changes = declared_schema_changes(&created);
        assert_eq!(changes.len(), 1);
        assert!(!changes[0].destructive);
        assert_eq!(changes[0].kind, SchemaChangeKind::TableCreated);
    }

    #[test]
    fn adding_a_not_null_column_without_default_to_an_existing_table_is_destructive() {
        let mut added = column("grace_period_days", "INT");
        added.nullable = Some(false);
        let migration = table("subscriptions", vec![added]);
        let changes = declared_schema_changes(&migration);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].destructive);
        assert_eq!(
            changes[0].kind,
            SchemaChangeKind::ColumnAddedNotNullWithoutDefault {
                column: "grace_period_days".to_owned()
            }
        );
    }

    #[test]
    fn a_nullable_added_column_is_not_destructive() {
        let mut added = column("grace_period_days", "INT");
        added.nullable = Some(true);
        let migration = table("subscriptions", vec![added]);
        let changes = declared_schema_changes(&migration);
        assert!(!changes[0].destructive);
    }

    #[test]
    fn declared_drop_and_rename_are_destructive() {
        let migration = SchemaTableDefinition {
            entity: "subscriptions".to_owned(),
            dropped_columns: vec!["legacy_flag".to_owned()],
            renamed_columns: vec![ColumnRename {
                previous_name: "status".to_owned(),
                new_name: "state".to_owned(),
            }],
            ..SchemaTableDefinition::default()
        };
        let changes = declared_schema_changes(&migration);
        assert_eq!(changes.len(), 2);
        assert!(changes.iter().all(|change| change.destructive));
    }

    #[test]
    fn table_dropped_and_renamed_are_destructive() {
        let dropped = table("subscriptions", vec![]);
        let mut dropped = dropped;
        dropped.table_dropped = true;
        assert!(declared_schema_changes(&dropped)[0].destructive);

        let mut renamed = table("subscription_plans", vec![]);
        renamed.renamed_from = Some("subscriptions".to_owned());
        assert!(declared_schema_changes(&renamed)[0].destructive);
    }

    #[test]
    fn index_changes_are_never_destructive() {
        let mut added_index = table("subscriptions", vec![]);
        added_index.indexes_added.push(SchemaIndex {
            name: Some("idx_email".to_owned()),
            columns: vec!["email".to_owned()],
            unique: false,
        });
        assert!(!declared_schema_changes(&added_index)[0].destructive);

        let mut dropped_index = table("subscriptions", vec![]);
        dropped_index.indexes_dropped.push("idx_email".to_owned());
        assert!(!declared_schema_changes(&dropped_index)[0].destructive);
    }

    #[test]
    fn declared_alter_column_operations_classify_correctly() {
        use crate::ir::ColumnAlteration;

        let mut type_altered = table("subscriptions", vec![]);
        type_altered.column_alterations.push(ColumnAlteration {
            column: "amount".to_owned(),
            new_type: Some("NUMERIC(10, 2)".to_owned()),
            ..ColumnAlteration::default()
        });
        let changes = declared_schema_changes(&type_altered);
        assert_eq!(changes.len(), 1);
        assert!(changes[0].destructive);
        assert_eq!(
            changes[0].kind,
            SchemaChangeKind::ColumnTypeAltered {
                column: "amount".to_owned(),
                new_type: "NUMERIC(10, 2)".to_owned(),
            }
        );

        let mut tightened = table("subscriptions", vec![]);
        tightened.column_alterations.push(ColumnAlteration {
            column: "status".to_owned(),
            nullable: Some(false),
            ..ColumnAlteration::default()
        });
        assert!(declared_schema_changes(&tightened)[0].destructive);

        let mut relaxed = table("subscriptions", vec![]);
        relaxed.column_alterations.push(ColumnAlteration {
            column: "status".to_owned(),
            nullable: Some(true),
            ..ColumnAlteration::default()
        });
        assert!(!declared_schema_changes(&relaxed)[0].destructive);

        let mut default_altered = table("subscriptions", vec![]);
        default_altered.column_alterations.push(ColumnAlteration {
            column: "status".to_owned(),
            default_changed: true,
            ..ColumnAlteration::default()
        });
        assert!(!declared_schema_changes(&default_altered)[0].destructive);
    }

    #[test]
    fn diff_detects_column_removed_as_destructive() {
        let before = table("subscriptions", vec![column("email", "VARCHAR")]);
        let after = table("subscriptions", vec![]);
        let changes = diff_schema_tables(&before, &after);
        assert_eq!(
            changes,
            vec![change(
                "subscriptions",
                SchemaChangeKind::ColumnDropped {
                    column: "email".to_owned()
                },
                true
            )]
        );
    }

    #[test]
    fn diff_does_not_guess_a_rename_from_name_similarity() {
        let before = table("subscriptions", vec![column("email_address", "VARCHAR")]);
        let after = table("subscriptions", vec![column("email", "VARCHAR")]);
        let changes = diff_schema_tables(&before, &after);
        assert_eq!(changes.len(), 2);
        assert!(changes.contains(&change(
            "subscriptions",
            SchemaChangeKind::ColumnDropped {
                column: "email_address".to_owned()
            },
            true
        )));
        assert!(changes.contains(&change(
            "subscriptions",
            SchemaChangeKind::ColumnAdded {
                column: "email".to_owned()
            },
            false
        )));
    }

    #[test]
    fn diff_detects_type_and_nullability_and_foreign_key_changes() {
        let mut before_col = column("amount", "INTEGER");
        before_col.nullable = Some(true);
        let mut after_col = column("amount", "NUMERIC(10, 2)");
        after_col.nullable = Some(false);
        after_col.foreign_key = Some(ForeignKeyRef {
            table: "currencies".to_owned(),
            column: Some("code".to_owned()),
        });
        let before = table("subscriptions", vec![before_col]);
        let after = table("subscriptions", vec![after_col]);
        let changes = diff_schema_tables(&before, &after);
        assert!(
            changes.iter().any(
                |c| matches!(c.kind, SchemaChangeKind::ColumnTypeChanged { .. }) && c.destructive
            )
        );
        assert!(changes.iter().any(|c| matches!(
            c.kind,
            SchemaChangeKind::ColumnNullabilityTightened { .. }
        ) && c.destructive));
        assert!(
            changes.iter().any(
                |c| matches!(c.kind, SchemaChangeKind::ForeignKeyChanged { .. }) && c.destructive
            )
        );
    }

    #[test]
    fn diff_respects_an_explicit_rename_declaration() {
        let before = table("subscriptions", vec![column("status", "VARCHAR")]);
        let mut after = table("subscriptions", vec![column("state", "VARCHAR")]);
        after.renamed_columns.push(ColumnRename {
            previous_name: "status".to_owned(),
            new_name: "state".to_owned(),
        });
        let changes = diff_schema_tables(&before, &after);
        // The explicitly renamed-away column must not also be reported as
        // dropped; the caller (review) surfaces the rename via
        // `declared_schema_changes` on the same `after` table instead.
        assert!(!changes.iter().any(|c| matches!(
            &c.kind,
            SchemaChangeKind::ColumnDropped { column } if column == "status"
        )));
    }

    #[test]
    fn unrelated_column_reordering_and_unchanged_types_produce_no_findings() {
        let before = table(
            "subscriptions",
            vec![column("id", "UUID"), column("status", "VARCHAR(50)")],
        );
        let after = table(
            "subscriptions",
            vec![column("status", "VARCHAR(50)"), column("id", "UUID")],
        );
        assert!(diff_schema_tables(&before, &after).is_empty());
    }

    #[test]
    fn check_constraint_text_changes_are_destructive() {
        let mut before = table("subscriptions", vec![]);
        before.checks.push("amount >= 0".to_owned());
        let mut after = table("subscriptions", vec![]);
        after.checks.push("amount > 0".to_owned());
        let changes = diff_schema_tables(&before, &after);
        assert_eq!(
            changes,
            vec![change(
                "subscriptions",
                SchemaChangeKind::CheckConstraintChanged,
                true
            )]
        );
    }

    use crate::{
        domain::{NodeKind, StableKey},
        graph::GraphNode,
    };

    fn symbol_node(
        key: &str,
        symbol_kind: SymbolKind,
        file_path: &str,
        canonical_path: &str,
        schema_tables: Vec<SchemaTableDefinition>,
    ) -> GraphNode {
        GraphNode {
            stable_key: StableKey::new(key).expect("stable key"),
            kind: NodeKind::CodeSymbol,
            name: canonical_path.to_owned(),
            content_hash: "hash".to_owned(),
            attributes: PlannedNodeAttributes::Symbol {
                file_path: file_path.to_owned(),
                canonical_path: canonical_path.to_owned(),
                symbol_kind,
                range: SourceRange::default(),
                signature: None,
                structural_fingerprint: "fp".to_owned(),
                calls: Vec::new(),
                database_accesses: Vec::new(),
                orm_accesses: Vec::new(),
                schema_tables,
                api_endpoints: Vec::new(),
                external_calls: Vec::new(),
            },
        }
    }

    fn snapshot(nodes: Vec<GraphNode>) -> GraphSnapshot {
        GraphSnapshot {
            nodes: nodes
                .into_iter()
                .map(|node| (node.stable_key.clone(), node))
                .collect(),
            edges: Vec::new(),
        }
    }

    #[test]
    fn reconciliation_ignores_entities_known_from_only_one_source() {
        let migration_only = symbol_node(
            "symbol:goose:m1",
            SymbolKind::SchemaMigration,
            "migrations/001.sql",
            "migrations.001",
            vec![table("audit_log", vec![column("id", "UUID")])],
        );
        let orm_only = symbol_node(
            "symbol:python:models.Foo",
            SymbolKind::Class,
            "models.py",
            "models.Foo",
            vec![table("widgets", vec![column("id", "UUID")])],
        );
        let divergences = reconcile_orm_and_migrations(&snapshot(vec![migration_only, orm_only]));
        assert!(divergences.is_empty());
    }

    #[test]
    fn reconciliation_detects_columns_only_expected_by_orm() {
        let migration = symbol_node(
            "symbol:goose:m1",
            SymbolKind::SchemaMigration,
            "migrations/001.sql",
            "migrations.001",
            vec![table("users", vec![column("id", "UUID")])],
        );
        let orm = symbol_node(
            "symbol:python:models.User",
            SymbolKind::Class,
            "models.py",
            "models.User",
            vec![table(
                "users",
                vec![column("id", "UUID"), column("email", "VARCHAR")],
            )],
        );
        let divergences = reconcile_orm_and_migrations(&snapshot(vec![migration, orm]));
        assert_eq!(
            divergences,
            vec![SchemaDivergence {
                entity: "users".to_owned(),
                column: "email".to_owned(),
                kind: DivergenceKind::ExpectedByOrmOnly,
            }]
        );
    }

    #[test]
    fn reconciliation_detects_columns_only_declared_by_migrations() {
        let migration = symbol_node(
            "symbol:goose:m1",
            SymbolKind::SchemaMigration,
            "migrations/001.sql",
            "migrations.001",
            vec![table(
                "subscriptions",
                vec![column("id", "UUID"), column("grace_period", "INT")],
            )],
        );
        let orm = symbol_node(
            "symbol:python:models.Subscription",
            SymbolKind::Class,
            "models.py",
            "models.Subscription",
            vec![table("subscriptions", vec![column("id", "UUID")])],
        );
        let divergences = reconcile_orm_and_migrations(&snapshot(vec![migration, orm]));
        assert_eq!(
            divergences,
            vec![SchemaDivergence {
                entity: "subscriptions".to_owned(),
                column: "grace_period".to_owned(),
                kind: DivergenceKind::DeclaredByMigrationOnly,
            }]
        );
    }

    #[test]
    fn reconciliation_replays_migrations_in_file_path_order() {
        let create = symbol_node(
            "symbol:goose:m1",
            SymbolKind::SchemaMigration,
            "migrations/001_create.sql",
            "migrations.001_create",
            vec![table(
                "subscriptions",
                vec![column("id", "UUID"), column("legacy_flag", "BOOLEAN")],
            )],
        );
        let drop = symbol_node(
            "symbol:goose:m2",
            SymbolKind::SchemaMigration,
            "migrations/002_drop_legacy.sql",
            "migrations.002_drop_legacy",
            vec![SchemaTableDefinition {
                entity: "subscriptions".to_owned(),
                dropped_columns: vec!["legacy_flag".to_owned()],
                ..SchemaTableDefinition::default()
            }],
        );
        let orm = symbol_node(
            "symbol:python:models.Subscription",
            SymbolKind::Class,
            "models.py",
            "models.Subscription",
            vec![table("subscriptions", vec![column("id", "UUID")])],
        );
        // Order the nodes so the drop migration would be replayed before the
        // create migration if file-path ordering were not enforced.
        let divergences = reconcile_orm_and_migrations(&snapshot(vec![drop, create, orm]));
        assert!(
            divergences.is_empty(),
            "expected the drop to be replayed after the create: {divergences:?}"
        );
    }

    #[test]
    fn reconciliation_is_consistent_when_both_sources_agree() {
        let migration = symbol_node(
            "symbol:goose:m1",
            SymbolKind::SchemaMigration,
            "migrations/001.sql",
            "migrations.001",
            vec![table("subscriptions", vec![column("id", "UUID")])],
        );
        let orm = symbol_node(
            "symbol:python:models.Subscription",
            SymbolKind::Class,
            "models.py",
            "models.Subscription",
            vec![table("subscriptions", vec![column("id", "UUID")])],
        );
        assert!(reconcile_orm_and_migrations(&snapshot(vec![migration, orm])).is_empty());
    }
}
