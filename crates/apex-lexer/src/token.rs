//! Token kinds and the `Token` type itself.
//!
//! Variants and grouping mirror `BaseApexLexer.g4` /
//! `ApexLexer.g4` (`options { caseInsensitive = true; }`) from
//! https://github.com/apex-dev-tools/apex-parser, section-by-section, so
//! the two can be diffed by eye. Apex keywords are matched
//! case-insensitively; see `keyword::lookup` for how that's done without
//! allocating a lowercased copy of the input.

/// A syntactic token kind. `Copy`/fieldless by design: token *text* is never
/// stored here, only recovered on demand via the `Token`'s span into the
/// original source, so `TokenKind` stays a single small integer to compare
/// and to pack into `Token`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u16)]
pub enum TokenKind {
    // ---- General keywords ----
    Abstract,
    After,
    Before,
    Break,
    Catch,
    Class,
    Continue,
    Delete,
    Do,
    Else,
    Enum,
    Extends,
    Final,
    Finally,
    For,
    Get,
    Global,
    If,
    Implements,
    Inherited,
    Insert,
    Instanceof,
    Interface,
    Merge,
    New,
    Null,
    On,
    Override,
    Private,
    Protected,
    Public,
    Return,
    /// `system.runas` — a single lexical token per the grammar, despite the dot.
    SystemRunAs,
    Set,
    Sharing,
    Static,
    Super,
    Switch,
    Testmethod,
    This,
    Throw,
    Transient,
    Trigger,
    Try,
    Undelete,
    Update,
    Upsert,
    Virtual,
    Void,
    Webservice,
    When,
    While,
    With,
    Without,

    // ---- Apex generic types (`Set` is shared with the property keyword above) ----
    List,
    Map,

    // ---- DML keywords ----
    System,
    User,

    // ---- SOQL keywords ----
    Select,
    Count,
    From,
    As,
    Using,
    Scope,
    Where,
    Order,
    By,
    Limit,
    /// `and` — named `SoqlAnd` in the grammar to disambiguate from `&&`.
    SoqlAnd,
    /// `or` — named `SoqlOr` in the grammar to disambiguate from `||`.
    SoqlOr,
    Not,
    Avg,
    CountDistinct,
    Min,
    Max,
    Sum,
    Typeof,
    End,
    Then,
    Like,
    In,
    Includes,
    Excludes,
    Asc,
    Desc,
    Nulls,
    First,
    Last,
    Group,
    All,
    Rows,
    View,
    Having,
    Rollup,
    ToLabel,
    Offset,
    Data,
    Category,
    At,
    Above,
    Below,
    AboveOrBelow,
    SecurityEnforced,
    SystemMode,
    UserMode,
    Reference,
    Cube,
    Format,
    Tracking,
    Viewstat,
    Custom,
    Standard,
    Distance,
    Geolocation,
    Grouping,
    /// `convertcurrency` — used in both SOQL and SOSL.
    ConvertCurrency,
    Formula,

    // ---- SOQL date functions ----
    CalendarMonth,
    CalendarQuarter,
    CalendarYear,
    DayInMonth,
    DayInWeek,
    DayInYear,
    DayOnly,
    FiscalMonth,
    FiscalQuarter,
    FiscalYear,
    HourInDay,
    WeekInMonth,
    WeekInYear,
    ConvertTimezone,

    // ---- SOQL date formulas ----
    Yesterday,
    Today,
    Tomorrow,
    LastWeek,
    ThisWeek,
    NextWeek,
    LastMonth,
    ThisMonth,
    NextMonth,
    Last90Days,
    Next90Days,
    LastNDaysN,
    NextNDaysN,
    NDaysAgoN,
    NextNWeeksN,
    LastNWeeksN,
    NWeeksAgoN,
    NextNMonthsN,
    LastNMonthsN,
    NMonthsAgoN,
    ThisQuarter,
    LastQuarter,
    NextQuarter,
    NextNQuartersN,
    LastNQuartersN,
    NQuartersAgoN,
    ThisYear,
    LastYear,
    NextYear,
    NextNYearsN,
    LastNYearsN,
    NYearsAgoN,
    ThisFiscalQuarter,
    LastFiscalQuarter,
    NextFiscalQuarter,
    NextNFiscalQuartersN,
    LastNFiscalQuartersN,
    NFiscalQuartersAgoN,
    ThisFiscalYear,
    LastFiscalYear,
    NextFiscalYear,
    NextNFiscalYearsN,
    LastNFiscalYearsN,
    NFiscalYearsAgoN,

    // ---- SOSL keywords ----
    Find,
    Email,
    Name,
    Phone,
    Sidebar,
    Fields,
    Metadata,
    PricebookId,
    Network,
    Snippet,
    TargetLength,
    Division,
    Returning,
    Listview,
    Highlight,
    SpellCorrection,

    // ---- Literals ----
    /// `yyyy-MM-dd`.
    DateLiteral,
    /// `HH:mm:ss[.SSS](Z|(+|-)HH[:mm])`.
    TimeLiteral,
    /// `DateLiteral 't' TimeLiteral`.
    DateTimeLiteral,
    /// e.g. `usd10` — three letters followed by digits. Lexically
    /// ambiguous with `Identifier`; the grammar resolves it by rule order
    /// (declared first) and ANTLR longest-match/first-alternative rules.
    /// Our lexer applies the same disambiguation explicitly (see the
    /// literal-scanning module) rather than relying on rule order.
    IntegralCurrencyLiteral,
    /// `[find '...']` — SOSL find clause, single-quoted body.
    FindLiteral,
    /// `[find {...}]` — SOSL find clause, brace-delimited body.
    FindLiteralAlt,
    IntegerLiteral,
    LongLiteral,
    NumberLiteral,
    BooleanLiteral,
    NullLiteral,
    StringLiteral,
    /// Salesforce Summer '26 triple-quoted multi-line string.
    MultilineStringLiteral,

    // ---- Separators ----
    LParen,
    RParen,
    LBrace,
    RBrace,
    LBrack,
    RBrack,
    Semi,
    Comma,
    Dot,

    // ---- Operators ----
    Assign,
    Gt,
    Lt,
    Bang,
    Tilde,
    QuestionDot,
    Question,
    Colon,
    Equal,
    TripleEqual,
    NotEqual,
    LessAndGreater,
    TripleNotEqual,
    And,
    Or,
    Coal,
    Inc,
    Dec,
    Add,
    Sub,
    Mul,
    Div,
    BitAnd,
    BitOr,
    Caret,
    MapTo,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    AndAssign,
    OrAssign,
    XorAssign,
    LShiftAssign,
    RShiftAssign,
    URShiftAssign,
    AtSign,

    // ---- Identifiers ----
    Identifier,

    // ---- Trivia ----
    Whitespace,
    LineComment,
    BlockComment,
    DocComment,

    // ---- Meta ----
    /// A byte sequence that matched no rule; the lexer never fails, it
    /// emits `Unknown` tokens (typically length 1) so callers can recover.
    Unknown,
    Eof,
}

impl TokenKind {
    /// Whitespace and comments: not significant to the grammar, but part of
    /// the token stream so a lossless CST can reattach them as trivia.
    #[inline]
    pub const fn is_trivia(self) -> bool {
        matches!(
            self,
            TokenKind::Whitespace
                | TokenKind::LineComment
                | TokenKind::BlockComment
                | TokenKind::DocComment
        )
    }
}

/// A single lexed token: its kind, plus a zero-copy `[start, start+len)`
/// byte span into the source the lexer was constructed with. No owned
/// string data is ever stored per-token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Token {
    pub kind: TokenKind,
    pub start: u32,
    pub len: u32,
}

impl Token {
    #[inline]
    pub const fn new(kind: TokenKind, start: u32, len: u32) -> Self {
        Token { kind, start, len }
    }

    #[inline]
    pub const fn end(&self) -> u32 {
        self.start + self.len
    }

    /// Recover this token's source text. Requires the same `source` the
    /// lexer was built from.
    #[inline]
    pub fn text<'src>(&self, source: &'src str) -> &'src str {
        &source[self.start as usize..self.end() as usize]
    }
}
