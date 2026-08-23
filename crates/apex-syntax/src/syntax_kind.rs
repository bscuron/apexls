//! `SyntaxKind`: the single `Copy` kind type for every leaf (token) and
//! branch (node) in the tree, and the `rowan::Language` glue that lets
//! `rowan` work with it.
//!
//! Token variants are generated from the same list `apex_lexer::TokenKind`
//! declares, via a macro, so `from_token_kind` is *exhaustively matched*
//! against the real enum rather than a numeric cast: if a future lexer
//! change adds a `TokenKind` variant, this crate fails to compile until
//! the list here is updated. No node kind reuses a token name (rowan
//! needs one flat kind space), and no synthetic kinds exist for
//! parser-level token merges (`<=`, shift, ...) -- `BIN_EXPR` just holds
//! the raw operator leaf token(s) the lexer actually produced; deciding
//! "this was actually `<=`" is a typed-AST concern, not a tree-shape one.

use num_enum::TryFromPrimitive;

macro_rules! syntax_kind {
    (
        tokens { $($token:ident),* $(,)? }
        nodes { $($node:ident),* $(,)? }
    ) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, TryFromPrimitive)]
        #[repr(u16)]
        pub enum SyntaxKind {
            $($token,)*
            $($node,)*
        }

        impl SyntaxKind {
            /// Exhaustive: fails to compile if `apex_lexer::TokenKind` gains
            /// a variant not listed in this macro's `tokens { .. }` block.
            pub fn from_token_kind(kind: apex_lexer::TokenKind) -> SyntaxKind {
                match kind {
                    $(apex_lexer::TokenKind::$token => SyntaxKind::$token,)*
                }
            }
        }
    };
}

syntax_kind! {
    tokens {
        // ---- General keywords ----
        Abstract, After, Before, Break, Catch, Class, Continue, Delete, Do,
        Else, Enum, Extends, Final, Finally, For, Get, Global, If,
        Implements, Inherited, Insert, Instanceof, Interface, Merge, New,
        Null, On, Override, Private, Protected, Public, Return,
        SystemRunAs, Set, Sharing, Static, Super, Switch, Testmethod, This,
        Throw, Transient, Trigger, Try, Undelete, Update, Upsert, Virtual,
        Void, Webservice, When, While, With, Without,

        // ---- Apex generic types ----
        List, Map,

        // ---- DML keywords ----
        System, User,

        // ---- SOQL keywords ----
        Select, Count, From, As, Using, Scope, Where, Order, By, Limit,
        SoqlAnd, SoqlOr, Not, Avg, CountDistinct, Min, Max, Sum, Typeof,
        End, Then, Like, In, Includes, Excludes, Asc, Desc, Nulls, First,
        Last, Group, All, Rows, View, Having, Rollup, ToLabel, Offset,
        Data, Category, At, Above, Below, AboveOrBelow, SecurityEnforced,
        SystemMode, UserMode, Reference, Cube, Format, Tracking, Viewstat,
        Custom, Standard, Distance, Geolocation, Grouping, ConvertCurrency,
        Formula,

        // ---- SOQL date functions ----
        CalendarMonth, CalendarQuarter, CalendarYear, DayInMonth,
        DayInWeek, DayInYear, DayOnly, FiscalMonth, FiscalQuarter,
        FiscalYear, HourInDay, WeekInMonth, WeekInYear, ConvertTimezone,

        // ---- SOQL date formulas ----
        Yesterday, Today, Tomorrow, LastWeek, ThisWeek, NextWeek,
        LastMonth, ThisMonth, NextMonth, Last90Days, Next90Days,
        LastNDaysN, NextNDaysN, NDaysAgoN, NextNWeeksN, LastNWeeksN,
        NWeeksAgoN, NextNMonthsN, LastNMonthsN, NMonthsAgoN, ThisQuarter,
        LastQuarter, NextQuarter, NextNQuartersN, LastNQuartersN,
        NQuartersAgoN, ThisYear, LastYear, NextYear, NextNYearsN,
        LastNYearsN, NYearsAgoN, ThisFiscalQuarter, LastFiscalQuarter,
        NextFiscalQuarter, NextNFiscalQuartersN, LastNFiscalQuartersN,
        NFiscalQuartersAgoN, ThisFiscalYear, LastFiscalYear,
        NextFiscalYear, NextNFiscalYearsN, LastNFiscalYearsN,
        NFiscalYearsAgoN,

        // ---- SOSL keywords ----
        Find, Email, Name, Phone, Sidebar, Fields, Metadata, PricebookId,
        Network, Snippet, TargetLength, Division, Returning, Listview,
        Highlight, SpellCorrection,

        // ---- Literals ----
        DateLiteral, TimeLiteral, DateTimeLiteral, IntegralCurrencyLiteral,
        FindLiteral, FindLiteralAlt, IntegerLiteral, LongLiteral,
        NumberLiteral, BooleanLiteral, NullLiteral, StringLiteral,
        MultilineStringLiteral,

        // ---- Separators ----
        LParen, RParen, LBrace, RBrace, LBrack, RBrack, Semi, Comma, Dot,

        // ---- Operators ----
        Assign, Gt, Lt, Bang, Tilde, QuestionDot, Question, Colon, Equal,
        TripleEqual, NotEqual, LessAndGreater, TripleNotEqual, And, Or,
        Coal, Inc, Dec, Add, Sub, Mul, Div, BitAnd, BitOr, Caret, MapTo,
        AddAssign, SubAssign, MulAssign, DivAssign, AndAssign, OrAssign,
        XorAssign, LShiftAssign, RShiftAssign, URShiftAssign, AtSign,

        // ---- Identifier ----
        Identifier,

        // ---- Trivia ----
        Whitespace, LineComment, BlockComment, DocComment,

        // ---- Meta ----
        Unknown, Eof,
    }
    nodes {
        // ---- Roots (fragment-parse entry points) / recovery ----
        ExprRoot, StmtRoot, BlockRoot, ErrorNode,

        // ---- Types (expression/statement-scoped subset only) ----
        Type, TypeArgList, QualifiedName,

        // ---- Names ----
        // A declared-name position (class/interface/enum/method/field/
        // property/parameter/enum-constant/local-variable/catch-variable
        // name), wrapped in its own node so the typed AST layer can find
        // it without scanning for "the token after the introducing
        // keyword/type" -- unlike `NameExpr` (a name used as a value) or
        // `QualifiedName` (a dotted reference chain), `DeclName` never has
        // children of its own, just the one identifier-shaped token.
        // Named `DeclName` rather than `Name` because `Name` is already a
        // token kind (the SOSL `NAME` keyword) and rowan needs one flat
        // kind space shared by tokens and nodes.
        DeclName,

        // ---- Expressions ----
        LiteralExpr, NameExpr, ThisExpr, SuperExpr, ParenExpr, CastExpr,
        BinExpr, UnaryExpr, PostfixExpr, TernaryExpr, InstanceofExpr,
        FieldExpr, IndexExpr, CallExpr, MethodCallExpr, ArgList, NewExpr,
        ArrayInitializer, MapInitializer, MapEntry, SetInitializer,

        // ---- Statements ----
        Block, IfStmt, SwitchStmt, WhenClause, WhenValue, WhenLiteral,
        ForStmt, ForEachStmt, ForInit, ForUpdate, WhileStmt, DoWhileStmt,
        TryStmt, CatchClause, FinallyClause, ReturnStmt, ThrowStmt,
        BreakStmt, ContinueStmt, AccessLevelClause, InsertStmt,
        UpdateStmt, DeleteStmt, UndeleteStmt, UpsertStmt, MergeStmt,
        RunAsStmt, LocalVarDeclStmt, VarDeclarator, ExprStmt,

        // ---- Declarations (Phase 3) ----
        CompilationUnit, TriggerUnit, TriggerCase, TriggerBlock,
        ClassDecl, InterfaceDecl, EnumDecl, EnumConstantList,
        ClassBody, InterfaceBody, TypeRefList,
        Modifier, Annotation, AnnotationArgList, AnnotationArg,
        MethodDecl, ConstructorDecl, FieldDecl, PropertyDecl,
        PropertyAccessor, FormalParamList, FormalParam,

        // ---- SOQL (Phase 4) ----
        SoqlExpr, SoqlSelectList, SoqlSelectEntry, SoqlFieldName,
        SoqlFromList, SoqlUsingScope, SoqlWhereClause, SoqlLogicalExpr,
        SoqlComparison, SoqlValue, SoqlValueList, SoqlWithClause,
        SoqlFilteringExpr, SoqlDataCategorySelection, SoqlGroupBy,
        SoqlOrderBy, SoqlFieldOrder, SoqlLimit, SoqlOffset, SoqlForClause,
        SoqlUpdateList, SoqlBoundExpr, SoqlFunction, SoqlTypeOf,
        SoqlWhenClause, SoqlElseClause, SoqlFieldNameList, SoqlSubQuery,

        // ---- SOSL (Phase 4) ----
        SoslExpr, SoslClauses, SoslSearchGroup, SoslFieldSpecList,
        SoslFieldSpec, SoslWithClause, SoslFieldList, SoslUpdateList,
        SoslNetworkList,
    }
}

impl SyntaxKind {
    /// Mirrors `apex_lexer::TokenKind::is_trivia` (the token variants
    /// share names 1:1) -- useful for any tree walk that wants to ignore
    /// whitespace/comment leaves, e.g. shape comparisons in tests.
    pub fn is_trivia(self) -> bool {
        matches!(
            self,
            SyntaxKind::Whitespace
                | SyntaxKind::LineComment
                | SyntaxKind::BlockComment
                | SyntaxKind::DocComment
        )
    }
}

impl From<SyntaxKind> for rowan::SyntaxKind {
    fn from(kind: SyntaxKind) -> Self {
        rowan::SyntaxKind(kind as u16)
    }
}

/// Zero-variant marker type implementing `rowan::Language` for Apex --
/// `rowan::SyntaxNode<ApexLanguage>` etc. are what `apex-parser`,
/// `apex-printer`, and downstream consumers actually work with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ApexLanguage {}

impl rowan::Language for ApexLanguage {
    type Kind = SyntaxKind;

    fn kind_from_raw(raw: rowan::SyntaxKind) -> SyntaxKind {
        SyntaxKind::try_from(raw.0)
            .unwrap_or_else(|_| panic!("{} is not a valid SyntaxKind discriminant", raw.0))
    }

    fn kind_to_raw(kind: SyntaxKind) -> rowan::SyntaxKind {
        kind.into()
    }
}
