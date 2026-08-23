//! Shared "keyword usable as identifier" token sets, matching the
//! reference grammar's `id` (general identifier positions: declaration
//! names, types, statement-level names) and `anyId` (post-dot member
//! names -- a strict superset of `id`, since accessing `x.new` or
//! `x.delete` is unambiguous even though *declaring* something named
//! `new`/`delete` would be, hence `id`'s narrower set).
//!
//! Both exist because many SOQL/SOSL/DML keywords double as extremely
//! common real-world identifiers (`System`, `User`, `Name`, `Rollup`,
//! `All`, `Trigger.new`, ...) -- Apex resolves this the way most
//! keyword-heavy languages do: only reserve a keyword in the specific
//! syntactic position it's actually needed in, and let it be an ordinary
//! identifier everywhere else. Missing this was the single largest gap
//! in Phase 3's first pass at whole-file NPSP parsing.

use crate::parser::Parser;
use apex_syntax::SyntaxKind;

pub(crate) fn at_id(p: &Parser<'_>) -> bool {
    is_id_kind(p.current())
}

pub(crate) fn at_any_id(p: &Parser<'_>) -> bool {
    is_any_id_kind(p.current())
}

/// Consumes the current token if it's `id`-shaped; otherwise records an
/// error without consuming, matching `Parser::expect`'s contract.
pub(crate) fn expect_id(p: &mut Parser<'_>) -> bool {
    if at_id(p) {
        p.bump();
        true
    } else {
        p.error(format!("expected a name, found {:?}", p.current()));
        false
    }
}

pub(crate) fn expect_any_id(p: &mut Parser<'_>) -> bool {
    if at_any_id(p) {
        p.bump();
        true
    } else {
        p.error(format!("expected a member name, found {:?}", p.current()));
        false
    }
}

/// `anyId` minus the "Apex Keywords" block that would be ambiguous or
/// nonsensical as a *declared* name (`class`, `new`, `return`, ...) --
/// still fine to *access* via `.new`, which is why `anyId` allows it and
/// `id` doesn't. Exposed (not just `at_id`) for callers doing
/// offset-based lookahead (`p.nth(n)`) rather than checking the current
/// token.
pub(crate) fn is_id_kind(k: SyntaxKind) -> bool {
    is_any_id_kind(k)
        && !matches!(
            k,
            SyntaxKind::Abstract
                | SyntaxKind::Break
                | SyntaxKind::Catch
                | SyntaxKind::Class
                | SyntaxKind::Continue
                | SyntaxKind::Delete
                | SyntaxKind::Do
                | SyntaxKind::Else
                | SyntaxKind::Enum
                | SyntaxKind::Extends
                | SyntaxKind::Final
                | SyntaxKind::Finally
                | SyntaxKind::For
                | SyntaxKind::Global
                | SyntaxKind::If
                | SyntaxKind::Implements
                | SyntaxKind::Insert
                | SyntaxKind::Interface
                | SyntaxKind::List
                | SyntaxKind::Map
                | SyntaxKind::Merge
                | SyntaxKind::New
                | SyntaxKind::Null
                | SyntaxKind::On
                | SyntaxKind::Override
                | SyntaxKind::Private
                | SyntaxKind::Protected
                | SyntaxKind::Public
                | SyntaxKind::Return
                | SyntaxKind::Static
                | SyntaxKind::Super
                | SyntaxKind::Testmethod
                | SyntaxKind::This
                | SyntaxKind::Throw
                | SyntaxKind::Try
                | SyntaxKind::Undelete
                | SyntaxKind::Update
                | SyntaxKind::Upsert
                | SyntaxKind::Virtual
                | SyntaxKind::Webservice
                | SyntaxKind::While
        )
}

fn is_any_id_kind(k: SyntaxKind) -> bool {
    matches!(
        k,
        SyntaxKind::Identifier
            // "Apex Keywords" block (anyId only; excluded from id above)
            | SyntaxKind::Abstract | SyntaxKind::After | SyntaxKind::Before | SyntaxKind::Break
            | SyntaxKind::Catch | SyntaxKind::Class | SyntaxKind::Continue | SyntaxKind::Delete
            | SyntaxKind::Do | SyntaxKind::Else | SyntaxKind::Enum | SyntaxKind::Extends
            | SyntaxKind::Final | SyntaxKind::Finally | SyntaxKind::For | SyntaxKind::Get
            | SyntaxKind::Global | SyntaxKind::If | SyntaxKind::Implements | SyntaxKind::Inherited
            | SyntaxKind::Insert | SyntaxKind::Instanceof | SyntaxKind::Interface
            | SyntaxKind::List | SyntaxKind::Map | SyntaxKind::Merge | SyntaxKind::New
            | SyntaxKind::Null | SyntaxKind::On | SyntaxKind::Override | SyntaxKind::Private
            | SyntaxKind::Protected | SyntaxKind::Public | SyntaxKind::Return | SyntaxKind::Set
            | SyntaxKind::Sharing | SyntaxKind::Static | SyntaxKind::Super | SyntaxKind::Switch
            | SyntaxKind::Testmethod | SyntaxKind::This | SyntaxKind::Throw
            | SyntaxKind::Transient | SyntaxKind::Trigger | SyntaxKind::Try
            | SyntaxKind::Undelete | SyntaxKind::Update | SyntaxKind::Upsert
            | SyntaxKind::Virtual | SyntaxKind::Webservice | SyntaxKind::When
            | SyntaxKind::While | SyntaxKind::With | SyntaxKind::Without
            // DML keywords
            | SyntaxKind::User | SyntaxKind::System
            // SOQL currency-shaped literal (also a valid bare id per the grammar)
            | SyntaxKind::IntegralCurrencyLiteral
            // SOQL keywords
            | SyntaxKind::Select | SyntaxKind::Count | SyntaxKind::From | SyntaxKind::As
            | SyntaxKind::Using | SyntaxKind::Scope | SyntaxKind::Where | SyntaxKind::Order
            | SyntaxKind::By | SyntaxKind::Limit | SyntaxKind::SoqlAnd | SyntaxKind::SoqlOr
            | SyntaxKind::Not | SyntaxKind::Avg | SyntaxKind::CountDistinct | SyntaxKind::Min
            | SyntaxKind::Max | SyntaxKind::Sum | SyntaxKind::Typeof | SyntaxKind::End
            | SyntaxKind::Then | SyntaxKind::Like | SyntaxKind::In | SyntaxKind::Includes
            | SyntaxKind::Excludes | SyntaxKind::Asc | SyntaxKind::Desc | SyntaxKind::Nulls
            | SyntaxKind::First | SyntaxKind::Last | SyntaxKind::Group | SyntaxKind::All
            | SyntaxKind::Rows | SyntaxKind::View | SyntaxKind::Having | SyntaxKind::Rollup
            | SyntaxKind::ToLabel | SyntaxKind::Offset | SyntaxKind::Data
            | SyntaxKind::Category | SyntaxKind::At | SyntaxKind::Above | SyntaxKind::Below
            | SyntaxKind::AboveOrBelow | SyntaxKind::SecurityEnforced
            | SyntaxKind::SystemMode | SyntaxKind::UserMode | SyntaxKind::Reference
            | SyntaxKind::Cube | SyntaxKind::Format | SyntaxKind::Tracking
            | SyntaxKind::Viewstat | SyntaxKind::Standard | SyntaxKind::Custom
            | SyntaxKind::Distance | SyntaxKind::Geolocation | SyntaxKind::Grouping
            | SyntaxKind::Formula | SyntaxKind::ConvertCurrency
            // SOQL date functions
            | SyntaxKind::CalendarMonth | SyntaxKind::CalendarQuarter | SyntaxKind::CalendarYear
            | SyntaxKind::DayInMonth | SyntaxKind::DayInWeek | SyntaxKind::DayInYear
            | SyntaxKind::DayOnly | SyntaxKind::FiscalMonth | SyntaxKind::FiscalQuarter
            | SyntaxKind::FiscalYear | SyntaxKind::HourInDay | SyntaxKind::WeekInMonth
            | SyntaxKind::WeekInYear | SyntaxKind::ConvertTimezone
            // SOQL date formulas
            | SyntaxKind::Yesterday | SyntaxKind::Today | SyntaxKind::Tomorrow
            | SyntaxKind::LastWeek | SyntaxKind::ThisWeek | SyntaxKind::NextWeek
            | SyntaxKind::LastMonth | SyntaxKind::ThisMonth | SyntaxKind::NextMonth
            | SyntaxKind::Last90Days | SyntaxKind::Next90Days | SyntaxKind::LastNDaysN
            | SyntaxKind::NextNDaysN | SyntaxKind::NDaysAgoN | SyntaxKind::NextNWeeksN
            | SyntaxKind::LastNWeeksN | SyntaxKind::NWeeksAgoN | SyntaxKind::NextNMonthsN
            | SyntaxKind::LastNMonthsN | SyntaxKind::NMonthsAgoN | SyntaxKind::ThisQuarter
            | SyntaxKind::LastQuarter | SyntaxKind::NextQuarter | SyntaxKind::NextNQuartersN
            | SyntaxKind::LastNQuartersN | SyntaxKind::NQuartersAgoN | SyntaxKind::ThisYear
            | SyntaxKind::LastYear | SyntaxKind::NextYear | SyntaxKind::NextNYearsN
            | SyntaxKind::LastNYearsN | SyntaxKind::NYearsAgoN | SyntaxKind::ThisFiscalQuarter
            | SyntaxKind::LastFiscalQuarter | SyntaxKind::NextFiscalQuarter
            | SyntaxKind::NextNFiscalQuartersN | SyntaxKind::LastNFiscalQuartersN
            | SyntaxKind::NFiscalQuartersAgoN | SyntaxKind::ThisFiscalYear
            | SyntaxKind::LastFiscalYear | SyntaxKind::NextFiscalYear
            | SyntaxKind::NextNFiscalYearsN | SyntaxKind::LastNFiscalYearsN
            | SyntaxKind::NFiscalYearsAgoN
            // SOSL keywords
            | SyntaxKind::Find | SyntaxKind::Email | SyntaxKind::Name | SyntaxKind::Phone
            | SyntaxKind::Sidebar | SyntaxKind::Fields | SyntaxKind::Metadata
            | SyntaxKind::PricebookId | SyntaxKind::Network | SyntaxKind::Snippet
            | SyntaxKind::TargetLength | SyntaxKind::Division | SyntaxKind::Returning
            | SyntaxKind::Listview | SyntaxKind::Highlight | SyntaxKind::SpellCorrection
    )
}
