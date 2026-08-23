//! Grammar rules, one module per concern: types (expression/statement-
//! scoped subset only), expressions (precedence climbing), statements,
//! declarations (Phase 3: classes/interfaces/enums/triggers/members).

pub(crate) mod declarations;
pub(crate) mod expressions;
pub(crate) mod ids;
pub(crate) mod soql;
pub(crate) mod statements;
pub(crate) mod types;
