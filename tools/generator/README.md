# Grammar-based generator

Generates syntactically-valid-by-construction Apex source from the grammar
rules, for property-based/differential testing (feeds generated programs to
apexls and to the oracle parsers; both should accept, and results should
agree). Complements the mutation-based fuzzers in `fuzz/`, which stress
robustness on invalid/garbage input instead.
