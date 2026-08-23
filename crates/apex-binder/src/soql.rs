//! SOQL/SOSL field-chain resolution against `crate::schema_index::SchemaIndex`,
//! plus `:var` bind-expression resolution back through the enclosing
//! Apex scope (`BodyBinder::bind_expr`) -- the one place SOQL resolution
//! feeds into the ordinary Apex scope chain rather than the schema
//! index.
//!
//! A SOQL/SOSL query's "object context" (what object a bare field name
//! is relative to) comes from its own `FROM`/field-spec object, resolved
//! once and threaded through every nested field-name/function/subquery
//! walk below -- a relationship-field chain like `Owner.Name` hops from
//! one object to the next via [`SchemaIndex::field`]'s `reference_to`.

use crate::ptr::SyntaxPtr;
use crate::reference_table::Resolution;
use crate::resolve::BodyBinder;
use crate::scope::ScopeId;
use apex_syntax::ast::soql::{
    SoqlBoundExpr, SoqlComparison, SoqlCondition, SoqlExpr, SoqlFieldName, SoqlFieldOrFunction,
    SoqlFromList, SoqlFunction, SoqlLogicalExpr, SoqlSelectEntry, SoqlSubQuery, SoqlTypeOf,
    SoqlValue, SoslExpr, SoslFieldSpec,
};
use rowan::ast::AstNode;

/// A custom relationship name's field-schema-lookup form (`Batch__r` ->
/// `Batch__c`) -- the common, documented Salesforce convention for
/// custom lookup/master-detail fields; a standard relationship name
/// (`Owner`, `CreatedBy`, ...) has no such transform and is looked up
/// as-is. v1 doesn't model the full relationship-name table Salesforce
/// derives server-side, so a standard relationship name that doesn't
/// happen to equal its field's own API name (rare, but possible) won't
/// hop correctly -- an accepted, documented gap, not silently assumed
/// away.
fn relationship_field_api_name(segment: &str) -> String {
    if segment.len() > 3 && segment.to_ascii_lowercase().ends_with("__r") {
        format!("{}__c", &segment[..segment.len() - 3])
    } else {
        segment.to_string()
    }
}

fn resolve_object_ptr(binder: &mut BodyBinder<'_>, ptr: SyntaxPtr, name: &str) {
    crate::schema_index::resolve_object(binder.schema, binder.refs, ptr, name);
}

/// Resolves a dotted `SoqlFieldName` (`Owner.Name`, a bare `AccountId`,
/// ...) relative to `object` (the query's `FROM`/field-spec anchor),
/// hopping through each non-final segment's `reference_to` via
/// `SchemaIndex::field`. `object == None` (a bare SOSL field spec with
/// no resolvable object, or a query whose `FROM` itself didn't resolve)
/// -> `Unresolved`, since there's no schema to look anything up against
/// at all.
fn resolve_field_path(
    binder: &mut BodyBinder<'_>,
    field_name: &SoqlFieldName,
    object: Option<&str>,
) {
    let ptr = SyntaxPtr::new(field_name.syntax());
    let segments = field_name.segments();
    let Some(mut current_object) = object.map(str::to_string) else {
        binder.refs.set(ptr, Resolution::Unresolved);
        return;
    };
    if segments.is_empty() {
        binder.refs.set(ptr, Resolution::Unresolved);
        return;
    }

    let last = segments.len() - 1;
    for (i, seg) in segments.iter().enumerate() {
        let seg_text = seg.text().to_string();
        if i == last {
            let resolution = match binder.schema.field(&current_object, &seg_text) {
                Some(_) => Resolution::SchemaObject {
                    object: current_object,
                    field: Some(seg_text),
                },
                None => Resolution::UnknownSchema {
                    object: Some(current_object),
                    field: Some(seg_text),
                },
            };
            binder.refs.set(ptr, resolution);
            return;
        }

        let rel_field = relationship_field_api_name(&seg_text);
        match binder
            .schema
            .field(&current_object, &rel_field)
            .and_then(|f| f.reference_to.first())
        {
            Some(next) => current_object = next.clone(),
            None => {
                binder.refs.set(
                    ptr,
                    Resolution::UnknownSchema {
                        object: Some(current_object),
                        field: Some(seg_text),
                    },
                );
                return;
            }
        }
    }
}

fn bind_function(binder: &mut BodyBinder<'_>, func: &SoqlFunction, object: Option<&str>) {
    if let Some(field) = func.field_name() {
        resolve_field_path(binder, &field, object);
    }
    if let Some(nested) = func.nested_function() {
        bind_function(binder, &nested, object);
    }
}

fn bind_field_or_function(
    binder: &mut BodyBinder<'_>,
    f: &SoqlFieldOrFunction,
    object: Option<&str>,
) {
    match f {
        SoqlFieldOrFunction::Function(func) => bind_function(binder, func, object),
        SoqlFieldOrFunction::Field(field) => resolve_field_path(binder, field, object),
    }
}

fn bind_bound_expr(binder: &mut BodyBinder<'_>, scope: ScopeId, be: &SoqlBoundExpr) {
    if let Some(expr) = be.expr() {
        binder.bind_expr(scope, &expr);
    }
}

fn bind_value(binder: &mut BodyBinder<'_>, scope: ScopeId, value: &SoqlValue) {
    if let Some(be) = value.bound_expr() {
        bind_bound_expr(binder, scope, &be);
    }
    if let Some(sub) = value.sub_query() {
        bind_subquery(binder, scope, &sub);
    }
    if let Some(list) = value.value_list() {
        for v in list.values() {
            bind_value(binder, scope, &v);
        }
    }
}

fn bind_comparison(
    binder: &mut BodyBinder<'_>,
    scope: ScopeId,
    cmp: &SoqlComparison,
    object: Option<&str>,
) {
    if let Some(field) = cmp.field_name() {
        resolve_field_path(binder, &field, object);
    }
    if let Some(func) = cmp.function() {
        bind_function(binder, &func, object);
    }
    if let Some(value) = cmp.value() {
        bind_value(binder, scope, &value);
    }
}

fn bind_logical_expr(
    binder: &mut BodyBinder<'_>,
    scope: ScopeId,
    expr: &SoqlLogicalExpr,
    object: Option<&str>,
) {
    for cond in expr.conditions() {
        match cond {
            SoqlCondition::Comparison(cmp) => bind_comparison(binder, scope, &cmp, object),
            SoqlCondition::Group(group) => bind_logical_expr(binder, scope, &group, object),
        }
    }
}

fn bind_select_entry(
    binder: &mut BodyBinder<'_>,
    scope: ScopeId,
    entry: &SoqlSelectEntry,
    object: Option<&str>,
) {
    if let Some(field) = entry.field_name() {
        resolve_field_path(binder, &field, object);
    }
    if let Some(func) = entry.function() {
        bind_function(binder, &func, object);
    }
    if let Some(sub) = entry.sub_query() {
        bind_subquery(binder, scope, &sub);
    }
    if let Some(type_of) = entry.type_of() {
        bind_type_of(binder, &type_of, object);
    }
}

fn bind_type_of(binder: &mut BodyBinder<'_>, type_of: &SoqlTypeOf, object: Option<&str>) {
    if let Some(field) = type_of.field_name() {
        // The polymorphic relationship field itself, on the query's own
        // object (e.g. `TYPEOF What`, `What` a polymorphic lookup on the
        // `FROM` object).
        resolve_field_path(binder, &field, object);
    }
    for when in type_of.when_clauses() {
        // `WHEN <objectType> THEN <fieldList>` -- the when-clause's own
        // "field name" actually names a target *object* type per the
        // grammar, not a field on `object`, so it's resolved as an
        // object reference directly rather than hopped through
        // `object`'s schema.
        let when_object = when.field_name().map(|f| {
            let name = f.text();
            resolve_object_ptr(binder, SyntaxPtr::new(f.syntax()), &name);
            name
        });
        if let Some(then_fields) = when.then_fields() {
            for f in then_fields.fields() {
                resolve_field_path(binder, &f, when_object.as_deref());
            }
        }
    }
    if let Some(else_clause) = type_of.else_clause() {
        if let Some(fields) = else_clause.fields() {
            for f in fields.fields() {
                // Applies across every non-matched type, so there's no
                // single object to resolve against -- walked for corpus
                // coverage, always `Unresolved`.
                binder
                    .refs
                    .set(SyntaxPtr::new(f.syntax()), Resolution::Unresolved);
            }
        }
    }
}

fn bind_from_list(binder: &mut BodyBinder<'_>, from: Option<SoqlFromList>) -> Option<String> {
    let from = from?;
    let mut first = None;
    for entry in from.entries() {
        let name = entry.text();
        resolve_object_ptr(binder, SyntaxPtr::new(entry.syntax()), &name);
        if first.is_none() {
            first = Some(name);
        }
    }
    first
}

fn bind_subquery(binder: &mut BodyBinder<'_>, scope: ScopeId, sub: &SoqlSubQuery) {
    let object = bind_from_list(binder, sub.from_list());
    if let Some(select_list) = sub.select_list() {
        for entry in select_list.entries() {
            bind_select_entry(binder, scope, &entry, object.as_deref());
        }
    }
    if let Some(where_clause) = sub.where_clause() {
        if let Some(cond) = where_clause.condition() {
            bind_logical_expr(binder, scope, &cond, object.as_deref());
        }
    }
    if let Some(order_by) = sub.order_by() {
        for fo in order_by.field_orders() {
            if let Some(t) = fo.target() {
                bind_field_or_function(binder, &t, object.as_deref());
            }
        }
    }
    if let Some(limit) = sub.limit() {
        if let Some(be) = limit.bound_expr() {
            bind_bound_expr(binder, scope, &be);
        }
    }
}

pub(crate) fn bind_soql(binder: &mut BodyBinder<'_>, scope: ScopeId, sq: &SoqlExpr) {
    let object = bind_from_list(binder, sq.from_list());

    if let Some(select_list) = sq.select_list() {
        for entry in select_list.entries() {
            bind_select_entry(binder, scope, &entry, object.as_deref());
        }
    }
    if let Some(where_clause) = sq.where_clause() {
        if let Some(cond) = where_clause.condition() {
            bind_logical_expr(binder, scope, &cond, object.as_deref());
        }
    }
    for with in sq.with_clauses() {
        if let Some(cond) = with.condition() {
            bind_logical_expr(binder, scope, &cond, object.as_deref());
        }
        // `filtering_expr` (Salesforce Knowledge data category
        // selections) is left shallow, matching `apex-syntax`'s own
        // choice not to break `SoqlDataCategorySelection` down further.
    }
    if let Some(group_by) = sq.group_by() {
        for f in group_by.fields() {
            bind_field_or_function(binder, &f, object.as_deref());
        }
        if let Some(having) = group_by.having() {
            bind_logical_expr(binder, scope, &having, object.as_deref());
        }
    }
    if let Some(order_by) = sq.order_by() {
        for fo in order_by.field_orders() {
            if let Some(t) = fo.target() {
                bind_field_or_function(binder, &t, object.as_deref());
            }
        }
    }
    if let Some(limit) = sq.limit() {
        if let Some(be) = limit.bound_expr() {
            bind_bound_expr(binder, scope, &be);
        }
    }
    if let Some(offset) = sq.offset() {
        if let Some(be) = offset.bound_expr() {
            bind_bound_expr(binder, scope, &be);
        }
    }
}

fn bind_sosl_field_spec(binder: &mut BodyBinder<'_>, scope: ScopeId, spec: &SoslFieldSpec) {
    let object = spec.object().map(|o| {
        let name = o.text();
        resolve_object_ptr(binder, SyntaxPtr::new(o.syntax()), &name);
        name
    });
    if let Some(fields) = spec.field_list() {
        for f in fields.fields() {
            resolve_field_path(binder, &f, object.as_deref());
        }
        if let Some(func) = fields.function() {
            bind_function(binder, &func, object.as_deref());
        }
    }
    if let Some(where_clause) = spec.where_clause() {
        if let Some(cond) = where_clause.condition() {
            bind_logical_expr(binder, scope, &cond, object.as_deref());
        }
    }
    if let Some(order_by) = spec.order_by() {
        for fo in order_by.field_orders() {
            if let Some(t) = fo.target() {
                bind_field_or_function(binder, &t, object.as_deref());
            }
        }
    }
    if let Some(limit) = spec.limit() {
        if let Some(be) = limit.bound_expr() {
            bind_bound_expr(binder, scope, &be);
        }
    }
}

pub(crate) fn bind_sosl(binder: &mut BodyBinder<'_>, scope: ScopeId, ss: &SoslExpr) {
    if let Some(be) = ss.bound_expr() {
        bind_bound_expr(binder, scope, &be);
    }
    let Some(clauses) = ss.clauses() else {
        return;
    };
    if let Some(spec_list) = clauses.field_spec_list() {
        for spec in spec_list.specs() {
            bind_sosl_field_spec(binder, scope, &spec);
        }
    }
    for with in clauses.with_clauses() {
        if let Some(be) = with.bound_expr() {
            bind_bound_expr(binder, scope, &be);
        }
    }
    if let Some(limit) = clauses.limit() {
        if let Some(be) = limit.bound_expr() {
            bind_bound_expr(binder, scope, &be);
        }
    }
}
