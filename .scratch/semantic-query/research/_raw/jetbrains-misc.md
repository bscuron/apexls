# Structural search-and-replace tools — research findings

Research against primary documentation only (official docs, GitHub README, source) for a design doc on structural search-and-replace tools. JetBrains SSR covered first and most thoroughly, PMD-Apex and Refaster thoroughly, gogrep and rslint briefly.

## 1. JetBrains Structural Search and Replace (SSR) — most thorough

Sources: https://www.jetbrains.com/help/idea/structural-search-and-replace.html , https://www.jetbrains.com/help/idea/search-templates.html , https://www.jetbrains.com/help/idea/structural-search-and-replace-examples.html , https://www.jetbrains.com/help/idea/tutorial-work-with-structural-search-and-replace.html

Note: `structural-search-filters.html` and `creating-editing-search-templates.html` both 404 on the current JetBrains site — the current doc IA folds "filters" content into `search-templates.html`, and template-editing content into `structural-search-and-replace.html`. These are the correct current URLs to cite instead of the ones originally guessed.

### Core concept (verbatim)
> "A conventional search process does not take into account the syntax and semantics of the source code."

SSR parses the search template with the real language parser and matches it against the **PSI tree** (Program Structure Interface) of the source — not text/regex. Confirmed via JetBrains' own platform SDK docs (plugins.jetbrains.com/docs/intellij/psi.html) and IntelliJ Community source (`Matcher.java` in `platform/structuralsearch`):
> "The pattern in structural search is valid code that is parsed by the parser, producing the PSI tree. The tree is then matched on source code trees to find fragments that are suitable for the specified constraints."

So: search templates are themselves syntactically valid (or near-valid) source-language fragments, parsed into PSI, then matched structurally against the PSI of the target file. Fragments (a statement, an expression, a tag) are matched at whatever grammar entry point the template fragment itself parses as.

### Verbatim example templates (from structural-search-and-replace-examples.html)
- `$Instance$.$MethodCall$($Parameter$)` — "matches method call expressions. If the number of occurrences is zero, it means that a method call can be omitted." (This is THE canonical example, e.g. matching `System.out.println(...)`.)
- `@Deprecated $Instance$.$MethodCall$($Parameter$)` — "find deprecated methods and use it as prototype for creating other annotated method templates"
- `synchronized ($parameter$){ $statement$; }` — "search for all the synchronizable methods with an arbitrary number of parameters, but with only one line of code in the body"
- `$Statement$;` — "find sequences of statements that contain up to the specified number of elements"
- `if ($Expr$) { $ThenStatements$; } else { $ElseStatements$; }`
- Replace pair — Search: `$Statements$;` → Replace: `try { $Statements$; } catch(Exception ex) { }` — "replace a statement with a try/catch/finally construct"
- `class $Clazz$ extends $AnotherClass$ {}` — finding all descendants of a class
- `class $Clazz$ implements $SomeInterface$ {}` — all classes that implement a certain interface
- `class $a$ { public void $show$(); }` — "look for the different implementations of the same interface method"
- `class $Class$ { @Modifier("packageLocal") @Modifier("Instance" ) $ReturnType$ $MethodName$($ParameterType$ $Parameter$); } }` — "finding all methods with the visibility modifiers package local and instance"
- `LOG.debug($params$);` — logging statements example for "Contained in Constraints" field usage
- `<$tag$/>` — "simplest template to search for a tag"
- `<$tag$ $attribute$=$value$ />` — "searching in XML and HTML" for tags with specific attributes and numeric values
- `<$tag$ $attribute$="$value$">` — HTML template for deleting lines with id attribute greater than 2
- Replace pair — Search: `<$tag$ $attribute$="$value$">` → Replace: `$to_lower_case$` — "Convert uppercase values of the class attribute in li tags to lowercase"
- `new java.lang.RuntimeException($x$)` — the canonical Count-modifier example

### "Match anything" / sequences — the distinctive design
There is **no separate "varargs" or "any sequence" syntax**. Repetition is expressed purely as a **constraint on a variable**: the **Count modifier**.
> "The Count modifier specifies a number of occurrences." You set min/max on a variable (e.g. click `$Parameter$` → Add modifier → Count → set min/max). "To set the unlimited maximum count, provide an empty value in the modifier field."

Example: `$Parameter$` with count `min=1 max=1` forces exactly one argument; leaving max empty (∞) lets `$Parameter$` match zero-or-more call arguments, i.e., varargs-style matching. The UI shows this as `[0,∞]` next to the variable. Same mechanism handles "sequence of statements" (`$Statement$;` with an unbounded count matches an arbitrary run of statements) — there's no distinct "sequence operator" the way gogrep has `$*_`. This is a genuinely distinctive design choice: **one filter (count/min-max range) unifies "optional", "exactly N", and "arbitrary sequence."**

### Full filter list (search-templates.html) — verbatim descriptions
> "Each search or replace template consists of variables `$variable_name$` to which you can add a condition (modifier) to narrow your search results."

1. **Count** — "The Count modifier specifies a number of occurrences." (min/max range; empty max = unlimited)
2. **Text** — "The Text modifier checks the variable against regular expressions or plain text." Includes matching by fully qualified name, and a **"Within type hierarchy"** checkbox option (also appears associated with the Type filter per other references) to widen matching to sub/supertypes.
3. **Type** — "The Type modifier adds a type of the value or expression that is expected for the specified variable." (e.g., restricting `$expression$` to `int` catches boxing operations.)
4. **Reference** — "The Reference modifier lets you reference some other search template in the variable," i.e. reuse a saved/predefined template as a sub-constraint.
5. **Script** — "The Script modifier adds Groovy script constraints to the search template." Used for constructs plain filters can't express, e.g. "constructors with the specified number of parameters," "members with the specified visibility modifiers." All matched variables are exposed to the Groovy script as PSI nodes: "All variables used in a template can be accessed from script constraints... this variable is in fact a node in the PSI tree" (accessible via `variable.name`, `variable.text`, etc.).

**Not independently confirmed on current live docs:** separate named filters called "contained in constructor," "read/write access," or "formal argument type" as distinct top-level modifier names. The current live page enumerates exactly the five above (Count, Text, Type, Reference, Script). Those other names may be older UI labels (pre-2020 IntelliJ) or Script-level predicates rather than dedicated filter types; the "Contained in Constraints" field mentioned in the `LOG.debug($params$);` example is a related-but-distinct scoping field (restrict matches to within a certain containing element), not one of the five modifiers above. Flag this for the design doc as unconfirmed-verbatim rather than asserted — worth a manual check in the actual IDE UI before citing.

### `$var$` reuse / back-reference
No dedicated "same as" filter is enumerated among the five official modifiers. Reusing the same `$variable$` name at multiple positions in one template requires those positions to match structurally-equal/same code (this is implicit in how PSI variable-binding works — same variable name = same bound node position), and the **Script** modifier is the documented way to assert "matches same text as another variable" explicitly via Groovy predicate over `.text`/`.name` on two variables, since there is no dedicated "same as" filter enumerated among the five official modifiers.

### Replace side (verbatim, from structural-search-and-replace.html)
> "Shorten fully-qualified names - replaces fully qualified class names with short names and imports."
> "Reformat - automatically formats the replaced code."
> "Use static import - uses static import in replacement when possible."

Comment preservation is not explicitly documented on the pages fetched — not confirmed either way, don't assert it.

### Sharing
> "You can share a search template with your peers by exporting or importing it."
This is the one sharing mechanism documented — export/import of the template (as a config snippet), not a portable text format or CLI-embeddable pattern.

### Does it use the real language parser?
Yes — confirmed above via PSI. SSR is not a text/regex tool; it parses both the template and the target file with the actual language grammar and matches structurally on the resulting trees.

### Known ergonomic complaints (design-doc framing, not verbatim doc claims)
- GUI-only: everything above is driven through the Structural Search / Structural Replace dialogs (Edit → Find → Search Structurally); no documented CLI/batch invocation.
- Sharing is via IDE export/import, not a plain-text pattern file you'd put in a repo or CI.
- Syntax is verbose: every hole is `$Name$` with a trailing `$`, and match-anywhere/sequence semantics live in a separate modifier dialog rather than being visible inline in the pattern text — you can't tell from the pattern alone that `$Parameter$` means "one or more," you have to open its filter.

---

## 2. gogrep (github.com/mvdan/gogrep, and quasilyte/gogrep) — archived

Source: https://raw.githubusercontent.com/mvdan/gogrep/master/README.md (verbatim, fetched in full)

**Status: archived / no longer developed.** README: "Note that this project is **no longer being developed**. See https://github.com/mvdan/gogrep/issues/64 for more details."

Full verbatim README:
```
gogrep

	GO111MODULE=on go get mvdan.cc/gogrep

Search for Go code using syntax trees.

	gogrep -x 'if $x != nil { return $x, $*_ }'

Note that this project is no longer being developed.
See https://github.com/mvdan/gogrep/issues/64 for more details.

Instructions

	usage: gogrep commands [packages]

A command is of the form "-A pattern", where -A is one of:

       -x  find all nodes matching a pattern
       -g  discard nodes not matching a pattern
       -v  discard nodes matching a pattern
       -a  filter nodes by certain attributes
       -s  substitute with a given syntax tree
       -w  write source back to disk or stdout

A pattern is a piece of Go code which may include wildcards. It can be:

       a statement (many if split by semicolons)
       an expression (many if split by commas)
       a type expression
       a top-level declaration (var, func, const)
       an entire file

Wildcards consist of $ and a name. All wildcards with the same name
within an expression must match the same node, excluding "_". Example:

       $x.$_ = $x // assignment of self to a field in self

If * is before the name, it will match any number of nodes. Example:

       fmt.Fprintf(os.Stdout, $*_) // all Fprintfs on stdout

* can also be used to match optional nodes, like:

	for $*_ { $*_ }    // will match all for loops
	if $*_; $b { $*_ } // will match all ifs with condition $b

The nodes resulting from applying the commands will be printed line by
line to standard output.

Here are two simple examples of the -a operand:

       gogrep -x '$x + $y'                   // will match both numerical and string "+" operations
       gogrep -x '$x + $y' -a 'type(string)' // matches only string concatenations
```

Command-chaining model: commands are chained flags (`-x`/`-g`/`-v`/`-a`/`-s`/`-w`), each a "-A pattern" pair, applied to the invocation's node set in sequence (find → filter-in/out → attribute-filter → substitute → write) — a pipeline expressed entirely as repeated CLI flags on one invocation, not separate piped processes.

Fragment parsing: the README states patterns can be a statement, expression, type expression, top-level declaration, or entire file — gogrep tries these entry points against Go's own `go/parser` to figure out what grammar rule a given pattern fragment is (this multi-entry-point trial-parse approach is stated structurally in the README's pattern-kind list; a more explicit line describing the exact trial-and-error order beyond that list was not found). Note also **quasilyte/gogrep** is a separate, actively-maintained fork/rewrite (used inside `go-critic`) — the README quoted above is specifically mvdan's original, archived version; its raw README content was not retrievable (GitHub's rendered page didn't expose raw markdown to WebFetch), only confirmed it exists at github.com/quasilyte/gogrep.

---

## 3. Refaster (Error Prone) templates

Source: https://errorprone.info/docs/refaster (fetched, substantially verbatim)

### Key idea
Refaster templates are **plain, compilable Java classes** — the pattern is real Java code, type-checked by javac, so Refaster gets type resolution "for free" (no custom type-constraint syntax needed, unlike SSR's Type filter or PMD's XPath type predicates).

### Verbatim example
```java
public class StringIsEmpty {
  @BeforeTemplate
  boolean equalsEmptyString(String string) {
    return string.equals("");
  }

  @BeforeTemplate
  boolean lengthEquals0(String string) {
    return string.length() == 0;
  }

  @AfterTemplate
  boolean optimizedMethod(String string) {
    return string.isEmpty();
  }
}
```
Doc description: "Refaster templates are any class with multiple methods with the same return type and list of arguments with the same name." One method is `@AfterTemplate`; every other method is `@BeforeTemplate`. Any code matching one of the `@BeforeTemplate` bodies — however that expression is constructed/chained, e.g. `someChained().methodCall().returningAString().length() == 0` — gets rewritten to the `@AfterTemplate` form.

### Metavariables
There is no `$x$`-style placeholder syntax at all — **the template method's own parameters ARE the metavariables** (`String string` above binds to "any expression of type String"). This is the other half of "it's just Java": generic-typed parameters give you generic-typed matching for free via the compiler.

### `@Placeholder`
Marks an abstract method representing "some function in terms of the specified input" — i.e. lets a template capture an arbitrary sub-expression/block as a hole, not just a single terminal parameter. Constraint quoted: "The code matched by the placeholder method **cannot** refer to variables in the `@BeforeTemplate` that are not explicitly passed in." Related annotations: `@MayOptionallyUse` (lets the after-template optionally use an argument) and `allowsIdentity = true` (permits identity/no-op matches for placeholder arguments). Some support noted for "block versus expression lambdas" bracket adjustment when placeholders are used in lambda bodies.

### Limits
Primarily expression-level matching (the whole mechanism is "a Java method body is a pattern"); multiple `@BeforeTemplate`s are supported per class (as shown above) so several syntactic variants can map to one canonical `@AfterTemplate`. `@Placeholder` is the escape hatch for statement/block-shaped holes beyond single expressions.

---

## 4. PMD XPath rules (Apex-relevant)

Sources: https://pmd.github.io/pmd/pmd_userdocs_extending_writing_xpath_rules.html , and live rule source at https://github.com/pmd/pmd/blob/main/pmd-apex/src/main/resources/category/apex/bestpractices.xml (fetched, verbatim XML — the strongest primary-source evidence gathered in this research)

### Verbatim real Apex XPath rules (from PMD's own shipped ruleset, not a summary)
```xml
<rule name="ApexUnitTestMethodShouldHaveIsTestAnnotation"
    since="6.13.0"
    language="apex"
    message="Apex test methods should have @isTest annotation."
    class="net.sourceforge.pmd.lang.rule.xpath.XPathRule"
    externalInfoUrl="${pmd.website.baseurl}/pmd_rules_apex_bestpractices.html#apexunittestmethodshouldhaveistestannotation">
    <description>
        Apex test methods should have `@isTest` annotation instead of the `testMethod` keyword,
        as `testMethod` is deprecated.
    </description>
    <priority>3</priority>
    <properties>
        <property name="xpath">
            <value>
                <![CDATA[
                //Method[ModifierNode[@DeprecatedTestMethod = true()]]
                ]]>
            </value>
        </property>
    </properties>
</rule>
```
```xml
<rule name="AvoidFutureAnnotation"
    since="7.19.0"
    language="apex"
    message="Usage of @Future annotation should be limited. Consider implementing the Queueable interface instead."
    class="net.sourceforge.pmd.lang.rule.xpath.XPathRule"
    externalInfoUrl="${pmd.website.baseurl}/pmd_rules_apex_bestpractices.html#avoidfutureannotation">
    <description>
        Usage of the `@Future` annotation should be limited for asynchronous execution.
    </description>
    <priority>4</priority>
    <properties>
        <property name="xpath">
            <value>
                <![CDATA[
                //Method/ModifierNode/Annotation[lower-case(@Name) = 'future']
                ]]>
            </value>
        </property>
    </properties>
</rule>
```
Also referenced (from a secondary blog source, lower confidence, not independently re-verified verbatim): an Apex rule combining `//UserClass[not(ends-with(@Image, 'Accessor'))]/Method/ModifierNode[@Test=false()]/..//(SoqlExpression | MethodCallExpression[lower-case(@FullMethodName)='database.query'])`.

### Apex AST node names surfaced in real rules
`Method`, `ModifierNode`, `Annotation`, `UserClass`, `SoqlExpression`, `MethodCallExpression` — attribute access via `@Name`, `@DeprecatedTestMethod`, `@FullMethodName`. This is directly relevant since apexls works with Apex; PMD's node-naming convention drops the `AST` prefix in XPath queries even though the Java API classes are named `ASTMethod`, `ASTUserClass`, etc. (XPath axis names are the un-prefixed short names).

### General XPath rule mechanics (from the writing-xpath-rules doc)
- `//*[pmd-java:nodeIs("Expression")]` — match by node-kind predicate function
- `//MethodDeclaration[pmd-java:hasAnnotation("java.lang.Override")]` — annotation-presence predicate
- `//MethodCall[pmd-java:matchesSig("_#equals(java.lang.Object)")]` — signature-matching predicate
- `//b[pmd:fileName() = 'Foo.xml']`, `//b[pmd:endLine(.) == pmd:startLine(.)]` — cross-cutting utility functions available in any language's XPath rules
- PMD 7 uses XPath 3.1; the fetched page confirms these language-specific function extensions exist but the XPath-version claim itself was not re-derived from a fresh fetch in this pass — treat as background knowledge, not independently re-verified here.

### No rewrite side — confirmed
The fetched documentation page contains **no mention of automated fixes or rewrite output** for XPath rules — it is exclusively about writing detection queries. PMD XPath rules are match/report only; there is no replace/fix template mechanism analogous to SSR's replace side or Refaster's `@AfterTemplate`.

---

## 5. rslint / other Rust-based ones — brief

`rslint/rslint` (github.com/rslint/rslint) — "A (WIP) Extremely fast JavaScript and TypeScript linter and Rust crate," explicitly early/WIP, not confirmed archived per repo activity signals found, but not confirmed as still actively maintained either — status uncertain, not dead-and-archived (unlike gogrep, which explicitly says so). No structural-search/pattern-DSL syntax specific to rslint was found in the search results (its lint rules appear to be a built-in fixed rule set rather than a user-facing SSR-style pattern language) — nothing distinctive enough to report beyond that.

---

## Sources
- https://www.jetbrains.com/help/idea/structural-search-and-replace.html
- https://www.jetbrains.com/help/idea/search-templates.html
- https://www.jetbrains.com/help/idea/structural-search-and-replace-examples.html
- https://www.jetbrains.com/help/idea/tutorial-work-with-structural-search-and-replace.html
- https://plugins.jetbrains.com/docs/intellij/psi.html
- https://github.com/JetBrains/intellij-community/blob/master/platform/structuralsearch/source/com/intellij/structuralsearch/Matcher.java
- https://raw.githubusercontent.com/mvdan/gogrep/master/README.md
- https://github.com/mvdan/gogrep/issues/64
- https://github.com/quasilyte/gogrep
- https://errorprone.info/docs/refaster
- https://pmd.github.io/pmd/pmd_userdocs_extending_writing_xpath_rules.html
- https://github.com/pmd/pmd/blob/main/pmd-apex/src/main/resources/category/apex/bestpractices.xml
- https://github.com/rslint/rslint

## Caveats for the design doc author
- WebFetch in this research pass renders pages through a summarizing model rather than returning raw HTML, so most "verbatim" JetBrains quotes above were cross-checked across 2+ fetches/searches for consistency, but a couple of items (the "contained in constructor" / "read/write access" / "formal argument type" filter names from the original task prompt) could NOT be confirmed on the current live docs — the current docs enumerate exactly five modifiers (Count, Text, Type, Reference, Script). Worth a manual double-check in the actual IDE UI before citing those three names in the design doc.
- The Apex PMD XML rule blocks ARE genuine verbatim fetches of PMD's real shipped ruleset source (not paraphrased), which is the strongest primary-source evidence in this report.
