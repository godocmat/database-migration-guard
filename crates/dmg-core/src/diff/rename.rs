use std::{borrow::Cow, collections::BTreeMap};

use crate::inspection::is_catalog_identifier;

use super::{input::table_id, *};

/// Only reviewed mappings and the owner-qualified identity changes they entail.
pub(super) struct Renames<'a> {
    forward: BTreeMap<&'a DatabaseObject, &'a DatabaseObject>,
    backward: BTreeMap<&'a DatabaseObject, &'a DatabaseObject>,
    table_forward: BTreeMap<QualifiedName, QualifiedName>,
    table_backward: BTreeMap<QualifiedName, QualifiedName>,
}

impl<'a> Renames<'a> {
    pub(super) fn new(
        mappings: &'a [RenameMapping],
        before: &Inventory<'_>,
        after: &Inventory<'_>,
    ) -> Result<Self, DiffError> {
        let mut renames = Self {
            forward: BTreeMap::new(),
            backward: BTreeMap::new(),
            table_forward: BTreeMap::new(),
            table_backward: BTreeMap::new(),
        };
        for (offset, mapping) in mappings.iter().enumerate() {
            let index = offset + 1;
            let from = &mapping.from;
            let to = &mapping.to;
            if from == to
                || from.kind != to.kind
                || from.schema != to.schema
                || !valid_endpoint(from)
                || !valid_endpoint(to)
            {
                return Err(DiffError::InvalidRename { index });
            }
            if renames.forward.insert(from, to).is_some()
                || renames.backward.insert(to, from).is_some()
            {
                return Err(DiffError::ConflictingRename { index });
            }
            if let (Some(Record::Table(left)), Some(Record::Table(right))) =
                (before.objects.get(from), after.objects.get(to))
            {
                renames
                    .table_forward
                    .insert(left.name.clone(), right.name.clone());
                renames
                    .table_backward
                    .insert(right.name.clone(), left.name.clone());
            }
        }
        for (offset, mapping) in mappings.iter().enumerate() {
            let index = offset + 1;
            let Some(left) = before.objects.get(&mapping.from).copied() else {
                return Err(DiffError::UnverifiedRename { index });
            };
            let Some(right) = after.objects.get(&mapping.to).copied() else {
                return Err(DiffError::UnverifiedRename { index });
            };
            if !renames.owner_matches(left, right) || !same_kind(left, right) {
                return Err(DiffError::RenameOwnerOrKind { index });
            }

            let old_name_after = renames.forward_with_owner(&mapping.from);
            let new_name_before = renames.backward_with_owner(&mapping.to);
            if old_name_after.as_ref() == &mapping.to || new_name_before.as_ref() == &mapping.from {
                return Err(DiffError::InvalidRename { index });
            }
            if !absent(after, &old_name_after, renames.owner_forward(left.owner()))
                || !absent(
                    before,
                    &new_name_before,
                    renames.owner_backward(right.owner()),
                )
            {
                return Err(DiffError::UnverifiedRename { index });
            }
        }
        Ok(renames)
    }

    pub(super) fn is_explicit(&self, identity: &DatabaseObject) -> bool {
        self.forward.contains_key(identity)
    }

    pub(super) fn forward<'id>(
        &'id self,
        identity: &'id DatabaseObject,
    ) -> Cow<'id, DatabaseObject> {
        self.forward.get(identity).map_or_else(
            || self.forward_with_owner(identity),
            |mapped| Cow::Borrowed(*mapped),
        )
    }

    pub(super) fn backward<'id>(
        &'id self,
        identity: &'id DatabaseObject,
    ) -> Cow<'id, DatabaseObject> {
        self.backward.get(identity).map_or_else(
            || self.backward_with_owner(identity),
            |mapped| Cow::Borrowed(*mapped),
        )
    }

    fn forward_with_owner<'id>(&self, identity: &'id DatabaseObject) -> Cow<'id, DatabaseObject> {
        remap_owner(identity, &self.table_forward)
    }

    fn backward_with_owner<'id>(&self, identity: &'id DatabaseObject) -> Cow<'id, DatabaseObject> {
        remap_owner(identity, &self.table_backward)
    }

    pub(super) fn table_forward<'table>(
        &'table self,
        name: &'table QualifiedName,
    ) -> &'table QualifiedName {
        self.table_forward.get(name).unwrap_or(name)
    }

    pub(super) fn table_backward<'table>(
        &'table self,
        name: &'table QualifiedName,
    ) -> &'table QualifiedName {
        self.table_backward.get(name).unwrap_or(name)
    }

    pub(super) fn owner_forward<'table>(
        &'table self,
        owner: Option<&'table QualifiedName>,
    ) -> Option<&'table QualifiedName> {
        owner.map(|owner| self.table_forward(owner))
    }

    pub(super) fn owner_backward<'table>(
        &'table self,
        owner: Option<&'table QualifiedName>,
    ) -> Option<&'table QualifiedName> {
        owner.map(|owner| self.table_backward(owner))
    }

    pub(super) fn owner_matches(&self, before: Record<'_>, after: Record<'_>) -> bool {
        self.owner_forward(before.owner()) == after.owner()
    }

    pub(super) fn column_matches(
        &self,
        before_table: &QualifiedName,
        before_name: &str,
        after_name: &str,
    ) -> bool {
        if self.forward.is_empty() {
            return before_name == after_name;
        }
        let mut id = table_id(before_table);
        id.kind = ObjectKind::Column;
        id.member = Some(before_name.into());
        self.forward
            .get(&id)
            .and_then(|to| to.member.as_deref())
            .unwrap_or(before_name)
            == after_name
    }
}

fn valid_endpoint(id: &DatabaseObject) -> bool {
    is_catalog_identifier(&id.name)
        && id.schema.as_deref().is_some_and(is_catalog_identifier)
        && match id.kind {
            ObjectKind::Relation | ObjectKind::Index => id.member.is_none(),
            ObjectKind::Column | ObjectKind::Constraint => {
                id.member.as_deref().is_some_and(is_catalog_identifier)
            }
            ObjectKind::Schema | ObjectKind::Type => false,
        }
}

fn same_kind(before: Record<'_>, after: Record<'_>) -> bool {
    match (before, after) {
        (Record::Table(left), Record::Table(right)) => left.kind == right.kind,
        (Record::Constraint(_, left), Record::Constraint(_, right)) => left.kind == right.kind,
        _ => true,
    }
}

fn absent(other: &Inventory<'_>, id: &DatabaseObject, owner: Option<&QualifiedName>) -> bool {
    let sibling_kind = match id.kind {
        ObjectKind::Relation => Some(ObjectKind::Index),
        ObjectKind::Index => Some(ObjectKind::Relation),
        _ => None,
    };
    sibling_kind.is_none_or(|kind| {
        !other
            .objects
            .contains_key(&DatabaseObject { kind, ..id.clone() })
    }) && !other.objects.contains_key(id)
        && other.proves_absence(id, owner)
}

fn remap_owner<'id>(
    id: &'id DatabaseObject,
    tables: &BTreeMap<QualifiedName, QualifiedName>,
) -> Cow<'id, DatabaseObject> {
    if !matches!(
        id.kind,
        ObjectKind::Column | ObjectKind::Constraint | ObjectKind::Index
    ) || id.member.is_none()
        || tables.is_empty()
    {
        return Cow::Borrowed(id);
    }
    let Some(schema) = &id.schema else {
        return Cow::Borrowed(id);
    };
    let owner = QualifiedName {
        schema: schema.clone(),
        name: id.name.clone(),
    };
    tables.get(&owner).map_or(Cow::Borrowed(id), |mapped| {
        Cow::Owned(DatabaseObject {
            kind: id.kind,
            schema: Some(mapped.schema.clone()),
            name: mapped.name.clone(),
            member: id.member.clone(),
        })
    })
}
