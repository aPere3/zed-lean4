; Highlight queries for the Lean 4 tree-sitter grammar
; (https://github.com/Julian/tree-sitter-lean, 0.2.x node vocabulary).
;
; Zed resolves overlapping captures with "last pattern wins", so general
; patterns come first and specific ones later. Capture names follow Zed's
; theme syntax keys; dotted names fall back to their prefix.
;
; With `"semantic_tokens": "combined"`, the Lean server's tokens (keyword,
; variable, property, function, leanSorryLike) are layered on top of these.

; ---------- comments ------------------------------------------------------

(line_comment)        @comment
(block_comment)       @comment
(doc_comment)         @comment.doc
(module_doc_comment)  @comment.doc

; ---------- literals ------------------------------------------------------

(num_lit)             @number
(scientific_lit)      @number.float
(char_lit)            @character
(str_lit)             @string
(raw_string)          @string.special
(interpolated_str)    @string
(escape_sequence)     @string.escape
(name_lit)            @string.special.symbol

(interpolation
  "{" @punctuation.special
  "}" @punctuation.special)

(true_const)          @boolean
(false_const)         @boolean

(bot_const)           @constant.builtin
(top_const)           @constant.builtin
(empty_const)         @constant.builtin

(type_const)          @type.builtin
(sort_const)          @type.builtin
(prop_const)          @type.builtin

; ---------- identifiers ---------------------------------------------------

; Capitalised single-component names are almost always types, structures
; or namespaces (`Nat`, `Finset`, `Integrable`). Blackboard letters too.
((identifier) @type
  (#match? @type "^([A-Z][A-Za-z0-9_'!?]*|ℕ|ℤ|ℚ|ℝ|ℂ)$"))

((identifier) @variable.builtin
  (#eq? @variable.builtin "this"))

(hole)                @variable.builtin
(synth_hole)          @variable.builtin
(named_hole)          @variable.builtin
(cdot)                @punctuation.special
(goal_const)          @keyword.operator

; `.ctor` anonymous-constructor notation.
(dot_ident name: (identifier) @constructor)

; Binder names.
(binders (identifier) @variable.parameter)
(binders (hole) @variable.parameter)
(explicit_binder        name: (identifier) @variable.parameter)
(implicit_binder        name: (identifier) @variable.parameter)
(strict_implicit_binder name: (identifier) @variable.parameter)
(instance_binder        name: (identifier) @variable.parameter)
(tuple_binder (identifier) @variable.parameter)
(anon_ctor_binder (identifier) @variable.parameter)

; Local bindings.
(let      name: (identifier) @variable)
(have     name: (identifier) @variable)
(suffices name: (identifier) @variable)
(by_cases name: (identifier) @variable)
(block_assign name: (identifier) @variable)
(set_builder binder: (identifier) @variable.parameter)
(subtype_lit binder: (identifier) @variable.parameter)

; Named arguments and structure-literal fields.
(paren arg_name: (identifier) @variable.parameter)
(struct_field name: (identifier) @property)

; Projection.
(proj field: (identifier) @property)
(proj field: (num_lit)    @number)

; Function application head.
(app fn: (identifier) @function.call)

; ---------- declarations --------------------------------------------------

(def       name: (identifier) @function)
(theorem   name: (identifier) @function)
(abbrev    name: (identifier) @function)
(opaque    name: (identifier) @function)
(constant  name: (identifier) @function)
(axiom     name: (identifier) @function)
(instance  name: (identifier) @function)
(where_aux_def name: (identifier) @function)

(structure name: (identifier) @type)
(inductive name: (identifier) @type)

(ctor      name: (identifier) @constructor)
(ctor_alt  name: (identifier) @constructor)

(field     name: (identifier) @property)

(namespace name: (identifier) @namespace)
(section   name: (identifier) @namespace)
(end       name: (identifier) @namespace)

(universe  name: (identifier) @type)

(import    name: (identifier) @module)
(open      namespace: (identifier) @module)
(open      only: (identifier)      @function)
(open      scoped: (identifier)    @module)
(export    namespace: (identifier) @module)
(export    only: (identifier)      @function)

(attributes name: (identifier) @attribute)
(attribute_cmd target: (identifier) @function)
(deriving_cmd class: (identifier) @type)
(deriving_cmd target: (identifier) @type)

(tactic_case name: (identifier) @label)
(induction elim: (identifier) @function)
(set_option name: (identifier) @property)
(alias_cmd name: (identifier) @function)

; ---------- tactics -------------------------------------------------------

; Tactics are ordinary identifiers to the grammar. Highlight the common
; ones by name, as the VSCode extension does, when they sit in tactic
; position: a `by` statement, an application head, or (for `·` bullets
; and `<;>`) an application argument / right operand.
(by (identifier) @keyword
  (#match? @keyword "^(intro|intros|rintro|exact|exacts|apply|refine|refine'|rw|rwa|rewrite|erw|simp|simp_all|simp_rw|simpa|dsimp|norm_num|norm_num1|ring|ring_nf|ring1|linarith|nlinarith|positivity|omega|decide|aesop|tauto|trivial|rfl|constructor|use|obtain|rcases|cases'|induction'|ext|funext|congr|gcongr|congrm|calc|unfold|delta|change|specialize|generalize|subst|symm|trans|exfalso|contradiction|absurd|by_contra|by_contra!|push_neg|split|split_ifs|left|right|assumption|exact\\?|apply\\?|simp\\?|rw\\?|norm_cast|push_cast|exact_mod_cast|field_simp|convert|convert!|convert_to|filter_upwards|measurability|continuity|fun_prop|infer_instance|inferInstance|choose|lift|zify|qify|nth_rw|nth_rewrite|rename_i|clear|revert|repeat|first|try|all_goals|any_goals|focus|next|show_term|done|skip|classical|borelize|bound|abel|group|noncomm_ring|linear_combination|polyrith|interval_cases|fin_cases|mod_cases|decreasing_tactic|conv|conv_lhs|conv_rhs|enter|slice_lhs|slice_rhs|apply_fun|peel|match_scalars|nomatch|nofun|ac_rfl|and_intros|injection|contrapose|contrapose!|wlog|swap|rotate_left|rotate_right|pick_goal|on_goal|rename|replace|choose!|simp_arith|bv_decide|grind|solve_by_elim|hint|intro!|guard_hyp|guard_target|refold_let|nlinarith!|linarith!|set!|tfae_have|tfae_finish|rsuffices|only|at|using|with|generalizing|says)$"))

(app fn: (identifier) @keyword
  (#match? @keyword "^(intro|intros|rintro|exact|exacts|apply|refine|refine'|rw|rwa|rewrite|erw|simp|simp_all|simp_rw|simpa|dsimp|norm_num|norm_num1|ring|ring_nf|ring1|linarith|nlinarith|positivity|omega|decide|aesop|tauto|trivial|rfl|constructor|use|obtain|rcases|cases'|induction'|ext|funext|congr|gcongr|congrm|calc|unfold|delta|change|specialize|generalize|subst|symm|trans|exfalso|contradiction|absurd|by_contra|by_contra!|push_neg|split|split_ifs|left|right|assumption|exact\\?|apply\\?|simp\\?|rw\\?|norm_cast|push_cast|exact_mod_cast|field_simp|convert|convert!|convert_to|filter_upwards|measurability|continuity|fun_prop|infer_instance|inferInstance|choose|lift|zify|qify|nth_rw|nth_rewrite|rename_i|clear|revert|repeat|first|try|all_goals|any_goals|focus|next|show_term|done|skip|classical|borelize|bound|abel|group|noncomm_ring|linear_combination|polyrith|interval_cases|fin_cases|mod_cases|decreasing_tactic|conv|conv_lhs|conv_rhs|enter|slice_lhs|slice_rhs|apply_fun|peel|match_scalars|nomatch|nofun|ac_rfl|and_intros|injection|contrapose|contrapose!|wlog|swap|rotate_left|rotate_right|pick_goal|on_goal|rename|replace|choose!|simp_arith|bv_decide|grind|solve_by_elim|hint|intro!|guard_hyp|guard_target|refold_let|nlinarith!|linarith!|set!|tfae_have|tfae_finish|rsuffices|says)$"))

(app arg: (identifier) @keyword
  (#match? @keyword "^(intro|intros|rintro|exact|exacts|apply|refine|refine'|rw|rwa|rewrite|erw|simp|simp_all|simp_rw|simpa|dsimp|norm_num|norm_num1|ring|ring_nf|ring1|linarith|nlinarith|positivity|omega|decide|aesop|tauto|trivial|rfl|constructor|use|obtain|rcases|cases'|induction'|ext|funext|congr|gcongr|congrm|unfold|delta|change|specialize|generalize|subst|symm|trans|exfalso|contradiction|by_contra|by_contra!|push_neg|split|split_ifs|assumption|exact\\?|apply\\?|simp\\?|rw\\?|norm_cast|push_cast|exact_mod_cast|field_simp|convert|convert!|convert_to|filter_upwards|measurability|continuity|fun_prop|infer_instance|zify|qify|nth_rw|nth_rewrite|rename_i|clear|revert|repeat|first|try|all_goals|any_goals|focus|next|show_term|done|skip|classical|borelize|bound|abel|group|noncomm_ring|linear_combination|polyrith|interval_cases|fin_cases|mod_cases|decreasing_tactic|conv|conv_lhs|conv_rhs|enter|apply_fun|peel|nomatch|nofun|ac_rfl|and_intros|contrapose|contrapose!|wlog|swap|rotate_left|rotate_right|pick_goal|on_goal|rename|replace|simp_arith|bv_decide|grind|solve_by_elim|intro!|guard_hyp|guard_target|nlinarith!|linarith!|tfae_finish|only|at|using|with|generalizing|says)$"))

(binary_op rhs: (identifier) @keyword
  (#match? @keyword "^(intro|intros|rintro|exact|apply|refine|rw|rwa|erw|simp|simp_all|simpa|dsimp|norm_num|ring|ring_nf|linarith|nlinarith|positivity|omega|decide|aesop|tauto|trivial|rfl|constructor|ext|funext|congr|gcongr|assumption|norm_cast|push_cast|field_simp|fun_prop|infer_instance|skip|done|first|try|all_goals|any_goals|grind)$"))

; ---------- keywords ------------------------------------------------------

(moduleTk)        @keyword.import
(prelude)         @keyword.import
(public)          @keyword.import
(meta)            @keyword.import
(all)             @keyword.import
(public_section)  @keyword.import
"import"          @keyword.import

(mutual) @keyword

[
  "namespace"
  "section"
  "end"
] @keyword

[
  "def"
  "theorem"
  "lemma"
  "example"
  "abbrev"
  "instance"
  "axiom"
  "opaque"
  "constant"
  "structure"
  "inductive"
  "class"
] @keyword

[
  "variable"
  "universe"
  "universes"
  "open"
  "export"
  "extends"
  "deriving"
  "where"
  "attribute"
  "set_option"
  "initialize"
  "builtin_initialize"
  "alias"
  "add_decl_doc"
  "library_note"
  "recommended_spelling"
  "initialize_simps_projections"
  "assert_not_exists"
  "assert_not_imported"
  "assert_exists"
  "check_assertions"
  "grind_pattern"
  "unif_hint"
  "register_builtin_option"
  "register_option"
  "register_error_explanation"
  "declare_syntax_cat"
  "to_dual_insert_cast"
  "to_dual_insert_cast_fun"
] @keyword

[
  "notation"
  "infix"
  "infixl"
  "infixr"
  "prefix"
  "postfix"
  "syntax"
  "macro"
  "macro_rules"
  "elab_rules"
] @keyword

[
  "noncomputable"
  "partial"
  "nonrec"
  "private"
  "protected"
  "unsafe"
  "mut"
  "rec"
  "scoped"
  "local"
] @keyword.modifier

[
  "#check"
  "#check_failure"
  "#eval"
  "#print"
  "#print_axioms"
  "#reduce"
  "#exit"
  "#synth"
  "#version"
] @keyword.directive

[
  "fun"
  "λ"
] @keyword.function

[
  "let"
  "letI"
  "have"
  "haveI"
  "obtain"
  "set"
  "show"
  "from"
  "suffices"
  "by_cases"
  "induction"
  "cases"
  "using"
  "generalizing"
  "case"
  "in"
  "for"
] @keyword

[
  "if"
  "then"
  "else"
] @keyword.conditional

[
  "match"
  "with"
] @keyword.conditional

[
  "by"
  "do"
] @keyword

[
  "forall"
  "∀"
  "exists"
  "∃"
  "Π"
  "Σ"
  "Σ'"
] @keyword.operator

; Big-operator binders (∑ x ∈ s, f x).
[
  "⨆" "⨅" "∑" "∏"
  "⋃" "⋂" "⋀" "⋁" "⨁" "⨂" "∐"
  "∀ᵐ" "∃ᵐ"
  "∫" "∫⁻" "⨍"
] @keyword.operator

; ---------- operators -----------------------------------------------------

(binary_op  op: _ @operator)
(unary_op   op: _ @operator)
(postfix_op op: _ @operator)

[
  "→" "->" "↔" "<->" "↦" "=>"
  "←" "<-"
  ":=" "::"
  "@" "$"
  "↑" "↓" "↥" "⇑"
  "∂" "•"
] @operator

; ---------- punctuation ---------------------------------------------------

[":" "," ";" "|" "."] @punctuation.delimiter

[
  "(" ")" "{" "}" "[" "]"
  "⟨" "⟩" "⦃" "⦄" "{{" "}}"
  "⟦" "⟧" "‹" "›"
  "⌊" "⌋" "⌋₊" "⌈" "⌉" "⌉₊"
  "#["
] @punctuation.bracket

(attributes "@[" @attribute "]" @attribute)

; ---------- raw atoms -----------------------------------------------------

["`" "``"] @string.special.symbol
["s!\"" "m!\""] @string

; `sorry` / `admit`: the Lean server tags these leanSorryLike.
(sorry) @keyword.exception
