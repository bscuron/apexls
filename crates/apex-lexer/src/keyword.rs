//! Case-insensitive keyword classification, matching `BaseApexLexer.g4` /
//! `ApexLexer.g4`'s `options { caseInsensitive = true; }`.
//!
//! Lookup never allocates on the heap: the scanned word is lowercased into
//! a fixed-size stack buffer, then resolved via a compile-time perfect
//! hash map (`phf`) — O(1), collision-free, no runtime table construction.
//!
//! Two grammar quirks are handled outside this table, deliberately, to
//! stay faithful to the reference lexer's actual (ANTLR longest-match)
//! behavior rather than a "cleaner" reinterpretation:
//!
//! - `system.runas` is matched by the grammar as a single fixed-string
//!   token distinct from `system`, so it can't be a simple word->kind
//!   entry here; see `Lexer::scan_identifier_or_keyword`, which special-
//!   cases the `system` + literal `.runas` lookahead.
//! - `null` is defined by *two* rules in the grammar (`NULL: 'null';` and
//!   later `NullLiteral: NULL;`); ANTLR resolves same-length ties in favor
//!   of whichever rule is declared first, so `NULL` always wins and
//!   `NullLiteral` is effectively dead. We only map `"null" -> Null` here
//!   to match that observable behavior; `TokenKind::NullLiteral` exists
//!   for grammar-doc parity but is never produced by the lexer.

use crate::token::TokenKind;
use phf::phf_map;

/// Longest keyword below (`next_n_fiscal_quarters` / `n_fiscal_quarters_ago`,
/// 22 chars) plus headroom. Any candidate word longer than this cannot be a
/// keyword and skips the lowercase-and-lookup path entirely.
pub(crate) const MAX_KEYWORD_LEN: usize = 24;

static KEYWORDS: phf::Map<&'static str, TokenKind> = phf_map! {
    // ---- General keywords ----
    "abstract" => TokenKind::Abstract,
    "after" => TokenKind::After,
    "before" => TokenKind::Before,
    "break" => TokenKind::Break,
    "catch" => TokenKind::Catch,
    "class" => TokenKind::Class,
    "continue" => TokenKind::Continue,
    "delete" => TokenKind::Delete,
    "do" => TokenKind::Do,
    "else" => TokenKind::Else,
    "enum" => TokenKind::Enum,
    "extends" => TokenKind::Extends,
    "final" => TokenKind::Final,
    "finally" => TokenKind::Finally,
    "for" => TokenKind::For,
    "get" => TokenKind::Get,
    "global" => TokenKind::Global,
    "if" => TokenKind::If,
    "implements" => TokenKind::Implements,
    "inherited" => TokenKind::Inherited,
    "insert" => TokenKind::Insert,
    "instanceof" => TokenKind::Instanceof,
    "interface" => TokenKind::Interface,
    "merge" => TokenKind::Merge,
    "new" => TokenKind::New,
    "null" => TokenKind::Null,
    "on" => TokenKind::On,
    "override" => TokenKind::Override,
    "private" => TokenKind::Private,
    "protected" => TokenKind::Protected,
    "public" => TokenKind::Public,
    "return" => TokenKind::Return,
    "set" => TokenKind::Set,
    "sharing" => TokenKind::Sharing,
    "static" => TokenKind::Static,
    "super" => TokenKind::Super,
    "switch" => TokenKind::Switch,
    "testmethod" => TokenKind::Testmethod,
    "this" => TokenKind::This,
    "throw" => TokenKind::Throw,
    "transient" => TokenKind::Transient,
    "trigger" => TokenKind::Trigger,
    "try" => TokenKind::Try,
    "undelete" => TokenKind::Undelete,
    "update" => TokenKind::Update,
    "upsert" => TokenKind::Upsert,
    "virtual" => TokenKind::Virtual,
    "void" => TokenKind::Void,
    "webservice" => TokenKind::Webservice,
    "when" => TokenKind::When,
    "while" => TokenKind::While,
    "with" => TokenKind::With,
    "without" => TokenKind::Without,

    // ---- Apex generic types ----
    "list" => TokenKind::List,
    "map" => TokenKind::Map,

    // ---- DML keywords ----
    "system" => TokenKind::System,
    "user" => TokenKind::User,

    // ---- SOQL keywords ----
    "select" => TokenKind::Select,
    "count" => TokenKind::Count,
    "from" => TokenKind::From,
    "as" => TokenKind::As,
    "using" => TokenKind::Using,
    "scope" => TokenKind::Scope,
    "where" => TokenKind::Where,
    "order" => TokenKind::Order,
    "by" => TokenKind::By,
    "limit" => TokenKind::Limit,
    "and" => TokenKind::SoqlAnd,
    "or" => TokenKind::SoqlOr,
    "not" => TokenKind::Not,
    "avg" => TokenKind::Avg,
    "count_distinct" => TokenKind::CountDistinct,
    "min" => TokenKind::Min,
    "max" => TokenKind::Max,
    "sum" => TokenKind::Sum,
    "typeof" => TokenKind::Typeof,
    "end" => TokenKind::End,
    "then" => TokenKind::Then,
    "like" => TokenKind::Like,
    "in" => TokenKind::In,
    "includes" => TokenKind::Includes,
    "excludes" => TokenKind::Excludes,
    "asc" => TokenKind::Asc,
    "desc" => TokenKind::Desc,
    "nulls" => TokenKind::Nulls,
    "first" => TokenKind::First,
    "last" => TokenKind::Last,
    "group" => TokenKind::Group,
    "all" => TokenKind::All,
    "rows" => TokenKind::Rows,
    "view" => TokenKind::View,
    "having" => TokenKind::Having,
    "rollup" => TokenKind::Rollup,
    "tolabel" => TokenKind::ToLabel,
    "offset" => TokenKind::Offset,
    "data" => TokenKind::Data,
    "category" => TokenKind::Category,
    "at" => TokenKind::At,
    "above" => TokenKind::Above,
    "below" => TokenKind::Below,
    "above_or_below" => TokenKind::AboveOrBelow,
    "security_enforced" => TokenKind::SecurityEnforced,
    "system_mode" => TokenKind::SystemMode,
    "user_mode" => TokenKind::UserMode,
    "reference" => TokenKind::Reference,
    "cube" => TokenKind::Cube,
    "format" => TokenKind::Format,
    "tracking" => TokenKind::Tracking,
    "viewstat" => TokenKind::Viewstat,
    "custom" => TokenKind::Custom,
    "standard" => TokenKind::Standard,
    "distance" => TokenKind::Distance,
    "geolocation" => TokenKind::Geolocation,
    "grouping" => TokenKind::Grouping,
    "convertcurrency" => TokenKind::ConvertCurrency,
    "formula" => TokenKind::Formula,

    // ---- SOQL date functions ----
    "calendar_month" => TokenKind::CalendarMonth,
    "calendar_quarter" => TokenKind::CalendarQuarter,
    "calendar_year" => TokenKind::CalendarYear,
    "day_in_month" => TokenKind::DayInMonth,
    "day_in_week" => TokenKind::DayInWeek,
    "day_in_year" => TokenKind::DayInYear,
    "day_only" => TokenKind::DayOnly,
    "fiscal_month" => TokenKind::FiscalMonth,
    "fiscal_quarter" => TokenKind::FiscalQuarter,
    "fiscal_year" => TokenKind::FiscalYear,
    "hour_in_day" => TokenKind::HourInDay,
    "week_in_month" => TokenKind::WeekInMonth,
    "week_in_year" => TokenKind::WeekInYear,
    "converttimezone" => TokenKind::ConvertTimezone,

    // ---- SOQL date formulas ----
    "yesterday" => TokenKind::Yesterday,
    "today" => TokenKind::Today,
    "tomorrow" => TokenKind::Tomorrow,
    "last_week" => TokenKind::LastWeek,
    "this_week" => TokenKind::ThisWeek,
    "next_week" => TokenKind::NextWeek,
    "last_month" => TokenKind::LastMonth,
    "this_month" => TokenKind::ThisMonth,
    "next_month" => TokenKind::NextMonth,
    "last_90_days" => TokenKind::Last90Days,
    "next_90_days" => TokenKind::Next90Days,
    "last_n_days" => TokenKind::LastNDaysN,
    "next_n_days" => TokenKind::NextNDaysN,
    "n_days_ago" => TokenKind::NDaysAgoN,
    "next_n_weeks" => TokenKind::NextNWeeksN,
    "last_n_weeks" => TokenKind::LastNWeeksN,
    "n_weeks_ago" => TokenKind::NWeeksAgoN,
    "next_n_months" => TokenKind::NextNMonthsN,
    "last_n_months" => TokenKind::LastNMonthsN,
    "n_months_ago" => TokenKind::NMonthsAgoN,
    "this_quarter" => TokenKind::ThisQuarter,
    "last_quarter" => TokenKind::LastQuarter,
    "next_quarter" => TokenKind::NextQuarter,
    "next_n_quarters" => TokenKind::NextNQuartersN,
    "last_n_quarters" => TokenKind::LastNQuartersN,
    "n_quarters_ago" => TokenKind::NQuartersAgoN,
    "this_year" => TokenKind::ThisYear,
    "last_year" => TokenKind::LastYear,
    "next_year" => TokenKind::NextYear,
    "next_n_years" => TokenKind::NextNYearsN,
    "last_n_years" => TokenKind::LastNYearsN,
    "n_years_ago" => TokenKind::NYearsAgoN,
    "this_fiscal_quarter" => TokenKind::ThisFiscalQuarter,
    "last_fiscal_quarter" => TokenKind::LastFiscalQuarter,
    "next_fiscal_quarter" => TokenKind::NextFiscalQuarter,
    "next_n_fiscal_quarters" => TokenKind::NextNFiscalQuartersN,
    "last_n_fiscal_quarters" => TokenKind::LastNFiscalQuartersN,
    "n_fiscal_quarters_ago" => TokenKind::NFiscalQuartersAgoN,
    "this_fiscal_year" => TokenKind::ThisFiscalYear,
    "last_fiscal_year" => TokenKind::LastFiscalYear,
    "next_fiscal_year" => TokenKind::NextFiscalYear,
    "next_n_fiscal_years" => TokenKind::NextNFiscalYearsN,
    "last_n_fiscal_years" => TokenKind::LastNFiscalYearsN,
    "n_fiscal_years_ago" => TokenKind::NFiscalYearsAgoN,

    // ---- SOSL keywords ----
    "find" => TokenKind::Find,
    "email" => TokenKind::Email,
    "name" => TokenKind::Name,
    "phone" => TokenKind::Phone,
    "sidebar" => TokenKind::Sidebar,
    "fields" => TokenKind::Fields,
    "metadata" => TokenKind::Metadata,
    "pricebookid" => TokenKind::PricebookId,
    "network" => TokenKind::Network,
    "snippet" => TokenKind::Snippet,
    "target_length" => TokenKind::TargetLength,
    "division" => TokenKind::Division,
    "returning" => TokenKind::Returning,
    "listview" => TokenKind::Listview,
    "highlight" => TokenKind::Highlight,
    "spell_correction" => TokenKind::SpellCorrection,

    // ---- Boolean literals (one token kind covers both spellings; the
    // literal's truth value is recovered from the token's source text) ----
    "true" => TokenKind::BooleanLiteral,
    "false" => TokenKind::BooleanLiteral,
};

/// Classify an ASCII byte span case-insensitively. Returns `None` (meaning:
/// treat it as a plain `Identifier`) when the word is too long to be any
/// keyword, isn't one, or (defensively) isn't ASCII — callers should only
/// ever pass ASCII spans (see the module docs on the `system.runas` and
/// `null` special cases for the two keyword-shaped rules handled
/// elsewhere), but a non-ASCII span is simply "not a keyword" rather than
/// something worth an `unsafe` fast path to rule out.
#[inline]
pub(crate) fn lookup(word: &[u8]) -> Option<TokenKind> {
    if word.is_empty() || word.len() > MAX_KEYWORD_LEN || !word.is_ascii() {
        return None;
    }

    let mut buf = [0u8; MAX_KEYWORD_LEN];
    let lower = &mut buf[..word.len()];
    for (dst, &src) in lower.iter_mut().zip(word) {
        *dst = src.to_ascii_lowercase();
    }
    // Safe: `word.is_ascii()` was checked above, so the lowercased copy is
    // valid UTF-8.
    let lower = std::str::from_utf8(lower).expect("ASCII input is valid UTF-8");
    KEYWORDS.get(lower).copied()
}
