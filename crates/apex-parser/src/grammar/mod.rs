//! Grammar rules, one module per concern: types (expression/statement-
//! scoped subset only), expressions (precedence climbing), statements.

pub(crate) mod expressions;
pub(crate) mod statements;
pub(crate) mod types;
